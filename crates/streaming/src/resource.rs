//! Global resource controller for the stream pipeline.
//!
//! Uses a [`tokio::sync::Semaphore`] to bound the number of concurrently
//! active streams, preventing resource exhaustion on resource-constrained
//! laptops.
//!
//! # Example
//!
//! ```ignore
//! let ctrl = ResourceController::new(4); // max 4 concurrent streams
//! let permit = ctrl.acquire().await?;     // blocks until a slot is free
//! // ... use the stream ...
//! drop(permit);                           // releases the slot
//! ```

use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use tokio::sync::watch;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;
use uuid::Uuid;

/// Controls the maximum number of concurrent streams.
///
/// Wraps a [`tokio::sync::Semaphore`]. Each concurrent stream acquires a
/// [`StreamPermit`] at start and releases it on drop or explicit release.
#[derive(Clone, Debug)]
pub struct ResourceController {
    /// Maximum number of concurrent streams.
    max_streams: usize,
    /// Semaphore tracking available stream slots.
    semaphore: Arc<Semaphore>,
}

impl ResourceController {
    /// Create a new controller allowing up to `max_streams` concurrent streams.
    ///
    /// # Panics
    ///
    /// Panics if `max_streams` is 0.
    pub fn new(max_streams: usize) -> Self {
        assert!(
            max_streams > 0,
            "ResourceController requires at least 1 concurrent stream slot"
        );
        Self {
            max_streams,
            semaphore: Arc::new(Semaphore::new(max_streams)),
        }
    }

    /// Acquire a permit for a new stream.
    ///
    /// This will block asynchronously until a slot is available.
    /// Returns [`Err`] if the semaphore has been closed.
    pub async fn acquire(&self) -> Result<StreamPermit> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("ResourceController semaphore closed"))?;
        Ok(StreamPermit { _permit: permit })
    }

    /// Try to acquire a permit without blocking.
    ///
    /// Returns `None` immediately if all slots are taken.
    pub fn try_acquire(&self) -> Option<StreamPermit> {
        self.semaphore
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| StreamPermit { _permit: permit })
    }

    /// Returns the maximum number of concurrent streams.
    pub fn max_streams(&self) -> usize {
        self.max_streams
    }

    /// Returns the number of available (unused) slots.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

/// A permit that represents one active stream slot.
///
/// The slot is released when this permit is dropped.
#[derive(Debug)]
pub struct StreamPermit {
    _permit: OwnedSemaphorePermit,
}

impl StreamPermit {
    /// Explicitly release the permit, freeing the stream slot.
    pub fn release(self) {
        // Dropping the OwnedSemaphorePermit releases it.
        drop(self);
    }
}

// ── Per-Stream Memory Budget ─────────────────────────────────────────────────────

/// Default per-stream memory budget (10 MB).
pub const DEFAULT_MAX_BUDGET_PER_STREAM: usize = 10 * 1024 * 1024;

/// Per-stream memory budget tracker.
///
/// Tracks how many bytes each stream has allocated and enforces a
/// per-stream limit (default 10 MB). Used to prevent any single stream
/// from consuming excessive memory.
///
/// # Example
///
/// ```ignore
/// let budget = StreamBudget::new();
/// budget.try_alloc(my_stream_id, 65536)?; // allocate 64 KB
/// // ... use the buffer ...
/// budget.release(my_stream_id, 65536);    // free it
/// ```
#[derive(Clone, Debug)]
pub struct StreamBudget {
    inner: Arc<Mutex<HashMap<Uuid, usize>>>,
    max_per_stream: usize,
}

