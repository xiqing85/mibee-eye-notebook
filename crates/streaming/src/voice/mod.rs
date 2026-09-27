//! Voice interaction engine: streaming keyword spotting (wake word) +
//! offline paraformer transcription of the utterance that follows.
//!
//! Wake → capture → transcribe is the minimal closed loop of the voice
//! interaction plan; the LLM dialogue and TTS playback stages consume the
//! same [`VoiceEvent`]. The engine is fail-open (missing models or a build
//! without the `voice` feature leave it inactive) and reuses the always-on
//! 16 kHz monitor stream, so it never touches the audio hardware itself.

use capture::audio_monitor::AudioChunk;
use serde::{Deserialize, Serialize};
#[cfg(feature = "voice")]
use std::sync::Arc;
#[cfg(feature = "voice")]
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::info;
#[cfg(feature = "voice")]
use tracing::warn;

/// One completed voice interaction: the wake word that fired and the
/// transcript of the utterance captured after it (SPEC appendix A
/// notebook dialect: SSE `voice_transcript`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceEvent {
    /// The detected wake word (e.g. `"小蜜蜂"`).
    pub keyword: String,
    /// Transcript of the captured utterance (may be empty on silence).
    pub transcript: String,
    /// Unix epoch milliseconds captured at fire time.
    pub timestamp_ms: u64,
}

/// `[voice]` configuration section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub enabled: bool,
    /// KWS model files (zipformer transducer trio + tokens).
    pub kws_encoder: String,
    pub kws_decoder: String,
    pub kws_joiner: String,
    pub kws_tokens: String,
    /// Keywords file (`word :boost #threshold` per line).
    pub keywords_file: String,
    /// Wake-word sensitivity: lower threshold = easier to fire.
    pub keywords_threshold: f32,
    pub keywords_score: f32,
    /// Offline recognizer (paraformer zh int8).
    pub paraformer_model: String,
    pub paraformer_tokens: String,
    /// Seconds of audio captured after a wake word before transcription.
    pub capture_secs: u32,
    /// Inference threads (the target hosts are small).
    pub num_threads: i32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kws_encoder: "models/voice/kws/encoder.int8.onnx".into(),
            kws_decoder: "models/voice/kws/decoder.int8.onnx".into(),
            kws_joiner: "models/voice/kws/joiner.int8.onnx".into(),
            kws_tokens: "models/voice/kws/tokens.txt".into(),
            keywords_file: "models/voice/kws/keywords.txt".into(),
            keywords_threshold: 0.25,
            keywords_score: 1.0,
            paraformer_model: "models/voice/paraformer/model.int8.onnx".into(),
            paraformer_tokens: "models/voice/paraformer/tokens.txt".into(),
            capture_secs: 4,
            num_threads: 1,
        }
    }
}

/// Inference backends (`voice`-feature builds only).
#[cfg(feature = "voice")]
struct VoiceInternals {
    kws: Arc<sherpa_onnx::KeywordSpotter>,
    recognizer: Arc<sherpa_onnx::OfflineRecognizer>,
}

/// Process-wide voice engine.
pub struct VoiceEngine {
    #[cfg_attr(not(feature = "voice"), allow(dead_code))]
    config: VoiceConfig,
    active: bool,
    inactive_reason: String,
    #[allow(dead_code)]
    event_tx: broadcast::Sender<VoiceEvent>,
    #[cfg(feature = "voice")]
    internals: Option<VoiceInternals>,
}

