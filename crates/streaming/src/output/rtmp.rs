//! RTMP push output adapter.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use crate::output::{Output, parse_h264_nal_units};
use crate::source::MediaFrame;
use protocols::rtmp::{RtmpPushClient, build_video_nalus, build_video_sequence_header};

// ---------------------------------------------------------------------------
// RtmpOutput
// ---------------------------------------------------------------------------

/// RTMP push output.
///
/// Connects to an RTMP ingest point (such as MiBee NVR) and pushes
/// H.264/AAC frames as RTMP video/audio messages.
///
/// Uses the native [RtmpPushClient] for protocol-level push without
/// external dependencies on ffmpeg.
///
/// Audio frames are silently dropped for now.
pub struct RtmpOutput {
    /// RTMP URL (e.g. `rtmp://localhost:1935/live/stream`).
    url: String,
    /// Application name extracted from URL.
    app_name: String,
    /// Stream key extracted from URL.
    stream_key: String,
    /// RTMP host extracted from URL.
    host: String,
    /// RTMP port extracted from URL.
    port: u16,
    /// Push client (connected on start).
    client: Option<RtmpPushClient>,
    /// Whether the output has been started.
    started: bool,
    /// Whether the AVC sequence header has been sent.
    seq_header_sent: bool,
}

impl RtmpOutput {
    /// Create a new RTMP output from a full RTMP URL.
    ///
    /// The URL format is: `rtmp://host:port/app/streamKey`
    pub fn new(url: &str) -> Self {
        let (app_name, stream_key) = Self::parse_rtmp_url(url);
        let (host, port) = Self::parse_rtmp_host_port(url);
        Self {
            url: url.to_string(),
            app_name,
            stream_key,
            host,
            port,
            client: None,
            started: false,
            seq_header_sent: false,
        }
    }

    /// Create an RTMP output with explicit host, port, app, and stream key.
    pub fn new_with_parts(host: &str, port: u16, app: &str, stream: &str) -> Self {
        let url = format!("rtmp://{host}:{port}/{app}/{stream}");
        Self {
            url,
            app_name: app.to_string(),
            stream_key: stream.to_string(),
            host: host.to_string(),
            port,
            client: None,
            started: false,
            seq_header_sent: false,
        }
    }

    fn parse_rtmp_url(url: &str) -> (String, String) {
        // Strip "rtmp://" prefix, then parse path
        let rest = url.trim_start_matches("rtmp://");
        if let Some(slash_pos) = rest.find('/') {
            let after_host = &rest[slash_pos + 1..];
            if let Some(second_slash) = after_host.find('/') {
                (
                    after_host[..second_slash].to_string(),
                    after_host[second_slash + 1..].to_string(),
                )
            } else {
                (after_host.to_string(), String::new())
            }
        } else {
            ("live".to_string(), "stream".to_string())
        }
    }

    fn parse_rtmp_host_port(url: &str) -> (String, u16) {
        let rest = url.trim_start_matches("rtmp://");
        let host_part = if let Some(slash_pos) = rest.find('/') {
            &rest[..slash_pos]
        } else {
            rest
        };
        if let Some(colon_pos) = host_part.find(':') {
            (
                host_part[..colon_pos].to_string(),
                host_part[colon_pos + 1..].parse::<u16>().unwrap_or(1935),
            )
        } else {
            (host_part.to_string(), 1935)
        }
    }
}

