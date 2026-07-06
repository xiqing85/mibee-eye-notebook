//! MP4 segment archive file output adapter.
//!
//! Uses [`muxide`] (pure-Rust MP4 muxer) to write rolling `.mp4` segment files
//! from the H.264 NAL stream. Segments are named `{camera_id}_{YYYYmmddHHMMSS}.mp4`
//! and rotated every `segment_duration_secs` seconds. When the total size of
//! the output directory exceeds `max_capacity_mb`, the oldest segment is
//! deleted before starting a new one (FIFO pruning).
//!
//! This replaces the previous ffmpeg `-f segment` subprocess. muxide accepts
//! Annex B H.264 input directly and converts to AVCC internally — no manual
//! SPS/PPS bookkeeping is required.

use std::future::Future;
use std::io::BufWriter;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use muxide::api::{Muxer, MuxerBuilder, VideoCodec};
use parking_lot::Mutex;

use crate::capture_source::StreamDimensions;
use crate::output::Output;
use crate::source::MediaFrame;

/// MP4 segment archive output.
///
/// Writes H.264 NAL units into rolling MP4 segment files via muxide. Audio
/// frames are currently dropped (TODO: wire once AAC encoding is exercised).
pub struct FileOutput {
    /// Output directory for segment files.
    path: String,
    /// Camera identifier (used as filename prefix).
    camera_id: String,
    /// Segment duration in seconds.
    segment_duration_secs: u64,
    /// Max total directory size in MB (0 = unlimited).
    max_capacity_mb: u64,
    /// Width of the video track (passed to the muxer).
    width: u32,
    /// Height of the video track (passed to the muxer).
    height: u32,
    /// Frame rate of the video track (passed to the muxer).
    fps: f32,
    /// Optional live dimensions handle from the capture source. When present,
    /// the real negotiated `(width, height, fps)` is read from here at segment
    /// open time so muxer metadata matches the actual encoded frames rather
    /// than the hardcoded default.
    dimensions_handle: Option<Arc<Mutex<Option<StreamDimensions>>>>,
    /// Active muxer + its underlying file. `None` when not started, before the
    /// first keyframe arrives, or between segment rotations.
    active: Option<ActiveSegment>,
    /// Presentation timestamp (seconds) of the first frame in the current
    /// segment — used to compute per-frame PTS and detect rotation boundaries.
    segment_start_pts: f64,
    /// Whether the output has been started.
    started: bool,
    /// Frame counter — used to throttle pruning checks.
    frame_count: u64,
}

struct ActiveSegment {
    muxer: Muxer<BufWriter<std::fs::File>>,
    /// Wall-clock creation time, for rotation checks.
    created_at: std::time::Instant,
}

impl FileOutput {
    /// Create a new file output with default 1280x720@30 video dimensions.
    ///
    /// The `path` directory is created (recursively) on [`start`](Output::start).
    /// Dimensions are used to configure the muxer's video track; if they don't
    /// match the actual encoded frames the resulting MP4 may have incorrect
    /// metadata but will still be playable. Pass a live dimensions handle via
    /// [`with_dimensions_handle`](Self::with_dimensions_handle) to ensure the
    /// muxer picks up the real negotiated geometry.
    pub fn new(path: &str, camera_id: &str, segment_duration_secs: u64, max_capacity_mb: u64) -> Self {
        Self {
            path: path.to_string(),
            camera_id: camera_id.to_string(),
            segment_duration_secs,
            max_capacity_mb,
            width: 1280,
            height: 720,
            fps: 30.0,
            dimensions_handle: None,
            active: None,
            segment_start_pts: 0.0,
            started: false,
            frame_count: 0,
        }
    }

    /// Set the video track dimensions and frame rate.
    ///
    /// Should be called before [`start`](Output::start). Used to populate the
    /// muxer's track configuration so the resulting MP4 has correct metadata.
    pub fn with_dimensions(mut self, width: u32, height: u32, fps: f32) -> Self {
        self.width = width;
        self.height = height;
        self.fps = fps;
        self
    }

    /// Attach a live dimensions handle from the capture source.
    ///
    /// When set, [`open_new_segment`](Self::open_new_segment) reads the real
    /// negotiated `(width, height, fps)` from this handle, so the muxer track
    /// metadata matches the actual encoded frames even when the output is
    /// constructed before the first frame arrives.
    pub fn with_dimensions_handle(
        mut self,
        handle: Arc<Mutex<Option<StreamDimensions>>>,
    ) -> Self {
        self.dimensions_handle = Some(handle);
        self
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

    /// Open a new segment file and initialize the muxer.
    fn open_new_segment(&mut self) -> Result<()> {
        // Close the current segment first, if any.
        self.close_current_segment()?;

        // Pick up the real negotiated dimensions if a live handle is attached.
        // This lets the muxer emit correct track metadata even though the
        // output was constructed before the first frame arrived.
        if let Some(handle) = &self.dimensions_handle {
            if let Some(d) = *handle.lock() {
                self.width = d.width;
                self.height = d.height;
                self.fps = d.fps;
            }
        }

        // Generate the timestamped filename.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Format as YYYYmmddHHMMSS using a simple libc-free conversion.
        let stamp = format_local_timestamp(now);
        let filename = format!("{}/{}_{}.mp4", self.path, self.camera_id, stamp);

        let file = std::fs::File::create(&filename)
            .with_context(|| format!("failed to create segment file {filename}"))?;
        let writer = BufWriter::new(file);

        let muxer = MuxerBuilder::new(writer)
            .video(VideoCodec::H264, self.width, self.height, self.fps as f64)
            .build()
            .context("muxide MuxerBuilder failed")?;

        self.active = Some(ActiveSegment {
            muxer,
            created_at: std::time::Instant::now(),
        });

        tracing::info!(
            camera_id = %self.camera_id,
            file = %filename,
            "opened new MP4 segment"
        );
        Ok(())
    }

    /// Flush and finalize the current segment, if any.
    fn close_current_segment(&mut self) -> Result<()> {
        if let Some(active) = self.active.take() {
            let muxer = active.muxer;
            // muxide's finish flushes the moov/mdat and drops the writer,
            // which closes the underlying file.
            muxer.finish_with_stats()
                .map(|_stats| ())
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "muxide finish_with_stats failed (segment may be incomplete)");
                });
        }
        Ok(())
    }
}