impl VoiceEngine {
    /// Build from config (fail-open on every load failure).
    #[must_use]
    pub fn from_config(config: &VoiceConfig) -> Self {
        let loaded = Self::load(config);
        match loaded {
            #[cfg(feature = "voice")]
            Ok(internals) => {
                info!(
                    keyword_file = %config.keywords_file,
                    capture_secs = config.capture_secs,
                    "voice: engine loaded"
                );
                let (event_tx, _) = broadcast::channel(16);
                Self {
                    config: config.clone(),
                    active: true,
                    inactive_reason: String::new(),
                    event_tx,
                    internals: Some(internals),
                }
            }
            #[cfg(not(feature = "voice"))]
            Ok(()) => unreachable!("non-voice load never succeeds"),
            Err(reason) => {
                info!(%reason, "voice: disabled");
                let (event_tx, _) = broadcast::channel(16);
                Self {
                    config: config.clone(),
                    active: false,
                    inactive_reason: format!("{reason:#}"),
                    event_tx,
                    #[cfg(feature = "voice")]
                    internals: None,
                }
            }
        }
    }

    #[cfg(feature = "voice")]
    fn load(config: &VoiceConfig) -> anyhow::Result<VoiceInternals> {
        if !config.enabled {
            anyhow::bail!("disabled by configuration");
        }
        let kws = build_kws(config).map_err(|e| {
            warn!(error = %e, "voice: KWS unavailable (voice stays disabled, fail-open)");
            anyhow::anyhow!("KWS model unavailable (see startup log)")
        })?;
        let recognizer = build_recognizer(config).map_err(|e| {
            warn!(error = %e, "voice: paraformer unavailable (voice stays disabled, fail-open)");
            anyhow::anyhow!("paraformer model unavailable (see startup log)")
        })?;
        Ok(VoiceInternals {
            kws: Arc::new(kws),
            recognizer: Arc::new(recognizer),
        })
    }

    #[cfg(not(feature = "voice"))]
    fn load(config: &VoiceConfig) -> anyhow::Result<()> {
        if !config.enabled {
            anyhow::bail!("disabled by configuration");
        }
        anyhow::bail!("built without the `voice` feature")
    }

    /// Whether the wake-word loop is running.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Why the engine is inactive (empty string when active).
    #[must_use]
    pub fn inactive_reason(&self) -> &str {
        &self.inactive_reason
    }

