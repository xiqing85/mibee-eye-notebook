//! TTS playback via the sherpa-onnx CLI as a subprocess (GPL isolation).
//!
//! The sherpa-onnx-offline-tts binary statically links espeak-ng
//! (GPL-3.0); running it as a child process keeps that obligation out of
//! the Apache-2.0 binary. `speak()` synthesizes to a temp WAV and plays it
//! through `aplay` (present on all our target hosts) — two short-lived
//! subprocesses per utterance, no daemons.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `[tts]` configuration section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsConfig {
    pub enabled: bool,
    /// sherpa-onnx-offline-tts binary path.
    pub binary: String,
    /// vits-melo-tts-zh_en assets.
    pub model: String,
    pub lexicon: String,
    pub tokens: String,
    pub dict_dir: String,
    /// Number/date normalization FSTs (comma-joined when both exist).
    pub rule_fsts: String,
    /// Playback command (`aplay -q`); empty = synthesize only.
    pub player: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            binary: "tmp/sherpa-libs/tools-bin/bin/sherpa-onnx-offline-tts".into(),
            model: "models/voice/melo/model.onnx".into(),
            lexicon: "models/voice/melo/lexicon.txt".into(),
            tokens: "models/voice/melo/tokens.txt".into(),
            dict_dir: "models/voice/melo/dict".into(),
            rule_fsts: "models/voice/melo/number.fst,models/voice/melo/date.fst".into(),
            player: "aplay -q".into(),
        }
    }
}

/// TTS engine (fail-open: missing binary/model leaves it inactive).
#[derive(Debug)]
pub struct TtsEngine {
    config: TtsConfig,
    active: bool,
    inactive_reason: String,
}

impl TtsEngine {
    /// Build from config (fail-open).
    #[must_use]
    pub fn from_config(config: &TtsConfig) -> Self {
        let reason = Self::check(config);
        if reason.is_empty() {
            tracing::info!(model = %config.model, "tts: engine ready");
        } else {
            tracing::info!(%reason, "tts: disabled");
        }
        Self {
            config: config.clone(),
            active: reason.is_empty(),
            inactive_reason: reason,
        }
    }

    fn check(config: &TtsConfig) -> String {
        if !config.enabled {
            return "disabled by configuration".into();
        }
        for (label, path) in [
            ("tts.binary", &config.binary),
            ("tts.model", &config.model),
            ("tts.lexicon", &config.lexicon),
            ("tts.tokens", &config.tokens),
        ] {
            if !std::path::Path::new(path).exists() {
                return format!("{label}: file not found: {path}");
            }
        }
        String::new()
    }

    /// Whether TTS is usable.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Why TTS is inactive (empty string when active).
    #[must_use]
    pub fn inactive_reason(&self) -> &str {
        &self.inactive_reason
    }

    /// Synthesize (and play, when a player is configured) one utterance.
    /// Blocking — call from `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Inactive engine or subprocess failure.
    pub fn speak(&self, text: &str) -> anyhow::Result<PathBuf> {
        if !self.active {
            anyhow::bail!("tts inactive: {}", self.inactive_reason);
        }
        if text.trim().is_empty() {
            anyhow::bail!("tts: empty text");
        }
        let out = std::env::temp_dir().join(format!("mibee-tts-{}.wav", std::process::id()));
        let mut cmd = std::process::Command::new(&self.config.binary);
        cmd.arg(format!("--vits-model={}", self.config.model))
            .arg(format!("--vits-lexicon={}", self.config.lexicon))
            .arg(format!("--vits-tokens={}", self.config.tokens))
            .arg(format!("--vits-dict-dir={}", self.config.dict_dir))
            .arg(format!("--tts-rule-fsts={}", self.config.rule_fsts))
            .arg("--num-threads=1")
            .arg(format!("--output-filename={}", out.display()))
            .arg(text);
        let status = cmd
            .status()
            .map_err(|e| anyhow::anyhow!("spawn tts: {e}"))?;
        if !status.success() {
            anyhow::bail!("tts subprocess failed: {status}");
        }
        if !self.config.player.is_empty() && !self.config.player.trim().is_empty() {
            let mut parts = self.config.player.split_whitespace();
            let Some(prog) = parts.next() else {
                return Ok(out);
            };
            let play = std::process::Command::new(prog)
                .args(parts)
                .arg(&out)
                .status();
            if let Err(e) = play {
                tracing::warn!(error = %e, "tts: playback failed (file kept at {})", out.display());
            }
        }
        Ok(out)
    }
}

/// Deterministic offline self-test (`--selftest-tts`): synthesize one
/// utterance, report the WAV path/size.
///
/// # Errors
///
/// Inactive engine or subprocess failure.
pub fn selftest_tts(text: &str) -> anyhow::Result<serde_json::Value> {
    let mut config = TtsConfig {
        enabled: true,
        player: String::new(), // self-test does not play audio
        ..TtsConfig::default()
    };
    // Deployment hosts keep the TTS binary elsewhere than the workstation
    // default — allow pointing the self-test at it without editing the
    // config default (same pattern as VOICE_SELFTEST_THRESHOLD).
    if let Ok(bin) = std::env::var("MIBEE_TTS_BIN")
        && !bin.is_empty()
    {
        config.binary = bin;
    }
    let engine = TtsEngine::from_config(&config);
    if !engine.is_active() {
        anyhow::bail!("tts inactive: {}", engine.inactive_reason());
    }
    let started = std::time::Instant::now();
    let wav = engine.speak(text)?;
    let size = std::fs::metadata(&wav).map(|m| m.len()).unwrap_or(0);
    Ok(serde_json::json!({
        "text": text,
        "wav": wav.display().to_string(),
        "bytes": size,
        "elapsed_s": (started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_off() {
        let c = TtsConfig::default();
        assert!(!c.enabled, "tts is opt-in");
        assert!(c.binary.contains("sherpa-onnx-offline-tts"));
    }

    #[test]
    fn engine_disabled_by_default() {
        let e = TtsEngine::from_config(&TtsConfig::default());
        assert!(!e.is_active());
        assert_eq!(e.inactive_reason(), "disabled by configuration");
    }

    #[test]
    fn engine_enabled_but_binary_missing_fails_open() {
        let e = TtsEngine::from_config(&TtsConfig {
            enabled: true,
            ..TtsConfig::default()
        });
        assert!(!e.is_active(), "missing assets must not fake activity");
    }
}
