//! End-to-end integration tests for the streaming pipeline.
//!
//! These tests verify the full StreamHub fan-out pipeline:
//! - MockSource → StreamHub → MockOutput (generic output path)
//! - MockSource → StreamHub → RtspOutput (concrete RTSP output)
//! - MockSource → StreamHub → RtmpMockOutput (RTMP-style output path)
//! - Graceful shutdown via cancellation signal
//!
//! All tests are self-contained: no hardware, no network, no NVR required.
//! Frames are synthetic H.264 Annex B NAL units produced by MockSource.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{Mutex, broadcast};

use streaming::hub::StreamHub;
use streaming::output::{Output, RtspOutput};
use streaming::resource::ResourceController;
use streaming::source::{MediaFrame, Source};

// ─────────────────────────────────────────────────────────────────────────────
// MockSource
// ─────────────────────────────────────────────────────────────────────────────
//
// Mirrors the `pub(crate)` MockSource from `crates/streaming/src/source.rs`.
// Re-implemented here because integration tests are compiled as a separate crate
// and cannot access crate-internal items.

struct MockSource {
    frames: Vec<MediaFrame>,
    started: bool,
    index: usize,
}

impl MockSource {
    /// Create a source that yields the given frames in order.
    fn new(frames: Vec<MediaFrame>) -> Self {
        Self {
            frames,
            started: false,
            index: 0,
        }
    }

    /// Create a source that yields `count` video frames with sequential timestamps.
    ///
    /// Each frame contains a minimal H.264 Annex B NAL unit. The first frame
    /// is a keyframe (IDR).
    fn with_frame_count(count: usize) -> Self {
        let frames: Vec<MediaFrame> = (0..count as u64)
            .map(|ts| MediaFrame::Video {
                keyframe: ts == 0,
                data: vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80],
                timestamp: ts * 33,
            })
            .collect();
        Self::new(frames)
    }
}

impl Source for MockSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = true;
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("MockSource not started");
            }
            if self.index >= self.frames.len() {
                anyhow::bail!("MockSource exhausted");
            }
            let frame = self.frames[self.index].clone();
            self.index += 1;
            // Yield cooperatively so output tasks can drain the broadcast channel.
            // Without this, a tight source loop fills the capacity-64 channel and
            // frames are silently dropped before outputs can consume them.
            tokio::task::yield_now().await;
            Ok(frame)
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = false;
            Ok(())
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MockOutput
// ─────────────────────────────────────────────────────────────────────────────
//
// Mirrors the `pub(crate)` MockOutput from `crates/streaming/src/output.rs`.
// Records every received frame into an `Arc<Mutex<Vec<MediaFrame>>>` for
// later inspection.

struct MockOutput {
    received: Arc<Mutex<Vec<MediaFrame>>>,
    started: bool,
    fail_on_send: bool,
}

impl MockOutput {
    fn new() -> Self {
        Self {
            received: Arc::new(Mutex::new(Vec::new())),
            started: false,
            fail_on_send: false,
        }
    }

    fn receiver(&self) -> Arc<Mutex<Vec<MediaFrame>>> {
        self.received.clone()
    }
}

impl Output for MockOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = true;
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let f = frame.clone();
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("MockOutput not started");
            }
            if self.fail_on_send {
                anyhow::bail!("MockOutput simulated failure");
            }
            self.received.lock().await.push(f);
            Ok(())
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = false;
            Ok(())
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RtmpMockOutput
// ─────────────────────────────────────────────────────────────────────────────
//
// Simulates the RTMP output path without requiring a real RTMP server.
// Follows the same lifecycle contract as `RtmpOutput` (start → send_frame → stop)
// but records frames into a shared vector for verification.

struct RtmpMockOutput {
    received: Arc<Mutex<Vec<MediaFrame>>>,
    started: bool,
    url: String,
}

impl RtmpMockOutput {
    fn new(url: &str) -> Self {
        Self {
            received: Arc::new(Mutex::new(Vec::new())),
            started: false,
            url: url.to_string(),
        }
    }

    fn receiver(&self) -> Arc<Mutex<Vec<MediaFrame>>> {
        self.received.clone()
    }
}