    /// Subscribe to completed voice interactions.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<VoiceEvent> {
        self.event_tx.subscribe()
    }

    /// Spawn the monitor-consuming worker (no-op when inactive).
    pub fn spawn_worker(
        &self,
        #[cfg_attr(not(feature = "voice"), allow(unused_mut))] mut rx: broadcast::Receiver<
            AudioChunk,
        >,
    ) -> tokio::task::JoinHandle<()> {
        #[cfg(feature = "voice")]
        let Some(internals) = self.internals.as_ref().map(|i| VoiceInternals {
            kws: Arc::clone(&i.kws),
            recognizer: Arc::clone(&i.recognizer),
        }) else {
            return tokio::spawn(async {});
        };
        #[cfg(feature = "voice")]
        {
            let capture_secs = self.config.capture_secs;
            let event_tx = self.event_tx.clone();
            info!("voice: wake-word worker started");
            tokio::spawn(async move {
                // The KWS stream is single-owner; the decode loop runs on
                // this task, the heavy paraformer pass on spawn_blocking.
                let kws_stream = internals.kws.create_stream();
                let mut captured: Vec<f32> = Vec::new();
                let mut keyword_pending: Option<String> = None;
                let mut capture_deadline = tokio::time::Instant::now();
                loop {
                    match rx.recv().await {
                        Ok(chunk) => {
                            let samples: Vec<f32> =
                                chunk.samples.iter().map(|&s| s as f32 / 32_768.0).collect();
                            if let Some(keyword) = &keyword_pending {
                                captured.extend_from_slice(&samples);
                                if tokio::time::Instant::now() >= capture_deadline {
                                    let keyword = keyword.clone();
                                    let recognizer = Arc::clone(&internals.recognizer);
                                    let waveform = std::mem::take(&mut captured);
                                    let tx = event_tx.clone();
                                    tokio::task::spawn_blocking(move || {
                                        let text = transcribe(&recognizer, &waveform);
                                        let now_ms = unix_now_ms();
                                        info!(
                                            keyword = %keyword,
                                            chars = text.chars().count(),
                                            "voice: transcript"
                                        );
                                        let _ = tx.send(VoiceEvent {
                                            keyword,
                                            transcript: text,
                                            timestamp_ms: now_ms,
                                        });
                                    });
                                    keyword_pending = None;
                                }
                                continue;
                            }
                            kws_stream.accept_waveform(16_000, &samples);
                            while internals.kws.is_ready(&kws_stream) {
                                internals.kws.decode(&kws_stream);
                            }
                            if let Some(hit) = internals.kws.get_result(&kws_stream)
                                && !hit.keyword.is_empty()
                            {
                                info!(keyword = %hit.keyword, "voice: wake word detected");
                                internals.kws.reset(&kws_stream);
                                captured.clear();
                                keyword_pending = Some(hit.keyword);
                                capture_deadline = tokio::time::Instant::now()
                                    + Duration::from_secs(u64::from(capture_secs.max(1)));
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "voice: worker lagged behind monitor");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                info!("voice: wake-word worker stopped");
            })
        }

        #[cfg(not(feature = "voice"))]
        {
            drop(rx);
            tokio::spawn(async {})
        }
    }
}

/// Offline-transcribe a 16 kHz mono waveform (empty string on failure).
#[cfg(feature = "voice")]
fn transcribe(recognizer: &sherpa_onnx::OfflineRecognizer, waveform: &[f32]) -> String {
    let stream = recognizer.create_stream();
    stream.accept_waveform(16_000, waveform);
    recognizer.decode(&stream);
    stream.get_result().map(|r| r.text).unwrap_or_default()
}

#[cfg(feature = "voice")]
fn build_kws(config: &VoiceConfig) -> anyhow::Result<sherpa_onnx::KeywordSpotter> {
    use sherpa_onnx::{KeywordSpotter, KeywordSpotterConfig};
    for (label, path) in [
        ("kws_encoder", &config.kws_encoder),
        ("kws_decoder", &config.kws_decoder),
        ("kws_joiner", &config.kws_joiner),
        ("kws_tokens", &config.kws_tokens),
        ("keywords_file", &config.keywords_file),
    ] {
        if !std::path::Path::new(path).exists() {
            anyhow::bail!("voice.{label}: file not found: {path}");
        }
    }
    let mut cfg = KeywordSpotterConfig::default();
    cfg.model_config.transducer.encoder = Some(config.kws_encoder.clone());
    cfg.model_config.transducer.decoder = Some(config.kws_decoder.clone());
    cfg.model_config.transducer.joiner = Some(config.kws_joiner.clone());
    cfg.model_config.tokens = Some(config.kws_tokens.clone());
    cfg.keywords_file = Some(config.keywords_file.clone());
    cfg.keywords_threshold = config.keywords_threshold;
    cfg.keywords_score = config.keywords_score;
    cfg.model_config.num_threads = config.num_threads;
    KeywordSpotter::create(&cfg).ok_or_else(|| anyhow::anyhow!("KeywordSpotter::create failed"))
}

#[cfg(feature = "voice")]
fn build_recognizer(config: &VoiceConfig) -> anyhow::Result<sherpa_onnx::OfflineRecognizer> {
    use sherpa_onnx::{
        OfflineModelConfig, OfflineParaformerModelConfig, OfflineRecognizer,
        OfflineRecognizerConfig,
    };
    for (label, path) in [
        ("paraformer_model", &config.paraformer_model),
        ("paraformer_tokens", &config.paraformer_tokens),
    ] {
        if !std::path::Path::new(path).exists() {
            anyhow::bail!("voice.{label}: file not found: {path}");
        }
    }
    let mut model = OfflineModelConfig::default();
    model.paraformer = OfflineParaformerModelConfig {
        model: Some(config.paraformer_model.clone()),
    };
    model.tokens = Some(config.paraformer_tokens.clone());
    model.num_threads = config.num_threads;
    let cfg = OfflineRecognizerConfig {
        model_config: model,
        ..Default::default()
    };
    OfflineRecognizer::create(&cfg)
        .ok_or_else(|| anyhow::anyhow!("OfflineRecognizer::create failed"))
}

#[cfg_attr(not(feature = "voice"), allow(dead_code))]
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Deterministic offline self-test: run the wake-word + transcription loop
/// over a WAV file and report what fired (`--selftest-voice`).
///
/// # Errors
///
/// File/parse errors, missing models, or a build without `voice`.
pub fn selftest_voice(path: &str) -> anyhow::Result<serde_json::Value> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?;
    #[cfg_attr(not(feature = "voice"), allow(unused_variables))]
    let wav = super::audio_ai::wav::parse_wav(&bytes)?;
    let mut config = VoiceConfig {
        enabled: true,
        ..VoiceConfig::default()
    };
    if let Ok(t) = std::env::var("VOICE_SELFTEST_THRESHOLD")
        && let Ok(t) = t.parse::<f32>()
    {
        config.keywords_threshold = t;
    }
    #[cfg(feature = "voice")]
    {
        let kws = build_kws(&config)?;
        let recognizer = build_recognizer(&config)?;
        let mut resampler = capture::audio_monitor::LinearResampler::new(
            f64::from(wav.sample_rate) / f64::from(capture::audio_monitor::TARGET_RATE),
        );
        let samples = resampler.process(&wav.samples);
        let mut hits: Vec<serde_json::Value> = Vec::new();
        let mut full: Vec<f32> = Vec::new();
        let stream = kws.create_stream();
        let mut pos = 0_usize;
        let capture_samples = (u64::from(config.capture_secs)
            * u64::from(capture::audio_monitor::TARGET_RATE))
            as usize;
        while pos < samples.len() {
            let take = (samples.len() - pos).min(512);
            let chunk = &samples[pos..pos + take];
            pos += take;
            full.extend_from_slice(chunk);
            stream.accept_waveform(16_000, chunk);
            while kws.is_ready(&stream) {
                kws.decode(&stream);
            }
            if let Some(hit) = kws.get_result(&stream)
                && !hit.keyword.is_empty()
            {
                let at_s = (pos as f64 / f64::from(capture::audio_monitor::TARGET_RATE) * 100.0)
                    .round()
                    / 100.0;
                let end = (pos + capture_samples).min(samples.len());
                let utterance = &samples[pos..end];
                let transcript = transcribe(&recognizer, utterance);
                hits.push(serde_json::json!({
                    "keyword": hit.keyword,
                    "tokens": hit.tokens,
                    "json": hit.json,
                    "at_s": at_s,
                    "transcript": transcript,
                }));
                kws.reset(&stream);
                // Skip past the utterance so one wake word fires once.
                pos = end;
            }
        }
        let full_transcript = transcribe(&recognizer, &full);
        Ok(serde_json::json!({
            "file": path,
            "sample_rate": wav.sample_rate,
            "duration_s": (samples.len() as f64 / f64::from(capture::audio_monitor::TARGET_RATE)).round(),
            "wake_words": hits,
            "full_transcript": full_transcript,
        }))
    }
    #[cfg(not(feature = "voice"))]
    {
        let _ = &bytes;
        anyhow::bail!("built without the `voice` feature")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_off() {
        let c = VoiceConfig::default();
        assert!(!c.enabled, "voice is opt-in");
        assert!(c.kws_tokens.contains("kws"));
        assert_eq!(c.capture_secs, 4);
        assert!((c.keywords_threshold - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn engine_disabled_by_default() {
        let e = VoiceEngine::from_config(&VoiceConfig::default());
        assert!(!e.is_active());
        assert_eq!(e.inactive_reason(), "disabled by configuration");
    }

    #[test]
    fn engine_enabled_but_models_missing_fails_open() {
        let e = VoiceEngine::from_config(&VoiceConfig {
            enabled: true,
            ..VoiceConfig::default()
        });
        assert!(!e.is_active(), "missing models must not fake activity");
    }
}