impl Output for RtmpOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.url.is_empty() {
                anyhow::bail!("RtmpOutput URL must not be empty");
            }

            let mut client =
                RtmpPushClient::new(&self.host, self.port, &self.app_name, &self.stream_key);
            client.connect().await?;

            self.client = Some(client);
            self.started = true;
            self.seq_header_sent = false;
            tracing::info!("RtmpOutput started: {}", self.url);
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        match frame {
            MediaFrame::Video {
                data,
                keyframe,
                timestamp,
            } => {
                let data = data.clone();
                let kf = *keyframe;
                let ts = *timestamp as u32;
                Box::pin(async move {
                    if !self.started {
                        anyhow::bail!("RtmpOutput not started");
                    }
                    let client = self
                        .client
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("RtmpOutput client not connected"))?;

                    // Parse NAL units from the frame data (handle Annex B or AVCC)
                    let nal_units = parse_h264_nal_units(&data);
                    if nal_units.is_empty() {
                        return Ok(());
                    }

                    // Send AVC sequence header on first keyframe
                    let need_seq_header = kf && !self.seq_header_sent;
                    if need_seq_header {
                        let sps = nal_units
                            .iter()
                            .find(|n| n.first().map(|b| b & 0x1F) == Some(7));
                        let pps = nal_units
                            .iter()
                            .find(|n| n.first().map(|b| b & 0x1F) == Some(8));
                        if let (Some(sps), Some(pps)) = (sps, pps) {
                            let seq_header = build_video_sequence_header(sps, pps);
                            client.send_video(&seq_header, ts).await?;
                        }
                        self.seq_header_sent = true;
                    }

                    // Re-encode as AVCC (4-byte length prefix per NAL unit)
                    let mut avcc_data = Vec::new();
                    for nal in &nal_units {
                        avcc_data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        avcc_data.extend_from_slice(nal);
                    }

                    // Build RTMP video payload and send
                    let rtmp_payload = build_video_nalus(&avcc_data, kf, 0);
                    client.send_video(&rtmp_payload, ts).await?;

                    Ok(())
                })
            }
            MediaFrame::Audio { .. } => Box::pin(async move {
                // Audio frames not yet supported via RTMP push;
                // silently drop for now.
                Ok(())
            }),
        }
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if let Some(mut client) = self.client.take() {
                let _ = client.close().await;
            }
            self.started = false;
            tracing::info!("RtmpOutput stopped");
            Ok(())
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RtmpOutput tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rtmp_output_start_stop() {
        let mut out = RtmpOutput::new("rtmp://localhost:1935/live/stream");
        let result = out.start().await;
        // RTMP server may or may not be running; if start succeeded, verify stop.
        if result.is_ok() {
            assert!(out.started);
            out.stop().await.unwrap();
            assert!(!out.started);
        }
    }

    #[tokio::test]
    async fn test_rtmp_output_empty_url_fails() {
        let mut out = RtmpOutput::new("");
        assert!(out.start().await.is_err());
    }

    #[tokio::test]
    async fn test_rtmp_output_send_frame() {
        let mut out = RtmpOutput::new("rtmp://localhost:1935/live/stream");
        if out.start().await.is_err() {
            return; // RTMP server not available
        }
        // Send a video frame with Annex B H.264 data
        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![
                // Annex B start code + SPS NAL
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80, 0x1E,
                // Annex B start code + PPS NAL
                0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x3C, 0x80,
            ],
            timestamp: 100,
        };
        out.send_frame(&frame).await.unwrap();
        out.stop().await.unwrap();
    }

    #[test]
    fn test_rtmp_url_parsing() {
        let (app, key) = RtmpOutput::parse_rtmp_url("rtmp://example.com:1935/live/mykey");
        assert_eq!(app, "live");
        assert_eq!(key, "mykey");
    }

    #[test]
    fn test_rtmp_url_parsing_no_key() {
        let (app, key) = RtmpOutput::parse_rtmp_url("rtmp://example.com/app");
        assert_eq!(app, "app");
        assert_eq!(key, "");
    }

    #[test]
    fn test_rtmp_host_port_parsing() {
        let (host, port) = RtmpOutput::parse_rtmp_host_port("rtmp://example.com:1935/live/stream");
        assert_eq!(host, "example.com");
        assert_eq!(port, 1935);
    }

    #[test]
    fn test_rtmp_host_port_parsing_default_port() {
        let (host, port) = RtmpOutput::parse_rtmp_host_port("rtmp://example.com/live/stream");
        assert_eq!(host, "example.com");
        assert_eq!(port, 1935);
    }
    #[test]
    fn test_rtmp_output_new_with_parts() {
        let out = RtmpOutput::new_with_parts("localhost", 1935, "live", "test");
        assert_eq!(out.url, "rtmp://localhost:1935/live/test");
        assert_eq!(out.app_name, "live");
        assert_eq!(out.stream_key, "test");
        assert_eq!(out.host, "localhost");
        assert_eq!(out.port, 1935);
    }
}