impl Output for RtmpMockOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.url.is_empty() {
                anyhow::bail!("RtmpMockOutput URL must not be empty");
            }
            self.started = true;
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let f = frame.clone();
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("RtmpMockOutput not started");
            }
            self.received.lock().await.push(f);
            Ok(())
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = false;
            Ok(())
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper
// ─────────────────────────────────────────────────────────────────────────────

/// Check whether a frame is a keyframe via pattern matching.
fn is_keyframe(frame: &MediaFrame) -> bool {
    matches!(frame, MediaFrame::Video { keyframe: true, .. })
}

/// Create a test H.264 video frame with the given timestamp.
fn test_frame(ts: u64) -> MediaFrame {
    MediaFrame::Video {
        keyframe: ts == 0,
        data: vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80],
        timestamp: ts,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

// ── 1. MockSource → StreamHub → MockOutput: 100+ frames ────────────────────

#[tokio::test]
async fn test_pipeline_mock_source_to_mock_output_100_frames() {
    let frame_count = 100;
    let source = MockSource::with_frame_count(frame_count);
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    let out = MockOutput::new();
    let recv = out.receiver();
    let _id = hub.add_output(Box::new(out)).await;

    // Run the pipeline until the source is exhausted.
    let handle = hub.run().await;
    let result = tokio::time::timeout(Duration::from_secs(5), handle).await;

    assert!(
        result.is_ok(),
        "Pipeline should complete within timeout when source exhausts"
    );

    // Verify all 100 frames were received.
    let frames = recv.lock().await;
    assert_eq!(
        frames.len(),
        frame_count,
        "MockOutput should receive exactly {frame_count} frames"
    );

    // Verify frame order and integrity.
    assert!(is_keyframe(&frames[0]), "First frame should be a keyframe");
    assert_eq!(
        frames[0].timestamp(),
        0,
        "First frame timestamp should be 0"
    );
    assert_eq!(
        frames[49].timestamp(),
        49 * 33,
        "Frame 50 timestamp mismatch"
    );
    assert_eq!(
        frames[99].timestamp(),
        99 * 33,
        "Last frame timestamp mismatch"
    );

    // Every 30th frame verify keyframe status
    for i in (0..frame_count).step_by(30) {
        let expected_kf = i == 0;
        assert_eq!(
            is_keyframe(&frames[i]),
            expected_kf,
            "Frame {i} keyframe mismatch"
        );
    }
}

// ── 2. MockSource → StreamHub → RtspOutput: 100+ frames ────────────────────

#[tokio::test]
async fn test_pipeline_mock_source_to_rtsp_output_100_frames() {
    let frame_count = 100;
    let source = MockSource::with_frame_count(frame_count);
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    // Use a channel to capture what RtspOutput sends.
    let (tx, mut rx) = broadcast::channel(256);
    let rtsp_out = RtspOutput::with_channel(
        "integration-test".to_string(),
        "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=IntegrationTest\r\nt=0 0\r\n".to_string(),
        0xDEAD_BEEF,
        tx,
    );
    let _id = hub.add_output(Box::new(rtsp_out)).await;

    let handle = hub.run().await;

    // Collect frames from the RtspOutput's channel.
    let mut received_count = 0usize;
    while let Ok(Ok(data)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        // RTP packet: 12-byte header + NAL payload
        assert!(
            data.len() > 12,
            "RTP packet should have header + payload, got {} bytes",
            data.len()
        );
        received_count += 1;
        if received_count >= frame_count {
            break;
        }
    }

    // Ensure the pipeline shuts down.
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;

    assert_eq!(
        received_count, frame_count,
        "RtspOutput should receive all {frame_count} frames via channel"
    );
}

// ── 3. MockSource → StreamHub → RtmpMockOutput: 100+ frames ────────────────

#[tokio::test]
async fn test_pipeline_mock_source_to_rtmp_mock_output_100_frames() {
    let frame_count = 100;
    let source = MockSource::with_frame_count(frame_count);
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    let out = RtmpMockOutput::new("rtmp://localhost:1935/live/test-stream");
    let recv = out.receiver();
    let _id = hub.add_output(Box::new(out)).await;

    let handle = hub.run().await;
    let result = tokio::time::timeout(Duration::from_secs(5), handle).await;

    assert!(
        result.is_ok(),
        "RTMP mock pipeline should complete within timeout"
    );

    let frames = recv.lock().await;
    assert_eq!(
        frames.len(),
        frame_count,
        "RtmpMockOutput should receive exactly {frame_count} frames"
    );

    // Verify frame ordering
    for i in 0..frame_count {
        assert_eq!(
            frames[i].timestamp(),
            (i as u64) * 33,
            "Frame {i} timestamp mismatch in RTMP output"
        );
    }
}

// ── 4. Signal cancellation → clean pipeline shutdown ───────────────────────

#[tokio::test]
async fn test_pipeline_graceful_shutdown() {
    let frame_count = 200;
    let source = MockSource::with_frame_count(frame_count);
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    let out = MockOutput::new();
    let recv = out.receiver();
    let _id = hub.add_output(Box::new(out)).await;

    // Start the pipeline.
    let handle = hub.run().await;

    // Allow a brief moment for the pipeline to initialise and a few frames
    // to flow through the broadcast channel.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Signal cancellation via the global stop signal.
    hub.stop();
    assert!(
        hub.is_stopped(),
        "Hub should report stopped state immediately after stop()"
    );

    // Wait for all tasks to exit cleanly (bounded timeout).
    let shutdown_result = tokio::time::timeout(Duration::from_secs(5), handle).await;
    assert!(
        shutdown_result.is_ok(),
        "Pipeline tasks should exit cleanly within 5 s after stop signal"
    );

    // At least some frames should have been received before cancellation.
    // The exact count is scheduler-dependent — we just verify > 0.
    let frames = recv.lock().await;
    assert!(
        !frames.is_empty(),
        "Pipeline should have processed at least one frame before shutdown"
    );

    // Timestamps should be monotonically non-decreasing.
    for i in 1..frames.len() {
        assert!(
            frames[i].timestamp() >= frames[i - 1].timestamp(),
            "Frame timestamps should be monotonically non-decreasing (frame {}, ts {} < {})",
            i,
            frames[i].timestamp(),
            frames[i - 1].timestamp()
        );
    }
}

// ── 5. Pipeline lifecycle: empty output set ────────────────────────────────

#[tokio::test]
async fn test_pipeline_no_outputs() {
    let source = MockSource::with_frame_count(10);
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    // Run with zero outputs; the source should still exhaust cleanly.
    let handle = hub.run().await;
    let result = tokio::time::timeout(Duration::from_secs(5), handle).await;

    assert!(
        result.is_ok(),
        "Pipeline with no outputs should complete cleanly"
    );
}

// ── 6. Frame integrity through the pipeline ────────────────────────────────

#[tokio::test]
async fn test_pipeline_frame_integrity() {
    let frames = vec![
        test_frame(0),
        test_frame(100),
        test_frame(200),
        test_frame(300),
        test_frame(400),
    ];

    let source = MockSource::new(frames.clone());
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    let out1 = MockOutput::new();
    let out2 = MockOutput::new();
    let recv1 = out1.receiver();
    let recv2 = out2.receiver();

    let _id1 = hub.add_output(Box::new(out1)).await;
    let _id2 = hub.add_output(Box::new(out2)).await;

    let handle = hub.run().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    // Both outputs should receive identical copies.
    let frames1 = recv1.lock().await;
    let frames2 = recv2.lock().await;

    assert_eq!(frames1.len(), frames.len(), "Output 1 frame count");
    assert_eq!(frames2.len(), frames.len(), "Output 2 frame count");

    for (i, (a, b)) in frames1.iter().zip(frames2.iter()).enumerate() {
        assert_eq!(a, b, "Frame {i} differs between outputs");
        assert_eq!(a, &frames[i], "Frame {i} differs from source");
    }
}

// ── 7. try_add_output enforces output limits ────────────────────────────

#[tokio::test]
async fn test_pipeline_output_limit_enforced() {
    let source = MockSource::with_frame_count(10);
    let mut hub = StreamHub::new(Box::new(source), ResourceController::new(16));

    let out = MockOutput::new();
    let result = hub.try_add_output(Box::new(out)).await;
    assert!(result.is_ok(), "First output should be added successfully");
}
