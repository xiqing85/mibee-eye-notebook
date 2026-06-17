//! Test-only mock output adapter.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::output::Output;
use crate::source::MediaFrame;

// Mock output that records received frames for verification.
pub struct MockOutput {
    received: Arc<Mutex<Vec<MediaFrame>>>,
    started: bool,
    fail_on_send: bool,
}

impl Default for MockOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl MockOutput {
    pub fn new() -> Self {
        Self {
            received: Arc::new(Mutex::new(Vec::new())),
            started: false,
            fail_on_send: false,
        }
    }

    pub fn with_fail() -> Self {
        Self {
            received: Arc::new(Mutex::new(Vec::new())),
            started: false,
            fail_on_send: true,
        }
    }

    pub fn receiver(&self) -> Arc<Mutex<Vec<MediaFrame>>> {
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Output trait tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_mock_output_lifecycle() {
        let mut out = MockOutput::new();
        out.start().await.unwrap();

        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![0x67],
            timestamp: 0,
        };
        out.send_frame(&frame).await.unwrap();

        let received = out.receiver();
        let frames = received.lock().await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);

        out.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_output_send_before_start() {
        let mut out = MockOutput::new();
        let frame = MediaFrame::Audio {
            data: vec![],
            timestamp: 0,
        };
        assert!(out.send_frame(&frame).await.is_err());
    }

    #[tokio::test]
    async fn test_mock_output_failure_mode() {
        let mut out = MockOutput::with_fail();
        out.start().await.unwrap();
        let frame = MediaFrame::Video {
            keyframe: false,
            data: vec![],
            timestamp: 0,
        };
        assert!(out.send_frame(&frame).await.is_err());
    }
}
