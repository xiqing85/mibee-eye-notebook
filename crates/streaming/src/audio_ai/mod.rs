//! On-device audio intelligence (YAMNet sound events + Silero voice
//! presence), consuming the always-on 16 kHz monitor stream.
//!
//! Mirrors the `ai` module's lifecycle philosophy: fail-open (a missing
//! model, missing ONNX Runtime library, or missing microphone leaves the
//! engine inactive — the service runs on), events only for configured
//! classes, and a rising-edge + per-class cooldown state machine so a busy
//! street cannot spam the alarm pipeline.
//!
//! # Pipeline
//!
//! ```text
//! AudioMonitor (16 kHz mono, 512-sample chunks)
//!   ├─ Silero VAD  → voice-presence signal (status/metric; shared
//!   │                 precondition for the future wake-word path)
//!   └─ YAMNet      → every 0.96 s window, 0.48 s hop → 521 sigmoid scores
//!       → 3-patch mean vote → hysteresis → rising edge + per-class cooldown
//!       → SoundEvent → (main.rs bridge) SSE `alarm` source:"audio"
//!         + GB/T 28181 Alarm NOTIFY
//! ```
//!
//! Silent windows (RMS below −60 dBFS) skip classification entirely — dog
//! barks and glass shatter are high-energy events, so the energy gate has no
//! false-negative risk and saves most of the CPU on a quiet device.

#[cfg(feature = "ai")]
pub mod classify;
pub mod labels;
mod labels_data;
pub mod state;
pub mod wav;

use std::sync::Arc;
#[cfg(feature = "ai")]
use std::time::Duration;

use capture::audio_monitor::AudioChunk;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::info;
#[cfg(feature = "ai")]
use tracing::warn;

/// YAMNet input window: 0.96 s of 16 kHz audio.
pub const WINDOW_SAMPLES: usize = 15_360;

/// Hop between consecutive windows: 0.48 s (50% overlap).
pub const HOP_SAMPLES: usize = 7680;

/// Number of consecutive window scores averaged before a class may fire
/// (≈2–3 s of smoothing — single-patch blips never alarm).
pub const VOTE_PATCHES: usize = 3;

/// Windows quieter than this (dBFS RMS) skip classification entirely.
pub const SILENCE_GATE_DBFS: f32 = -60.0;

/// One fired sound event (SPEC v1 §6 `alarm` with `source:"audio"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundEvent {
    /// Canonical YAMNet/AudioSet display name (config key, e.g. `"Dog"`).
    pub class: String,
    /// Chinese display label (empty when no mapping exists).
    pub label_zh: String,
    /// Voted score at fire time (mean of the last `VOTE_PATCHES` windows).
    pub score: f32,
    /// Unix epoch milliseconds captured at fire time.
    pub timestamp_ms: u64,
}

/// `[audio_ai]` configuration section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioAiConfig {
    /// Master switch. Off by default — the microphone is privacy-sensitive
    /// input, so continuous listening is opt-in.
    pub enabled: bool,
    /// Input device selector: `"default"` or a substring of the device
    /// description.
    pub device: String,
    /// Watched YAMNet class display names (exact match). Unknown names are
    /// logged and ignored at startup.
    pub classes: Vec<String>,
    /// Voted-score threshold a class must reach to fire (0..=1).
    pub threshold: f32,
    /// Per-class cooldown in seconds.
    pub cooldown_secs: u64,
    /// YAMNet ONNX model path.
    pub model_path: String,
    /// Silero VAD ONNX model path (voice-presence signal).
    pub vad_model_path: String,
}

impl Default for AudioAiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device: "default".into(),
            classes: labels::DEFAULT_CLASSES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            threshold: 0.3,
            cooldown_secs: 30,
            model_path: "models/audio/yamnet.onnx".into(),
            vad_model_path: "models/audio/silero_vad.onnx".into(),
        }
    }
}

/// Inference backends (only exist in `ai`-feature builds).
#[cfg(feature = "ai")]
struct AudioAiInternals {
    classifier: Arc<classify::YamnetClassifier>,
    vad: Option<Arc<std::sync::Mutex<classify::SileroVad>>>,
}

