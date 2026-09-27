//! DBNet text detection: preprocessing, inference, and pure-Rust
//! post-processing (binarize → connected components → boxes).

use anyhow::{Result, bail};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

pub(super) struct DetModel {
    session: Mutex<Session>,
    input_name: String,
    output_name: String,
}

impl DetModel {
    pub(super) fn new(path: &str) -> Result<Self> {
        if !Path::new(path).exists() {
            bail!("det model not found: {path}");
        }
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("det session builder: {e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("det threads: {e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow::anyhow!("load {path}: {e}"))?;
        let (input_name, output_name) = {
            let input = session
                .inputs()
                .first()
                .ok_or_else(|| anyhow::anyhow!("det model has no inputs"))?;
            let output = session
                .outputs()
                .first()
                .ok_or_else(|| anyhow::anyhow!("det model has no outputs"))?;
            (input.name().to_string(), output.name().to_string())
        };
        Ok(Self {
            session: Mutex::new(session),
            input_name,
            output_name,
        })
    }

    /// Detect text boxes; returns `[x, y, w, h]` crops in the ORIGINAL
    /// image's pixel coordinates (expanded slightly for the recognizer).
    pub(super) fn detect(
        &self,
        rgb: &[u8],
        w: u32,
        h: u32,
        max_side: u32,
        threshold: f32,
    ) -> Result<Vec<[u32; 4]>> {
        // 1. Resize (nearest) so the longest side <= max_side, dims % 32.
        let scale = f64::from(max_side.min(w.max(h))) / f64::from(w.max(h));
        let nw = (((w as f64 * scale).round() as u32).max(32) + 31) & !31;
        let nh = (((h as f64 * scale).round() as u32).max(32) + 31) & !31;
        // NCHW plane-major fill (all B, then all G, then all R).
        let plane = (nw * nh) as usize;
        let mut input = vec![0_f32; plane * 3];
        for y in 0..nh {
            let sy = (y as f64 / f64::from(nh) * f64::from(h)) as u32;
            for x in 0..nw {
                let sx = (x as f64 / f64::from(nw) * f64::from(w)) as u32;
                let base = ((sy.min(h - 1) * w + sx.min(w - 1)) * 3) as usize;
                let out = (y * nw + x) as usize;
                // Paddle models are BGR-trained.
                input[out] = (f32::from(rgb[base + 2]) / 255.0 - MEAN[0]) / STD[0];
                input[plane + out] = (f32::from(rgb[base + 1]) / 255.0 - MEAN[1]) / STD[1];
                input[2 * plane + out] = (f32::from(rgb[base]) / 255.0 - MEAN[2]) / STD[2];
            }
        }

        // 2. Inference.
        let shape = vec![1_i64, 3, nh as i64, nw as i64];
        let tensor =
            Tensor::from_array((shape, input)).map_err(|e| anyhow::anyhow!("det tensor: {e}"))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("det session poisoned"))?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("det inference: {e}"))?;
        let output = outputs
            .get(&self.output_name)
            .ok_or_else(|| anyhow::anyhow!("det output missing"))?;
        let (dims, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("det extract: {e}"))?;

        // 3. Post-process: the output is [1, 1, mh, mw] (already sigmoid).
        // Output is [1, 1, mh, mw] or [1, mh, mw]; take the last two dims.
        if dims.len() < 2 {
            bail!("unexpected det output shape {dims:?}");
        }
        let mh = dims[dims.len() - 2].max(1) as u32;
        let mw = dims[dims.len() - 1].max(1) as u32;
        if data.len() < (mw * mh) as usize {
            bail!("det output too small: {} < {mw}x{mh}", data.len());
        }
        // Paddle exports vary: some ship post-sigmoid maps, some raw logits.
        let data_owned: Vec<f32>;
        let data: &[f32] = if data.iter().any(|v| *v > 1.5) {
            data_owned = data.iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect();
            &data_owned
        } else {
            data
        };
        let boxes = components_to_boxes(data, mw, mh, threshold);
        // Map back to original coordinates + expand for the recognizer.
        let sx = f64::from(w) / f64::from(mw);
        let sy = f64::from(h) / f64::from(mh);
        Ok(boxes
            .into_iter()
            .map(|(x, y, bw, bh)| {
                let pad_x = (bw as f64 * 0.15).max(2.0);
                let pad_y = (bh as f64 * 0.35).max(2.0);
                let x0 = ((x as f64 - pad_x) * sx).max(0.0).floor() as u32;
                let y0 = ((y as f64 - pad_y) * sy).max(0.0).floor() as u32;
                let x1 = (((x + bw) as f64 + pad_x) * sx).min(f64::from(w)).ceil() as u32;
                let y1 = (((y + bh) as f64 + pad_y) * sy).min(f64::from(h)).ceil() as u32;
                [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)]
            })
            .collect())
    }
}

/// Binarize the probability map, find 8-connected components, return their
/// bounding boxes (map coordinates) with tiny blobs filtered out.
fn components_to_boxes(prob: &[f32], w: u32, h: u32, threshold: f32) -> Vec<(u32, u32, u32, u32)> {
    let n = (w * h) as usize;
    let mut visited = vec![false; n];
    let mut boxes = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    for start in 0..n {
        if visited[start] || prob[start] < threshold {
            continue;
        }
        // BFS the component.
        visited[start] = true;
        queue.clear();
        queue.push_back(start);
        let (mut min_x, mut min_y) = (u32::MAX, u32::MAX);
        let (mut max_x, mut max_y) = (0_u32, 0_u32);
        let mut area = 0_u32;
        while let Some(idx) = queue.pop_front() {
            let x = (idx as u32) % w;
            let y = (idx as u32) / w;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            area += 1;
            // 8-neighborhood.
            for (dx, dy) in [
                (-1_i64, -1_i64),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let nx = i64::from(x) + dx;
                let ny = i64::from(y) + dy;
                if nx < 0 || ny < 0 || nx >= i64::from(w) || ny >= i64::from(h) {
                    continue;
                }
                let nidx = (ny as u32 * w + nx as u32) as usize;
                if !visited[nidx] && prob[nidx] >= threshold {
                    visited[nidx] = true;
                    queue.push_back(nidx);
                }
            }
        }
        // Text lines are wider than tall and not specks.
        let bw = max_x.saturating_sub(min_x) + 1;
        let bh = max_y.saturating_sub(min_y) + 1;
        if area >= 6 && bw >= 4 && bh >= 2 {
            boxes.push((min_x, min_y, bw, bh));
        }
    }
    boxes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_find_two_blobs() {
        // 10x5 map, one 4x2 blob on the left, one 3x1 on the right.
        let (w, h) = (10_u32, 5_u32);
        let mut prob = vec![0.0_f32; (w * h) as usize];
        for y in 1..=2 {
            for x in 1..=4 {
                prob[(y * w + x) as usize] = 0.9;
            }
        }
        for y in 3..=4 {
            for x in 6..=9 {
                prob[(y * w + x) as usize] = 0.8;
            }
        }
        let mut boxes = components_to_boxes(&prob, w, h, 0.3);
        boxes.sort();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0], (1, 1, 4, 2));
        assert_eq!(boxes[1], (6, 3, 4, 2));
    }

    #[test]
    fn specks_filtered() {
        let (w, h) = (8_u32, 8_u32);
        let mut prob = vec![0.0_f32; (w * h) as usize];
        prob[9] = 0.9; // single pixel
        assert!(components_to_boxes(&prob, w, h, 0.3).is_empty());
    }
}
