//! CRNN text recognition: crop normalization, inference, greedy CTC
//! decoding with the PP-OCR dictionary.

use anyhow::{Result, bail};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;

/// PP-OCR rec input height (v3/v4/v5 all use 48).
const REC_HEIGHT: u32 = 48;
/// Widest crop we feed the recognizer.
const REC_MAX_WIDTH: u32 = 960;
/// CTC class layout: 0 = blank, 1..=dict.len() = dict chars,
/// dict.len()+1 = space.
const SPACE_CLASS_OFFSET: usize = 1;

pub(super) struct RecModel {
    session: Mutex<Session>,
    input_name: String,
    output_name: String,
    dict: Vec<String>,
}

impl RecModel {
    pub(super) fn new(path: &str, dict: Vec<String>) -> Result<Self> {
        if !Path::new(path).exists() {
            bail!("rec model not found: {path}");
        }
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("rec session builder: {e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("rec threads: {e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow::anyhow!("load {path}: {e}"))?;
        let (input_name, output_name) = {
            let input = session
                .inputs()
                .first()
                .ok_or_else(|| anyhow::anyhow!("rec model has no inputs"))?;
            let output = session
                .outputs()
                .first()
                .ok_or_else(|| anyhow::anyhow!("rec model has no outputs"))?;
            (input.name().to_string(), output.name().to_string())
        };
        Ok(Self {
            session: Mutex::new(session),
            input_name,
            output_name,
            dict,
        })
    }

    /// Recognize one RGB crop; `None` when the crop decodes to nothing.
    pub(super) fn recognize(&self, rgb: &[u8], w: u32, h: u32) -> Result<Option<(String, f32)>> {
        if w == 0 || h == 0 {
            return Ok(None);
        }
        // Resize to height 48, keep the aspect ratio. The tensor is NCHW —
        // fill plane-major (all B, then all G, then all R), NOT interleaved.
        let nw = ((REC_HEIGHT as f64 * f64::from(w) / f64::from(h)).round() as u32)
            .clamp(16, REC_MAX_WIDTH);
        let plane = (nw * REC_HEIGHT) as usize;
        let mut input = vec![0_f32; plane * 3];
        for y in 0..REC_HEIGHT {
            let sy = (y as f64 / f64::from(REC_HEIGHT) * f64::from(h)) as u32;
            for x in 0..nw {
                let sx = (x as f64 / f64::from(nw) * f64::from(w)) as u32;
                let base = ((sy.min(h - 1) * w + sx.min(w - 1)) * 3) as usize;
                let out = (y * nw + x) as usize;
                input[out] = (f32::from(rgb[base]) / 255.0 - 0.5) / 0.5;
                input[plane + out] = (f32::from(rgb[base + 1]) / 255.0 - 0.5) / 0.5;
                input[2 * plane + out] = (f32::from(rgb[base + 2]) / 255.0 - 0.5) / 0.5;
            }
        }
        let shape = vec![1_i64, 3, REC_HEIGHT as i64, nw as i64];
        let tensor =
            Tensor::from_array((shape, input)).map_err(|e| anyhow::anyhow!("rec tensor: {e}"))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("rec session poisoned"))?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("rec inference: {e}"))?;
        let output = outputs
            .get(&self.output_name)
            .ok_or_else(|| anyhow::anyhow!("rec output missing"))?;
        let (dims, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("rec extract: {e}"))?;
        let n_classes = *dims.last().unwrap_or(&0) as usize;
        if n_classes == 0 || data.len() % n_classes != 0 {
            bail!("bad rec output: {dims:?}");
        }
        let steps = data.len() / n_classes;
        Ok(decode_ctc(data, steps, n_classes, &self.dict))
    }
}

/// Greedy CTC decode: per-step argmax, drop blanks and repeats.
fn decode_ctc(
    data: &[f32],
    steps: usize,
    n_classes: usize,
    dict: &[String],
) -> Option<(String, f32)> {
    let mut text = String::new();
    let mut probs: Vec<f32> = Vec::new();
    let mut last_class = 0_usize;
    for step in 0..steps {
        let row = &data[step * n_classes..(step + 1) * n_classes];
        let (mut best, mut best_v) = (0_usize, f32::MIN);
        for (cls, &v) in row.iter().enumerate() {
            if v > best_v {
                best_v = v;
                best = cls;
            }
        }
        if best != 0 && best != last_class {
            probs.push(best_v.max(0.0));
            if best == dict.len() + SPACE_CLASS_OFFSET {
                text.push(' ');
            } else if let Some(ch) = dict.get(best - SPACE_CLASS_OFFSET) {
                text.push_str(ch);
            }
        }
        last_class = best;
    }
    if text.trim().is_empty() {
        return None;
    }
    let mean = probs.iter().sum::<f32>() / probs.len().max(1) as f32;
    Some((text, mean))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Vec<String> {
        vec!["A".to_string(), "B".to_string(), "9".to_string()]
    }

    /// steps × classes logits; class 0 = blank.
    fn logits(steps: usize, classes: usize, picks: &[(usize, usize, f32)]) -> Vec<f32> {
        let mut v = vec![0.0_f32; steps * classes];
        for &(s, c, p) in picks {
            v[s * classes + c] = p;
        }
        v
    }

    #[test]
    fn ctc_basic_decode() {
        // "AAB9" with blanks between repeats; A repeated across steps
        // separated by a blank must produce two A's.
        let d = dict();
        let data = logits(
            7,
            4,
            &[
                (0, 1, 0.9), // A
                (1, 0, 0.8), // blank
                (2, 1, 0.7), // A (after blank → new char)
                (3, 2, 0.6), // B
                (4, 0, 0.9), // blank
                (5, 3, 0.8), // 9
                (6, 0, 0.9), // blank
            ],
        );
        let (text, score) = decode_ctc(&data, 7, 4, &d).expect("some");
        assert_eq!(text, "AAB9");
        assert!(score > 0.0);
    }

    #[test]
    fn ctc_space_class_and_empty() {
        let d = dict();
        // space class = dict.len()+1 = 4
        let data = logits(3, 5, &[(0, 1, 0.9), (1, 4, 0.8), (2, 2, 0.7)]);
        let (text, _) = decode_ctc(&data, 3, 5, &d).expect("some");
        assert_eq!(text, "A B");
        // All blanks → None.
        let data = logits(3, 5, &[]);
        assert!(decode_ctc(&data, 3, 5, &d).is_none());
    }
}