/// Process-wide audio AI engine: owns the models, the voice-presence flag
/// and the sound-event bus.
pub struct AudioAiEngine {
    #[cfg_attr(not(feature = "ai"), allow(dead_code))]
    config: AudioAiConfig,
    active: bool,
    inactive_reason: String,
    voice_present: Arc<RwLock<bool>>,
    #[allow(dead_code)]
    event_tx: broadcast::Sender<SoundEvent>,
    #[cfg(feature = "ai")]
    internals: Option<AudioAiInternals>,
}

impl AudioAiEngine {
    /// Build the engine from config (fail-open on every load failure).
    #[must_use]
    pub fn from_config(config: &AudioAiConfig) -> Self {
        let (active, reason, internals) = Self::load(config);
        if active {
            info!(
                classes = ?config.classes,
                threshold = config.threshold,
                cooldown_secs = config.cooldown_secs,
                "audio_ai: sound-event engine loaded"
            );
        } else {
            info!(reason = %reason, "audio_ai: disabled");
        }
        let (event_tx, _) = broadcast::channel(16);
        #[cfg(not(feature = "ai"))]
        let _ = internals; // the field does not exist in non-ai builds
        Self {
            config: config.clone(),
            active,
            inactive_reason: reason,
            voice_present: Arc::new(RwLock::new(false)),
            event_tx,
            #[cfg(feature = "ai")]
            internals,
        }
    }

    #[cfg(feature = "ai")]
    fn load(config: &AudioAiConfig) -> (bool, String, Option<AudioAiInternals>) {
        if !config.enabled {
            return (false, "disabled by configuration".into(), None);
        }
        let classifier = match classify::YamnetClassifier::new(&config.model_path) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!(
                    error = %e,
                    model = %config.model_path,
                    "audio_ai: YAMNet unavailable, sound events stay disabled (fail-open)"
                );
                return (
                    false,
                    "YAMNet model unavailable (see startup log)".into(),
                    None,
                );
            }
        };
        let vad = match classify::SileroVad::new(&config.vad_model_path) {
            Ok(v) => Some(Arc::new(std::sync::Mutex::new(v))),
            Err(e) => {
                // The VAD is an auxiliary signal; sound events work without it.
                warn!(
                    error = %e,
                    model = %config.vad_model_path,
                    "audio_ai: Silero VAD unavailable (voice-presence signal off)"
                );
                None
            }
        };
        (
            true,
            String::new(),
            Some(AudioAiInternals { classifier, vad }),
        )
    }

    #[cfg(not(feature = "ai"))]
    fn load(config: &AudioAiConfig) -> (bool, String, Option<()>) {
        if !config.enabled {
            return (false, "disabled by configuration".into(), None);
        }
        (false, "built without the `ai` feature".into(), None)
    }

    /// Whether sound-event detection is running.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Why the engine is inactive (empty string when active).
    #[must_use]
    pub fn inactive_reason(&self) -> &str {
        &self.inactive_reason
    }

    /// Current voice-presence flag (Silero VAD; always false without VAD).
    #[must_use]
    pub fn voice_present(&self) -> bool {
        *self.voice_present.read()
    }

    /// Subscribe to fired sound events.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<SoundEvent> {
        self.event_tx.subscribe()
    }

    /// Spawn the monitor-consuming worker. Returns immediately when the
    /// engine is inactive.
    pub fn spawn_worker(
        &self,
        #[cfg(feature = "ai")] mut rx: broadcast::Receiver<AudioChunk>,
        #[cfg(not(feature = "ai"))] _rx: broadcast::Receiver<AudioChunk>,
    ) -> tokio::task::JoinHandle<()> {
        #[cfg(feature = "ai")]
        let Some(internals) = self.internals.as_ref().map(|i| AudioAiInternals {
            classifier: Arc::clone(&i.classifier),
            vad: i.vad.clone(),
        }) else {
            return tokio::spawn(async {});
        };
        #[cfg(not(feature = "ai"))]
        {
            return tokio::spawn(async {});
        }
        #[cfg(feature = "ai")]
        {
            let classifier = internals.classifier;
            let vad = internals.vad;
            let mut state = state::SoundEventState::new(
                labels::resolve(&self.config.classes),
                self.config.threshold,
                Duration::from_secs(self.config.cooldown_secs.max(1)),
            );
            if state.watched().is_empty() {
                warn!("audio_ai: no resolvable watched classes — worker idle");
                return tokio::spawn(async {});
            }
            let voice_present = Arc::clone(&self.voice_present);
            let event_tx = self.event_tx.clone();
            info!(
                model = classifier.model_path(),
                watched = ?state.watched(),
                "audio_ai: sound-event worker started"
            );

            tokio::spawn(async move {
                let mut window: Vec<f32> = Vec::with_capacity(WINDOW_SAMPLES + HOP_SAMPLES);
                loop {
                    match rx.recv().await {
                        Ok(chunk) => {
                            let f32_chunk: Vec<f32> =
                                chunk.samples.iter().map(|&s| s as f32 / 32_768.0).collect();
                            if let Some(vad) = &vad
                                && let Ok(prob) = vad
                                    .lock()
                                    .map_err(|_| anyhow::anyhow!("vad mutex poisoned"))
                                    .and_then(|mut v| v.process(&f32_chunk))
                            {
                                *voice_present.write() = prob > 0.5;
                            }
                            window.extend_from_slice(&f32_chunk);
                            while window.len() >= WINDOW_SAMPLES {
                                let silent = {
                                    let w = &window[..WINDOW_SAMPLES];
                                    rms_dbfs(w) < SILENCE_GATE_DBFS
                                };
                                if !silent {
                                    let waveform: Vec<f32> = window[..WINDOW_SAMPLES].to_vec();
                                    let result = tokio::task::spawn_blocking({
                                        let classifier = Arc::clone(&classifier);
                                        move || classifier.classify(&waveform)
                                    })
                                    .await
                                    .unwrap_or_else(|e| {
                                        Err(anyhow::anyhow!("audio inference task failed: {e}"))
                                    });
                                    match result {
                                        Ok(scores) => {
                                            let now_ms = unix_now_ms();
                                            for ev in state.update(
                                                &scores,
                                                std::time::Instant::now(),
                                                now_ms,
                                            ) {
                                                info!(
                                                    class = %ev.class,
                                                    score = ev.score,
                                                    "audio_ai: sound event"
                                                );
                                                let _ = event_tx.send(ev);
                                            }
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "audio_ai: YAMNet inference error");
                                        }
                                    }
                                }
                                window.drain(..HOP_SAMPLES);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "audio_ai: worker lagged behind monitor");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                info!("audio_ai: sound-event worker stopped");
            })
        }
    }
}

/// Shared test/CLI probe: ort panics (not errors) when no ONNX Runtime
/// library can be loaded, so callers that only *optionally* use inference
/// must check this first.
#[doc(hidden)]
pub fn ort_available_for_test() -> bool {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return true;
    }
    [
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
        "/usr/local/lib/libonnxruntime.so",
        "/usr/lib/libonnxruntime.so",
    ]
    .iter()
    .any(|p| std::path::Path::new(p).exists())
}

