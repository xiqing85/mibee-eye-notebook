//! MP4 segment archive file output adapter.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use crate::output::Output;
use crate::source::MediaFrame;

// ---------------------------------------------------------------------------
// FileOutput
// ---------------------------------------------------------------------------

/// MP4 segment archive output.
///
/// Spawns an ffmpeg subprocess that reads raw H.264 (Annex B) from its
/// stdin and muxes it into rolling MP4 segment files. Segments are named
/// `{camera_id}_{YYYYmmddHHMMSS}.mp4` and rotated every
/// `segment_duration_secs` seconds.
///
/// When the total size of the output directory exceeds `max_capacity_mb`,
/// the oldest segment is deleted before starting a new one (FIFO pruning).
///
/// Audio is not yet muxed (TODO: feed AAC frames once AudioCaptureSource
/// is wired into the hub).
pub struct FileOutput {
    /// Output directory for segment files.
    path: String,
    /// Camera identifier (used as filename prefix).
    camera_id: String,
    /// Segment duration in seconds.
    segment_duration_secs: u64,
    /// Max total directory size in MB (0 = unlimited).
    max_capacity_mb: u64,
    /// ffmpeg child process stdin (Some while running).
    stdin: Option<tokio::process::ChildStdin>,
    /// ffmpeg child process handle (Some while running).
    child: Option<tokio::process::Child>,
    /// Whether the output has been started.
    started: bool,
    /// Frame counter — used to throttle pruning checks.
    frame_count: u64,
}

impl FileOutput {
    /// Create a new file output.
    ///
    /// The `path` directory is created (recursively) on [`start`](Output::start).
    pub fn new(
        path: &str,
        camera_id: &str,
        segment_duration_secs: u64,
        max_capacity_mb: u64,
    ) -> Self {
        Self {
            path: path.to_string(),
            camera_id: camera_id.to_string(),
            segment_duration_secs,
            max_capacity_mb,
            stdin: None,
            child: None,
            started: false,
            frame_count: 0,
        }
    }

    /// Delete the oldest MP4 file in `self.path` if the directory's total
    /// size exceeds `max_capacity_mb`. Returns true if a file was deleted.
    fn prune_oldest_if_needed(&self) -> std::io::Result<bool> {
        if self.max_capacity_mb == 0 {
            return Ok(false);
        }
        let max_bytes = self.max_capacity_mb * 1024 * 1024;
        let mut entries: Vec<_> = std::fs::read_dir(&self.path)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                if meta.is_file() && e.file_name().to_string_lossy().ends_with(".mp4") {
                    Some((e.path(), meta.modified().ok()?, meta.len()))
                } else {
                    None
                }
            })
            .collect();
        let total: u64 = entries.iter().map(|(_, _, s)| *s).sum();
        if total <= max_bytes {
            return Ok(false);
        }
        // Sort oldest-first by mtime.
        entries.sort_by_key(|(_, mtime, _)| *mtime);
        // Delete oldest files until under limit.
        let mut remaining = total;
        for (path, _, size) in &entries {
            if remaining <= max_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                tracing::info!(file = %path.display(), "pruned old recording segment");
                remaining -= *size;
            }
        }
        Ok(remaining < total)
    }
}

impl Output for FileOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let path = self.path.clone();
        let camera_id = self.camera_id.clone();
        let segment_secs = self.segment_duration_secs;
        Box::pin(async move {
            // Ensure output directory exists.
            std::fs::create_dir_all(&path)?;

            // Segment filename pattern: {camera_id}_{YYYYmmddHHMMSS}.mp4
            let pattern = format!("{}/{}_%Y%m%d%H%M%S.mp4", path, camera_id);

            // Spawn ffmpeg reading raw H.264 from stdin and muxing to MP4 segments.
            // -f h264          : input format is raw H.264 Annex B
            // -i pipe:0        : read from stdin
            // -c copy          : no re-encode (stream copy)
            // -f segment       : use segmenting muxer
            // -segment_time N  : segment duration in seconds
            // -reset_timestamps 1 : reset PTS at each segment boundary
            // -strftime 1      : use strftime patterns in filename
            // -movflags +faststart : optimize MP4 for progressive download
            let mut cmd = tokio::process::Command::new("ffmpeg");
            cmd.stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
            cmd.args([
                "-hide_banner",
                "-loglevel",
                "warning",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-c",
                "copy",
                "-f",
                "segment",
                "-segment_time",
                &segment_secs.to_string(),
                "-reset_timestamps",
                "1",
                "-strftime",
                "1",
                "-movflags",
                "+faststart",
                &pattern,
            ]);

            tracing::info!(camera_id = %camera_id, path = %path, segment_secs, "starting FileOutput ffmpeg subprocess");
            let mut child = cmd.spawn().map_err(|e| {
                anyhow::anyhow!(
                    "failed to spawn ffmpeg for FileOutput: {}. Is ffmpeg installed?",
                    e
                )
            })?;

            let stdin = child.stdin.take().ok_or_else(|| {
                anyhow::anyhow!("ffmpeg stdin not available — child process died?")
            })?;

            self.stdin = Some(stdin);
            self.child = Some(child);
            self.started = true;
            observability::inc_recording_active();
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        match frame {
            MediaFrame::Video { data, .. } => {
                let data = data.clone();
                Box::pin(async move {
                    if !self.started {
                        anyhow::bail!("FileOutput not started");
                    }
                    let stdin = self.stdin.as_mut().ok_or_else(|| {
                        anyhow::anyhow!("FileOutput stdin closed — ffmpeg process may have died")
                    })?;

                    // Write start code + NAL data (Annex B format expected by ffmpeg).
                    // H.264 start codes: 0x00 0x00 0x00 0x01.
                    use tokio::io::AsyncWriteExt;
                    stdin.write_all(&[0x00, 0x00, 0x00, 0x01]).await?;
                    stdin.write_all(&data).await?;

                    // Periodic pruning check (every 1000 frames ≈ ~33s at 30fps).
                    self.frame_count += 1;
                    if self.frame_count % 1000 == 0 {
                        if let Err(e) = self.prune_oldest_if_needed() {
                            tracing::warn!(error = %e, "FileOutput pruning check failed");
                        }
                    }
                    Ok(())
                })
            }
            MediaFrame::Audio { .. } => Box::pin(async move {
                // Audio not yet muxed; silently drop.
                Ok(())
            }),
        }
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Close stdin to signal EOF to ffmpeg.
            if let Some(mut stdin) = self.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.shutdown().await;
            }
            // Wait for ffmpeg to flush and exit (with timeout).
            if let Some(mut child) = self.child.take() {
                match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                    Ok(Ok(status)) => {
                        tracing::info!(status = %status, camera_id = %self.camera_id, "FileOutput ffmpeg exited");
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "FileOutput ffmpeg wait failed");
                    }
                    Err(_) => {
                        tracing::warn!("FileOutput ffmpeg did not exit in 5s, killing");
                        let _ = child.kill().await;
                    }
                }
            }
            self.started = false;
            observability::dec_recording_active();
            tracing::info!(camera_id = %self.camera_id, "FileOutput stopped");
            Ok(())
        })
    }
}