impl StreamBudget {
    /// Create a new budget tracker with the default per-stream limit (10 MB).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_per_stream: DEFAULT_MAX_BUDGET_PER_STREAM,
        }
    }

    /// Create a new budget tracker with a custom per-stream limit.
    pub fn with_max_per_stream(max_per_stream: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_per_stream,
        }
    }

    /// Try to reserve `bytes` for the given stream.
    ///
    /// Returns an error if the stream would exceed the per-stream budget.
    pub fn try_alloc(&self, stream_id: Uuid, bytes: usize) -> Result<()> {
        let mut inner = self.inner.lock();
        let current = inner.entry(stream_id).or_insert(0);
        let new_total = current.saturating_add(bytes);
        if new_total > self.max_per_stream {
            warn!(
                "Stream {stream_id} budget exceeded: {new_total} > {} bytes",
                self.max_per_stream
            );
            return Err(anyhow::anyhow!(
                "503: Per-stream memory budget exceeded for stream {stream_id}"
            ));
        }
        *current = new_total;
        Ok(())
    }

    /// Release `bytes` for the given stream.
    ///
    /// If the stream's allocated count drops to zero, the entry is removed.
    pub fn release(&self, stream_id: Uuid, bytes: usize) {
        let mut inner = self.inner.lock();
        if let Some(current) = inner.get_mut(&stream_id) {
            *current = current.saturating_sub(bytes);
            if *current == 0 {
                inner.remove(&stream_id);
            }
        }
    }

    /// Returns the number of bytes currently allocated for a stream.
    pub fn allocated(&self, stream_id: Uuid) -> usize {
        self.inner.lock().get(&stream_id).copied().unwrap_or(0)
    }

    /// Remove a stream from the tracker, releasing all of its budget.
    pub fn remove_stream(&self, stream_id: Uuid) {
        self.inner.lock().remove(&stream_id);
    }

    /// Number of tracked streams.
    pub fn stream_count(&self) -> usize {
        self.inner.lock().len()
    }

    /// Total bytes allocated across all streams.
    pub fn total_allocated(&self) -> usize {
        self.inner.lock().values().sum()
    }

    /// The per-stream budget limit in bytes.
    pub fn max_per_stream(&self) -> usize {
        self.max_per_stream
    }

    /// Returns a list of `(stream_id, allocated_bytes)` for all tracked streams.
    pub fn stream_budgets(&self) -> Vec<(Uuid, usize)> {
        self.inner.lock().iter().map(|(k, v)| (*k, *v)).collect()
    }
}

impl Default for StreamBudget {
    fn default() -> Self {
        Self::new()
    }
}

// ── Stream Lifecycle ──────────────────────────────────────────────────────────

/// The state of a managed stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamState {
    /// Stream is being set up.
    Starting,
    /// Stream is actively running.
    Running,
    /// Stream is being torn down gracefully.
    Stopping,
    /// Stream has stopped.
    Stopped,
    /// Stream encountered an error.
    Error,
}

impl StreamState {
    /// Returns `true` if transitioning to `next` is valid.
    pub fn can_transition_to(self, next: StreamState) -> bool {
        use StreamState::*;
        match self {
            Starting => matches!(next, Running | Error),
            Running => matches!(next, Stopping | Error),
            Stopping => matches!(next, Stopped | Error),
            Stopped => matches!(next, Starting),
            Error => matches!(next, Starting),
        }
    }
}

/// A handle to a managed stream, providing state observation.
///
/// The handle contains a `watch::Receiver` that produces updates whenever
/// the stream's state changes.
#[derive(Debug, Clone)]
pub struct StreamHandle {
    stream_id: Uuid,
    state_rx: watch::Receiver<StreamState>,
}

impl StreamHandle {
    /// The stream's unique identifier.
    pub fn stream_id(&self) -> Uuid {
        self.stream_id
    }

    /// The current state of the stream.
    pub fn state(&self) -> StreamState {
        *self.state_rx.borrow()
    }

    /// Wait asynchronously until the stream reaches the given state.
    pub async fn wait_for_state(&mut self, target: StreamState) -> Result<()> {
        loop {
            if *self.state_rx.borrow() == target {
                return Ok(());
            }
            self.state_rx
                .changed()
                .await
                .map_err(|_| anyhow::anyhow!("Stream {} state watch closed", self.stream_id))?;
        }
    }

