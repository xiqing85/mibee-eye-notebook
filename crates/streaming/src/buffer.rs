//! Frame buffer pool with per-stream memory budget.
//!
//! [`BufferPool`] maintains a pool of reusable `Vec<u8>` allocations.
//! Use [`BufferPool::acquire`] to obtain a [`Buffer`]; when the [`Buffer`]
//! is dropped the underlying `Vec<u8>` is returned to the pool (provided
//! the pool has not exceeded its `max_bytes` budget).

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// BufferPool
// ---------------------------------------------------------------------------

/// Shared state behind the pool.
struct PoolInner {
    /// Recycled buffers.
    buffers: Vec<Vec<u8>>,
    /// Total capacity of all buffers currently in the pool.
    total_capacity: usize,
}

/// A pool of reusable `Vec<u8>` allocations.
///
/// Each pool has a `max_bytes` budget. When a [`Buffer`] is dropped, the
/// backing `Vec<u8>` is returned to the pool only if the pool's total
/// capacity would remain at or below `max_bytes`.
///
/// # Example
///
/// ```ignore
/// let pool = BufferPool::new(10 * 1024 * 1024);  // 10 MB
/// let mut buf = pool.acquire(65536).await?;        // get a 64 KB buffer
/// buf.data().extend_from_slice(&[0u8; 1000]);
/// drop(buf);  // returned to pool
/// ```
#[derive(Clone)]
pub struct BufferPool {
    inner: Arc<Mutex<PoolInner>>,
    max_bytes: usize,
}

impl BufferPool {
    /// Create a new pool with the given `max_bytes` memory budget.
    ///
    /// The pool will not hold more than `max_bytes` of recycled buffer
    /// capacity at any time. If a returned buffer would exceed the budget,
    /// its allocation is freed instead of recycled.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                buffers: Vec::new(),
                total_capacity: 0,
            })),
            max_bytes,
        }
    }

    /// Acquire a buffer with at least `max_size` bytes of capacity.
    ///
    /// If a recycled buffer of sufficient capacity is available, it is
    /// cleared and returned. Otherwise a fresh `Vec` is allocated.
    pub async fn acquire(&self, max_size: usize) -> Result<Buffer> {
        let mut inner = self.inner.lock().await;
        // Find a buffer with enough capacity (largest first for best fit).
        if let Some(pos) = inner.buffers.iter().position(|b| b.capacity() >= max_size) {
            let mut buf = inner.buffers.swap_remove(pos);
            inner.total_capacity = inner.total_capacity.saturating_sub(buf.capacity());
            buf.clear();
            Ok(Buffer {
                pool: Some(self.inner.clone()),
                max_bytes: self.max_bytes,
                data: buf,
            })
        } else {
            Ok(Buffer {
                pool: Some(self.inner.clone()),
                max_bytes: self.max_bytes,
                data: Vec::with_capacity(max_size),
            })
        }
    }

    /// Number of buffers currently cached in the pool.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.buffers.len()
    }

    /// Check if the pool is empty.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Get a synchronous snapshot of pool usage.
    ///
    /// Returns `(number_of_buffers, total_capacity_bytes)` or `(0, 0)`
    /// if the lock is contended.
    pub fn usage(&self) -> (usize, usize) {
        match self.inner.try_lock() {
            Ok(inner) => (inner.buffers.len(), inner.total_capacity),
            Err(_) => (0, 0),
        }
    }
}

impl std::fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool")
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// A buffer obtained from a [`BufferPool`].
///
/// When dropped, the underlying storage is returned to the pool if the
/// pool's memory budget allows.
pub struct Buffer {
    pool: Option<Arc<Mutex<PoolInner>>>,
    max_bytes: usize,
    data: Vec<u8>,
}

impl Buffer {
    /// Access the underlying byte buffer.
    pub fn data(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }

    /// Consume the buffer and return the raw `Vec<u8>` without returning
    /// it to the pool.
    pub fn into_inner(mut self) -> Vec<u8> {
        // Prevent drop from returning this to the pool.
        let data = std::mem::take(&mut self.data);
        self.pool = None;
        data
    }

    /// Length of the buffer data.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl std::ops::Deref for Buffer {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        &self.data
    }
}

impl std::ops::DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("len", &self.data.len())
            .field("capacity", &self.data.capacity())
            .finish()
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if let Some(pool_arc) = self.pool.take() {
            let data = std::mem::take(&mut self.data);
            let cap = data.capacity();
            // Try to return the buffer. If the lock is contended, free instead.
            if let Ok(mut inner) = pool_arc.try_lock() {
                let would_be = inner.total_capacity.saturating_add(cap);
                if would_be <= self.max_bytes {
                    inner.total_capacity = would_be;
                    inner.buffers.push(data);
                }
                // else: silently free the allocation
            }
            // else: lock contended, free the allocation
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_buffer_pool_acquire_reuse() {
        let pool = BufferPool::new(1024 * 1024); // 1 MB budget
        assert!(pool.is_empty().await);

        {
            let mut buf = pool.acquire(100).await.unwrap();
            assert!(buf.capacity() >= 100);
            buf.data().extend_from_slice(&[0xAB; 50]);
            assert_eq!(buf.len(), 50);
        }
        // Buffer was dropped, should be back in pool.
        assert!(!pool.is_empty().await);
        let buf2 = pool.acquire(50).await.unwrap();
        assert!(buf2.capacity() >= 50);
        assert!(buf2.is_empty()); // cleared on re-acquire
    }

    #[tokio::test]
    async fn test_buffer_pool_budget_respected() {
        let pool = BufferPool::new(100); // Tiny budget: only 100 bytes

        // Acquire a 60-byte buffer
        let buf1 = pool.acquire(60).await.unwrap();
        drop(buf1);

        // Acquire another 60-byte buffer — should get a fresh one since
        // the pool's total capacity would be at 60 (within 100).
        let buf2 = pool.acquire(60).await.unwrap();
        drop(buf2);

        // Now pool should have ~120 bytes total if both were returned,
        // but the second return would exceed 100 so at most one stays.
        assert!(pool.len().await <= 1);
    }

    #[tokio::test]
    async fn test_buffer_into_inner() {
        let pool = BufferPool::new(1000);
        let mut buf = pool.acquire(100).await.unwrap();
        buf.data().extend_from_slice(&[1, 2, 3]);
        let raw = buf.into_inner();
        assert_eq!(raw, vec![1, 2, 3]);
        // Buffer was consumed, nothing returned to pool.
        assert!(pool.is_empty().await);
    }

    #[tokio::test]
    async fn test_buffer_deref() {
        let pool = BufferPool::new(1000);
        let mut buf = pool.acquire(10).await.unwrap();
        buf.extend_from_slice(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(&*buf, b"hello");
    }

    #[tokio::test]
    async fn test_pool_max_bytes_zero_never_recycles() {
        let pool = BufferPool::new(0);
        let buf = pool.acquire(100).await.unwrap();
        drop(buf);
        assert!(pool.is_empty().await);
    }

    #[tokio::test]
    async fn test_buffer_pool_multiple_buffers() {
        let pool = BufferPool::new(10_000);
        let b1 = pool.acquire(100).await.unwrap();
        let b2 = pool.acquire(200).await.unwrap();
        let b3 = pool.acquire(300).await.unwrap();
        drop(b1);
        drop(b2);
        drop(b3);
        assert_eq!(pool.len().await, 3);
    }
}