/// Deterministic offline self-test: run the full sound-event pipeline on a
/// WAV file and report what fired. Used by `mibee-eye --selftest-audio`
/// (deployment verification without playing audio through a speaker).
///
/// # Errors
///
/// File/parse errors, or "built without the `ai` feature".
pub fn selftest_wav(path: &str) -> anyhow::Result<serde_json::Value> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?;
    #[cfg_attr(not(feature = "ai"), allow(unused_variables))]
    let wav = wav::parse_wav(&bytes)?;
    #[cfg(feature = "ai")]
    {
        let classifier = classify::YamnetClassifier::new(&labels::self_model_path("yamnet"))?;
        let vad = classify::SileroVad::new(&labels::self_model_path("silero_vad"))
            .ok()
            .map(std::sync::Mutex::new);
        let mut resampler = capture::audio_monitor::LinearResampler::new(
            f64::from(wav.sample_rate) / f64::from(capture::audio_monitor::TARGET_RATE),
        );
        let samples = resampler.process(&wav.samples);
        let mut state = state::SoundEventState::new(
            labels::resolve(
                &labels::DEFAULT_CLASSES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect::<Vec<_>>(),
            ),
            0.3,
            Duration::from_secs(30),
        );
        let mut events = Vec::new();
        let mut windows = 0_u32;
        let mut voice_chunks = 0_u32;
        let mut total_chunks = 0_u32;
        let mut window: Vec<f32> = Vec::new();
        let mut pos = 0_usize;
        while pos < samples.len() {
            let take = (samples.len() - pos).min(512);
            let chunk = &samples[pos..pos + take];
            pos += take;
            total_chunks += 1;
            if let Some(v) = &vad
                && let Ok(p) = v
                    .lock()
                    .map_err(|_| anyhow::anyhow!("vad mutex poisoned"))
                    .and_then(|mut g| g.process(chunk))
                && p > 0.5
            {
                voice_chunks += 1;
            }
            window.extend_from_slice(chunk);
            while window.len() >= WINDOW_SAMPLES {
                windows += 1;
                if rms_dbfs(&window[..WINDOW_SAMPLES]) >= SILENCE_GATE_DBFS {
                    let scores = classifier.classify(&window[..WINDOW_SAMPLES])?;
                    let now_ms = unix_now_ms();
                    events.extend(state.update(&scores, std::time::Instant::now(), now_ms));
                }
                window.drain(..HOP_SAMPLES);
            }
        }
        // Serialize with the event fields the SSE alarm carries.
        let events: Vec<serde_json::Value> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "class": e.class,
                    "label_zh": e.label_zh,
                    "score": e.score,
                    "timestamp_ms": e.timestamp_ms,
                })
            })
            .collect();
        Ok(serde_json::json!({
            "file": path,
            "sample_rate": wav.sample_rate,
            "duration_s": (samples.len() as f64 / f64::from(capture::audio_monitor::TARGET_RATE)).round(),
            "windows": windows,
            "voice_present_ratio": if total_chunks > 0 {
                ((voice_chunks as f64 / total_chunks as f64) * 10_000.0).round() / 10_000.0
            } else { 0.0 },
            "events": events,
        }))
    }
    #[cfg(not(feature = "ai"))]
    {
        let _ = &bytes;
        anyhow::bail!("built without the `ai` feature")
    }
}