    /// Subscribe to state changes (cloning the receiver).
    pub fn subscribe(&self) -> watch::Receiver<StreamState> {
        self.state_rx.clone()
    }
}

/// Manages the lifecycle of streams.
///
/// Each stream is identified by a [`Uuid`] and transitions through
/// [`StreamState`] variants: `Starting -> Running -> Stopping -> Stopped`,
/// with `Error` as a possible terminal state. Streams in `Stopped` or
/// `Error` can be restarted.
///
/// # Example
///
/// ```ignore
/// let lifecycle = StreamLifecycle::new();
/// let id = Uuid::new_v4();
/// let handle = lifecycle.start(id);
/// assert_eq!(handle.state(), StreamState::Starting);
///
/// lifecycle.transition(id, StreamState::Running)?;
/// assert_eq!(handle.state(), StreamState::Running);
///
/// lifecycle.stop(id)?;
/// assert_eq!(handle.state(), StreamState::Stopped);
/// ```
type StreamEntries = HashMap<Uuid, (StreamState, watch::Sender<StreamState>)>;

#[derive(Clone, Debug)]
pub struct StreamLifecycle {
    inner: Arc<Mutex<StreamEntries>>,
}

impl StreamLifecycle {
    /// Create a new empty lifecycle manager.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new stream in the `Starting` state and return a handle.
    pub fn start(&self, stream_id: Uuid) -> StreamHandle {
        let (tx, rx) = watch::channel(StreamState::Starting);
        self.inner
            .lock()
            .insert(stream_id, (StreamState::Starting, tx));
        StreamHandle {
            stream_id,
            state_rx: rx,
        }
    }

    /// Transition a stream to a new state.
    ///
    /// Returns an error if the transition is invalid or the stream is unknown.
    pub fn transition(&self, stream_id: Uuid, state: StreamState) -> Result<()> {
        let mut inner = self.inner.lock();
        let entry = inner
            .get_mut(&stream_id)
            .ok_or_else(|| anyhow::anyhow!("Stream {stream_id} not found in lifecycle"))?;
        if entry.0.can_transition_to(state) {
            entry.0 = state;
            let _ = entry.1.send(state);
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Invalid transition for stream {stream_id}: {:?} -> {:?}",
                entry.0,
                state
            ))
        }
    }

    /// Stop a stream gracefully (transition through `Stopping` -> `Stopped`).
    ///
    /// Only valid for streams in `Starting` or `Running` state.
    pub fn stop(&self, stream_id: Uuid) -> Result<()> {
        let state = self
            .state(stream_id)
            .ok_or_else(|| anyhow::anyhow!("Stream {stream_id} not found in lifecycle"))?;
        match state {
            StreamState::Starting | StreamState::Running => {
                self.transition(stream_id, StreamState::Stopping)?;
                self.transition(stream_id, StreamState::Stopped)?;
                Ok(())
            }
            StreamState::Stopped => Err(anyhow::anyhow!("Stream {stream_id} is already stopped")),
            StreamState::Error => Err(anyhow::anyhow!(
                "Stream {stream_id} is in error state; use restart()"
            )),
            StreamState::Stopping => Err(anyhow::anyhow!("Stream {stream_id} is already stopping")),
        }
    }

    /// Restart a stream from `Stopped` or `Error` back to `Starting`.
    pub fn restart(&self, stream_id: Uuid) -> Result<()> {
        let state = self
            .state(stream_id)
            .ok_or_else(|| anyhow::anyhow!("Stream {stream_id} not found in lifecycle"))?;
        match state {
            StreamState::Stopped | StreamState::Error => {
                self.transition(stream_id, StreamState::Starting)
            }
            _ => Err(anyhow::anyhow!(
                "Stream {stream_id} is in {:?} state; cannot restart",
                state
            )),
        }
    }

    /// Get the current state of a stream.
    pub fn state(&self, stream_id: Uuid) -> Option<StreamState> {
        self.inner.lock().get(&stream_id).map(|(s, _)| *s)
    }

    /// Remove a stream from the lifecycle manager entirely.
    pub fn remove(&self, stream_id: Uuid) {
        self.inner.lock().remove(&stream_id);
    }

    /// Subscribe to state changes for a stream.
    pub fn subscribe(&self, stream_id: Uuid) -> Option<watch::Receiver<StreamState>> {
        self.inner
            .lock()
            .get(&stream_id)
            .map(|(_, tx)| tx.subscribe())
    }

    /// Number of streams currently in `Starting` or `Running` state.
    pub fn active_count(&self) -> usize {
        self.inner
            .lock()
            .values()
            .filter(|(s, _)| matches!(s, StreamState::Starting | StreamState::Running))
            .count()
    }

    /// Total number of tracked streams (in any state).
    pub fn stream_count(&self) -> usize {
        self.inner.lock().len()
    }
}

