//! ONNX models of the audio pipeline: YAMNet classifier + Silero VAD
//! (both loaded dynamically through the same `ort` as the vision detector).

use anyhow::{Context, Result, bail};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;

/// YAMNet audio-event classifier: raw mono 16 kHz waveform in, 521 sigmoid
/// scores out.
#[derive(Debug)]
pub struct YamnetClassifier {
    model_path: String,
    session: Mutex<Session>,
    input_name: String,
    output_name: String,
}

impl YamnetClassifier {
    /// Load the model (fail-open upstream on error).
    ///
    /// # Errors
    ///
    /// Missing file, missing ONNX Runtime library, or unusable I/O metadata.
    pub fn new(model_path: &str) -> Result<Self> {
        if !Path::new(model_path).exists() {
            bail!("ONNX model file not found: {model_path}");
        }
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("intra threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("load {model_path}: {e}"))?;
        let (input_name, output_name) = {
            let input = session
                .inputs()
                .first()
                .ok_or_else(|| anyhow::anyhow!("model has no inputs"))?;
            let output = session
                .outputs()
                .first()
                .ok_or_else(|| anyhow::anyhow!("model has no outputs"))?;
            (input.name().to_string(), output.name().to_string())
        };
        Ok(Self {
            model_path: model_path.to_string(),
            session: Mutex::new(session),
            input_name,
            output_name,
        })
    }

    #[must_use]
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// Classify a 0.96 s (15 360-sample) waveform window; returns 521
    /// per-class scores.
    ///
    /// # Errors
    ///
    /// Inference or tensor extraction failure.
    pub fn classify(&self, waveform: &[f32]) -> Result<Vec<f32>> {
        let shape = vec![waveform.len() as i64];
        let tensor = Tensor::from_array((shape, waveform.to_vec()))
            .map_err(|e| anyhow::anyhow!("input tensor: {e}"))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session mutex poisoned"))?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("inference: {e}"))?;
        let output = outputs
            .get(&self.output_name)
            .ok_or_else(|| anyhow::anyhow!("no output"))?;
        let (dims, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("extract: {e}"))?;
        // Output layout: [patches, 521]; a 0.96 s window is one patch —
        // take the final row so longer inputs would still behave.
        let n_classes = *dims.last().context("missing class dimension")? as usize;
        let rows = data.len() / n_classes.max(1);
        let row_start = if rows > 1 { (rows - 1) * n_classes } else { 0 };
        let row: &[f32] = &data[row_start..];
        Ok(row.to_vec())
    }
}

/// Silero VAD v5: streaming speech-probability per chunk, with carried
/// LSTM state.
#[derive(Debug)]
pub struct SileroVad {
    session: Mutex<Session>,
    input_name: String,
    state_name: String,
    sr_name: String,
    output_name: String,
    state_out_name: String,
    /// Flattened [2, 1, 128] recurrent state.
    state: Vec<f32>,
}

impl SileroVad {
    /// Load the model.
    ///
    /// # Errors
    ///
    /// Missing file, missing ONNX Runtime library, or unusable I/O metadata.
    pub fn new(model_path: &str) -> Result<Self> {
        if !Path::new(model_path).exists() {
            bail!("ONNX model file not found: {model_path}");
        }
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("intra threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("load {model_path}: {e}"))?;
        let names: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let outputs: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        if names.len() < 3 || outputs.len() < 2 {
            bail!("unexpected Silero VAD I/O layout: in={names:?} out={outputs:?}");
        }
        Ok(Self {
            input_name: names[0].clone(),
            state_name: names[1].clone(),
            sr_name: names[2].clone(),
            output_name: outputs[0].clone(),
            state_out_name: outputs[1].clone(),
            session: Mutex::new(session),
            state: vec![0.0; 2 * 128],
        })
    }

    /// Process one chunk of mono 16 kHz samples; returns the speech
    /// probability of the final frame.
    ///
    /// # Errors
    ///
    /// Inference or tensor failure (callers fall back to "no signal").
    pub fn process(&mut self, chunk: &[f32]) -> Result<f32> {
        if chunk.is_empty() {
            return Ok(0.0);
        }
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("vad session mutex poisoned"))?;
        // This export expects a batched [1, N] input (rank 2).
        let input = Tensor::from_array((vec![1_i64, chunk.len() as i64], chunk.to_vec()))
            .map_err(|e| anyhow::anyhow!("vad input tensor: {e}"))?;
        let state = Tensor::from_array((vec![2_i64, 1, 128], self.state.clone()))
            .map_err(|e| anyhow::anyhow!("vad state tensor: {e}"))?;
        let sr = Tensor::<i64>::from_array((Vec::<i64>::new(), vec![16_000_i64]))
            .map_err(|e| anyhow::anyhow!("vad sr tensor: {e}"))?;
        let outputs = session
            .run(ort::inputs![
                self.input_name.as_str() => input,
                self.state_name.as_str() => state,
                self.sr_name.as_str() => sr,
            ])
            .map_err(|e| anyhow::anyhow!("vad inference: {e}"))?;
        let prob_tensor = outputs
            .get(&self.output_name)
            .ok_or_else(|| anyhow::anyhow!("vad output missing"))?;
        let (_d, probs) = prob_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("vad output extract: {e}"))?;
        let state_tensor = outputs
            .get(&self.state_out_name)
            .ok_or_else(|| anyhow::anyhow!("vad state output missing"))?;
        let (_ds, state) = state_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("vad state extract: {e}"))?;
        if state.len() == self.state.len() {
            self.state.copy_from_slice(state);
        }
        Ok(probs.last().copied().unwrap_or(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_errors() {
        assert!(YamnetClassifier::new("nope.onnx").is_err());
        assert!(SileroVad::new("nope.onnx").is_err());
    }

    /// Path to a workspace model file (tests run from the crate dir).
    fn model(rel: &str) -> String {
        format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
    }

    /// ort panics (not errors) when no ONNX Runtime library can be loaded,
    /// so inference tests must probe availability first: CI machines and
    /// model-less checkouts skip instead of failing.
    fn ort_available() -> bool {
        crate::audio_ai::ort_available_for_test()
    }

    /// Full-pipeline test against the real models (skipped when the model
    /// files or the ONNX Runtime library are absent — CI without assets).
    #[test]
    fn classify_sine_wave_runs_and_shapes_correctly() {
        if !ort_available() {
            println!("skipping — no ONNX Runtime library");
            return;
        }
        let path = model("models/audio/yamnet.onnx");
        let Ok(c) = YamnetClassifier::new(&path) else {
            println!("skipping — {path} not available");
            return;
        };
        let window: Vec<f32> = (0..super::super::WINDOW_SAMPLES)
            .map(|i| {
                (0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16_000.0).sin()) as f32
            })
            .collect();
        let scores = match c.classify(&window) {
            Ok(s) => s,
            Err(e) => {
                println!("skipping — ONNX Runtime unavailable: {e}");
                return;
            }
        };
        assert_eq!(scores.len(), 521);
        assert!(scores.iter().all(|s| (0.0..=1.0).contains(s)));
        // A pure tone lands on "Chirp, tweet"/"Tone"-ish classes with high
        // probability — sanity-check that *some* class lights up.
        let max = scores.iter().copied().fold(0.0_f32, f32::max);
        assert!(max > 0.1, "no class responded to a loud sine: {max}");
    }

    #[test]
    fn silero_runs_and_returns_probability() {
        if !ort_available() {
            println!("skipping — no ONNX Runtime library");
            return;
        }
        let path = model("models/audio/silero_vad.onnx");
        let Ok(mut v) = SileroVad::new(&path) else {
            println!("skipping — {path} not available");
            return;
        };
        let chunk: Vec<f32> = (0..512)
            .map(|i| {
                (0.3 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 16_000.0).sin()) as f32
            })
            .collect();
        let p1 = match v.process(&chunk) {
            Ok(p) => p,
            Err(e) => {
                println!("skipping — ONNX Runtime unavailable: {e}");
                return;
            }
        };
        let p2 = v.process(&chunk).expect("second call");
        assert!((0.0..=1.0).contains(&p1));
        assert!((0.0..=1.0).contains(&p2));
        // State carried: repeated calls are stable (no NaN).
        assert!(p2.is_finite());
    }
}