#[cfg_attr(not(feature = "ai"), allow(dead_code))]
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// RMS level of a normalized (±1) f32 buffer, in dBFS.
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -96.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum / samples.len() as f32).sqrt();
    if rms <= 0.0 {
        -96.0
    } else {
        (20.0 * rms.log10()).max(-96.0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = AudioAiConfig::default();
        assert!(!cfg.enabled, "continuous listening is opt-in");
        assert_eq!(cfg.device, "default");
        assert!(cfg.classes.contains(&"Dog".to_string()));
        assert!((cfg.threshold - 0.3).abs() < f32::EPSILON);
        assert_eq!(cfg.cooldown_secs, 30);
        assert_eq!(cfg.model_path, "models/audio/yamnet.onnx");
    }

    #[test]
    fn test_config_parse_roundtrip() {
        let cfg: AudioAiConfig = serde_json::from_str(
            r#"{"enabled":true,"classes":["Siren"],"threshold":0.4,"cooldown_secs":10}"#,
        )
        .expect("parse");
        assert!(cfg.enabled);
        assert_eq!(cfg.classes, vec!["Siren".to_string()]);
        assert_eq!(cfg.cooldown_secs, 10);
        // Unspecified keys keep defaults.
        assert_eq!(cfg.model_path, "models/audio/yamnet.onnx");
    }

    #[test]
    fn test_engine_disabled_by_default() {
        let engine = AudioAiEngine::from_config(&AudioAiConfig::default());
        assert!(!engine.is_active());
        assert_eq!(engine.inactive_reason(), "disabled by configuration");
    }

    #[test]
    fn test_engine_enabled_but_model_missing_fails_open() {
        let cfg = AudioAiConfig {
            enabled: true,
            model_path: "definitely-missing.onnx".into(),
            ..AudioAiConfig::default()
        };
        let engine = AudioAiEngine::from_config(&cfg);
        assert!(!engine.is_active(), "missing model must not fake activity");
    }

    #[test]
    fn test_rms_dbfs_silence_and_full_scale() {
        assert!(rms_dbfs(&[0.0; 512]) <= -95.0);
        let sine: Vec<f32> = (0..512)
            .map(|i| ((2.0 * std::f64::consts::PI * i as f64 / 512.0).sin()) as f32)
            .collect();
        assert!((rms_dbfs(&sine) - (-3.01)).abs() < 0.2);
    }

    #[tokio::test]
    async fn test_worker_noop_when_inactive() {
        let engine = AudioAiEngine::from_config(&AudioAiConfig::default());
        let (tx, rx) = broadcast::channel(4);
        engine.spawn_worker(rx);
        // No panic, worker exits immediately; nothing to observe.
        drop(tx);
    }
}