impl Default for StreamLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

// ── Memory Profiling ──────────────────────────────────────────────────────────

/// Read the current RSS (resident set size) from `/proc/self/status`.
///
/// Returns 0 if the file cannot be read or parsed (e.g., on non-Linux
/// platforms or in containers without /proc).
pub fn measure_rss() -> usize {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            // Format: "VmRSS:   12345 kB"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2
                && let Ok(kb) = parts[1].parse::<usize>()
            {
                return kb * 1024;
            }
        }
    }
    0
}

/// A snapshot of memory usage at a point in time.
#[derive(Debug, Clone)]
pub struct MemorySnapshot {
    /// Resident set size of the process (bytes).
    pub rss_bytes: usize,
    /// Memory currently held in the buffer pool (bytes).
    pub buffer_pool_capacity: usize,
    /// Number of buffers in the pool.
    pub buffer_pool_buffers: usize,
    /// Number of active streams.
    pub active_streams: usize,
    /// Maximum allowed streams.
    pub max_streams: usize,
    /// Per-stream budget allocations.
    pub per_stream_budgets: Vec<(Uuid, usize)>,
}

/// Gathers memory usage snapshots from the stream pipeline.
pub struct MemoryProfiler;

impl MemoryProfiler {
    /// Capture a snapshot of memory usage.
    ///
    /// Collects RSS, buffer pool stats, stream count, and per-stream budgets.
    pub fn snapshot(
        resource: &ResourceController,
        budget: &StreamBudget,
        buffer_pool: &crate::buffer::BufferPool,
    ) -> MemorySnapshot {
        let (pool_bufs, pool_cap) = buffer_pool.usage();
        MemorySnapshot {
            rss_bytes: measure_rss(),
            buffer_pool_capacity: pool_cap,
            buffer_pool_buffers: pool_bufs,
            active_streams: resource.max_streams() - resource.available_permits(),
            max_streams: resource.max_streams(),
            per_stream_budgets: budget.stream_budgets(),
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acquire_and_release() {
        let ctrl = ResourceController::new(2);
        assert_eq!(ctrl.available_permits(), 2);

        let p1 = ctrl.acquire().await.unwrap();
        assert_eq!(ctrl.available_permits(), 1);

        let p2 = ctrl.acquire().await.unwrap();
        assert_eq!(ctrl.available_permits(), 0);

        // Release one
        drop(p1);
        assert_eq!(ctrl.available_permits(), 1);

        // Release the other
        drop(p2);
        assert_eq!(ctrl.available_permits(), 2);
    }

    #[tokio::test]
    async fn test_acquire_blocked_then_released() {
        let ctrl = ResourceController::new(1);
        let p1 = ctrl.acquire().await.unwrap();
        assert_eq!(ctrl.available_permits(), 0);

        // Try to acquire another — should fail immediately with try_acquire
        assert!(ctrl.try_acquire().is_none());

        // Release
        drop(p1);

        // Now it should succeed
        let p2 = ctrl.try_acquire();
        assert!(p2.is_some());
    }

    #[tokio::test]
    async fn test_max_streams_within_limit() {
        let ctrl = ResourceController::new(3);
        let p1 = ctrl.acquire().await.unwrap();
        let p2 = ctrl.acquire().await.unwrap();
        let p3 = ctrl.acquire().await.unwrap();
        assert_eq!(ctrl.available_permits(), 0);
        drop(p1);
        drop(p2);
        drop(p3);
    }

    #[tokio::test]
    async fn test_explicit_release() {
        let ctrl = ResourceController::new(1);
        let p = ctrl.acquire().await.unwrap();
        assert_eq!(ctrl.available_permits(), 0);
        p.release();
        assert_eq!(ctrl.available_permits(), 1);
    }

    #[test]
    #[should_panic(expected = "ResourceController requires at least 1")]
    fn test_zero_max_streams_panics() {
        let _ctrl = ResourceController::new(0);
    }

    #[tokio::test]
    async fn test_multiple_controllers_independent() {
        let ctrl1 = ResourceController::new(2);
        let ctrl2 = ResourceController::new(3);
        assert_eq!(ctrl1.available_permits(), 2);
        assert_eq!(ctrl2.available_permits(), 3);
    }

    // ── StreamBudget tests ────────────────────────────────────────────────────

    #[test]
    fn test_stream_budget_alloc_release() {
        let budget = StreamBudget::with_max_per_stream(1000);
        let sid = Uuid::new_v4();
        assert_eq!(budget.allocated(sid), 0);

        budget.try_alloc(sid, 400).unwrap();
        assert_eq!(budget.allocated(sid), 400);

        budget.try_alloc(sid, 300).unwrap();
        assert_eq!(budget.allocated(sid), 700);

        budget.release(sid, 200);
        assert_eq!(budget.allocated(sid), 500);

        budget.release(sid, 500);
        // After releasing all, entry should be removed
        assert_eq!(budget.allocated(sid), 0);
        assert_eq!(budget.stream_count(), 0);
    }

    #[test]
    fn test_stream_budget_exceed_limit() {
        let budget = StreamBudget::with_max_per_stream(500);
        let sid = Uuid::new_v4();

        budget.try_alloc(sid, 300).unwrap();
        budget.try_alloc(sid, 200).unwrap();

        // This should fail
        let err = budget.try_alloc(sid, 1).unwrap_err();
        assert!(
            err.to_string().contains("503"),
            "Expected 503 error, got: {err}"
        );
        assert_eq!(budget.allocated(sid), 500);
    }

    #[test]
    fn test_stream_budget_release_reallows() {
        let budget = StreamBudget::with_max_per_stream(500);
        let sid = Uuid::new_v4();

        budget.try_alloc(sid, 500).unwrap();
        assert!(budget.try_alloc(sid, 1).is_err());

        // Release half, should be able to allocate again
        budget.release(sid, 500);
        budget.try_alloc(sid, 300).unwrap();
        assert_eq!(budget.allocated(sid), 300);
    }

    #[test]
    fn test_stream_budget_multiple_streams() {
        let budget = StreamBudget::with_max_per_stream(1000);
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();

        budget.try_alloc(s1, 800).unwrap();
        budget.try_alloc(s2, 500).unwrap();
        assert_eq!(budget.allocated(s1), 800);
        assert_eq!(budget.allocated(s2), 500);
        assert_eq!(budget.stream_count(), 2);
        assert_eq!(budget.total_allocated(), 1300);

        budget.remove_stream(s1);
        assert_eq!(budget.stream_count(), 1);
        assert_eq!(budget.total_allocated(), 500);
    }

    #[test]
    fn test_stream_budget_default_limit() {
        let budget = StreamBudget::new();
        assert_eq!(budget.max_per_stream(), DEFAULT_MAX_BUDGET_PER_STREAM);
    }

    #[test]
    fn test_stream_budget_stream_budgets_report() {
        let budget = StreamBudget::with_max_per_stream(1000);
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();

        budget.try_alloc(s1, 100).unwrap();
        budget.try_alloc(s2, 200).unwrap();

        let report = budget.stream_budgets();
        assert_eq!(report.len(), 2);
        assert!(report.contains(&(s1, 100)));
        assert!(report.contains(&(s2, 200)));
    }

    // ── StreamLifecycle tests ─────────────────────────────────────────────────

    #[test]
    fn test_stream_lifecycle_start_and_state() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        let handle = lifecycle.start(sid);
        assert_eq!(handle.state(), StreamState::Starting);
        assert_eq!(lifecycle.active_count(), 1);
        assert_eq!(lifecycle.stream_count(), 1);

        lifecycle.transition(sid, StreamState::Running).unwrap();
        assert_eq!(handle.state(), StreamState::Running);
    }

    #[test]
    fn test_stream_lifecycle_normal_stop() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);
        lifecycle.transition(sid, StreamState::Running).unwrap();