impl Output for FileOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Ensure output directory exists.
            std::fs::create_dir_all(&self.path)?;

            // The first segment is opened lazily on the first keyframe so the
            // muxer picks up the real negotiated dimensions (which the capture
            // source publishes only after the first frame arrives).
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
            MediaFrame::Video {
                data,
                keyframe,
                timestamp,
            } => {
                let data = data.clone();
                let keyframe = *keyframe;
                let timestamp = *timestamp;
                Box::pin(async move {
                    if !self.started {
                        anyhow::bail!("FileOutput not started");
                    }

                    // Check for segment rotation: rotate when the wall-clock
                    // age of the current segment exceeds the duration, at a
                    // keyframe boundary so each segment starts cleanly. If no
                    // segment is open yet (lazy first-segment open), open one
                    // immediately regardless of keyframe status so no frames
                    // are dropped while waiting for the next IDR.
                    let needs_rotation = self
                        .active
                        .as_ref()
                        .map(|a| {
                            a.created_at.elapsed().as_secs() >= self.segment_duration_secs
                        })
                        .unwrap_or(true);

                    let open_now = needs_rotation && (keyframe || self.active.is_none());
                    if open_now {
                        if let Err(e) = self.open_new_segment() {
                            tracing::warn!(error = %e, "segment rotation failed, continuing with current segment");
                        }
                        // Reset segment-local PTS so the new segment starts at 0.
                        self.segment_start_pts = timestamp as f64 / 1000.0;
                    }

                    let active = self
                        .active
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("no active MP4 segment"))?;

                    // Build an Annex B byte stream from the single NAL unit.
                    // muxide accepts Annex B input; we prepend the 4-byte
                    // start code expected by the muxer's NAL scanner.
                    let mut annex_b = Vec::with_capacity(data.len() + 4);
                    annex_b.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    annex_b.extend_from_slice(&data);

                    // PTS in seconds, relative to the segment start.
                    let pts_secs = (timestamp as f64 / 1000.0) - self.segment_start_pts;
                    // Clamp to non-negative in case of clock skew.
                    let pts_secs = pts_secs.max(0.0);

                    active
                        .muxer
                        .write_video(pts_secs, &annex_b, keyframe)
                        .context("muxide write_video failed")?;

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
                // TODO: wire AAC track once the `aac` feature is exercised.
                Ok(())
            }),
        }
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Final flush of the current segment.
            if let Err(e) = self.close_current_segment() {
                tracing::warn!(error = %e, "FileOutput final flush failed");
            }
            self.started = false;
            observability::dec_recording_active();
            tracing::info!(camera_id = %self.camera_id, "FileOutput stopped");
            Ok(())
        })
    }
}

/// Format a Unix epoch second count as `YYYYmmddHHMMSS` (UTC).
///
/// A small libc-free implementation sufficient for filenames. Days-of-month
/// accounting uses a proleptic-Gregorian algorithm; precision to the second.
fn format_local_timestamp(epoch_secs: u64) -> String {
    // Civil-from-days algorithm (Howard Hinnant). Returns (y, m, d).
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        y, m, d, hour, min, sec
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_format_is_14_digits() {
        // Unix epoch 0 = 1970-01-01 00:00:00 UTC.
        let s = format_local_timestamp(0);
        assert_eq!(s, "19700101000000");
        assert_eq!(s.len(), 14);
    }

    #[test]
    fn timestamp_known_value() {
        // 2024-01-01 00:00:00 UTC = 1704067200.
        let s = format_local_timestamp(1_704_067_200);
        assert_eq!(s, "20240101000000");
    }

    #[test]
    fn timestamp_increments_seconds() {
        let a = format_local_timestamp(1_704_067_200);
        let b = format_local_timestamp(1_704_067_201);
        // The two should differ only in the last digit (seconds field).
        assert_eq!(&a[..13], &b[..13]);
        // The seconds digit should differ by one.
        let sa = &a[13..14];
        let sb = &b[13..14];
        assert_ne!(sa, sb);
    }

    #[test]
    fn prune_returns_false_when_unlimited() {
        let out = FileOutput::new("/tmp/mibee-test", "cam", 60, 0);
        assert!(!out.prune_oldest_if_needed().unwrap());
    }
}