        lifecycle.stop(sid).unwrap();
        assert_eq!(lifecycle.state(sid), Some(StreamState::Stopped));
        assert_eq!(lifecycle.active_count(), 0);
    }

    #[test]
    fn test_stream_lifecycle_invalid_transition() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);

        // Starting -> Stopped is invalid (must go through Running/Stopping)
        let err = lifecycle.transition(sid, StreamState::Stopped).unwrap_err();
        assert!(err.to_string().contains("Invalid transition"));

        // Current state should still be Starting
        assert_eq!(lifecycle.state(sid), Some(StreamState::Starting));
    }

    #[test]
    fn test_stream_lifecycle_restart_from_stopped() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);
        lifecycle.transition(sid, StreamState::Running).unwrap();
        lifecycle.stop(sid).unwrap();
        assert_eq!(lifecycle.state(sid), Some(StreamState::Stopped));

        lifecycle.restart(sid).unwrap();
        assert_eq!(lifecycle.state(sid), Some(StreamState::Starting));
    }

    #[test]
    fn test_stream_lifecycle_restart_from_error() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);
        lifecycle.transition(sid, StreamState::Running).unwrap();
        lifecycle.transition(sid, StreamState::Error).unwrap();

        lifecycle.restart(sid).unwrap();
        assert_eq!(lifecycle.state(sid), Some(StreamState::Starting));
    }

    #[test]
    fn test_stream_lifecycle_cannot_restart_running() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);
        lifecycle.transition(sid, StreamState::Running).unwrap();

        let err = lifecycle.restart(sid).unwrap_err();
        assert!(err.to_string().contains("cannot restart"));
    }

    #[test]
    fn test_stream_lifecycle_cannot_stop_already_stopped() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);
        lifecycle.transition(sid, StreamState::Running).unwrap();
        lifecycle.stop(sid).unwrap();

        let err = lifecycle.stop(sid).unwrap_err();
        assert!(err.to_string().contains("already stopped"));
    }

    #[test]
    fn test_stream_lifecycle_remove() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);
        lifecycle.remove(sid);
        assert!(lifecycle.state(sid).is_none());
        assert_eq!(lifecycle.stream_count(), 0);
    }

    #[test]
    fn test_stream_lifecycle_subscribe() {
        let lifecycle = StreamLifecycle::new();
        let sid = Uuid::new_v4();
        lifecycle.start(sid);

        let rx = lifecycle.subscribe(sid).unwrap();
        assert_eq!(*rx.borrow(), StreamState::Starting);

        lifecycle.transition(sid, StreamState::Running).unwrap();
        assert_eq!(*rx.borrow(), StreamState::Running);
    }

    // ── measure_rss tests ─────────────────────────────────────────────────────

    #[test]
    fn test_measure_rss_returns_nonzero() {
        let rss = measure_rss();
        // On Linux with /proc, RSS should be > 0
        if rss == 0 {
            // May be 0 in containers without /proc
            eprintln!("Warning: measure_rss() returned 0 (check /proc/self/status)");
        }
    }
}
