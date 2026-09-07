//! ONNX Runtime detector (NanoDet-Plus), loaded dynamically.
//!
//! The `ort` crate is configured with `load-dynamic`, so the binary itself
//! stays portable: at runtime it dlopens `libonnxruntime.so`, searched via
//! the `ORT_DYLIB_PATH` environment variable or the system library paths
//! (same deployment model as the raspi cameras).

use super::postprocess::{postprocess, scale_detections_to_frame};
use super::preprocess::preprocess_jpeg;
use super::{AiDetector, Detection};
use anyhow::{Result, bail};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;

/// Pre-NMS confidence filter applied inside post-processing. The configured
/// `confidence_threshold` is applied by the worker after `detect()` returns;
/// this lower pre-filter only trims obviously-empty grid points to keep NMS
/// cheap.
const PRE_NMS_CONFIDENCE: f32 = 0.05;

/// ONNX Runtime-based AI detector.
#[derive(Debug)]
pub struct OrtDetector {
    /// Path to the ONNX model file (also serves as the model identifier).
    model_path: String,
    /// ONNX Runtime session. Wrapped in a mutex because `run()` takes
    /// `&mut self`.
    session: Mutex<Session>,
    /// Name of the model's input tensor.
    input_name: String,
    /// Name of the model's output tensor.
    output_name: String,
    /// Model input width (read from the session's input shape).
    input_width: u32,
    /// Model input height (read from the session's input shape).
    input_height: u32,
}

impl OrtDetector {
    /// Create a detector from an ONNX model file.
    ///
    /// # Errors
    ///
    /// Fails if the model file is missing, the ONNX Runtime library cannot
    /// be loaded, or the model's input/output metadata is unusable. Callers
    /// treat this as "AI disabled" (fail-open), never fatal.
    pub fn new(model_path: &str) -> Result<Self> {
        if !Path::new(model_path).exists() {
            bail!("ONNX model file not found: {model_path}");
        }

        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("failed to create ONNX session builder: {e}"))?
            .with_intra_threads(2)
            .map_err(|e| anyhow::anyhow!("failed to set intra-op threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("failed to load ONNX model {model_path}: {e}"))?;

        let input = session
            .inputs()
            .first()
            .ok_or_else(|| anyhow::anyhow!("ONNX model has no inputs"))?;
        let output = session
            .outputs()
            .first()
            .ok_or_else(|| anyhow::anyhow!("ONNX model has no outputs"))?;

        let input_name = input.name().to_string();
        let output_name = output.name().to_string();

        // NCHW layout: [batch, channels, height, width].
        let shape = input
            .dtype()
            .tensor_shape()
            .ok_or_else(|| anyhow::anyhow!("ONNX model input is not a tensor"))?;
        let input_height = shape.get(2).copied().unwrap_or(-1);
        let input_width = shape.get(3).copied().unwrap_or(-1);
        if input_height <= 0 || input_width <= 0 {
            bail!("ONNX model input shape has dynamic dimensions: {shape:?}");
        }

        Ok(Self {
            model_path: model_path.to_string(),
            session: Mutex::new(session),
            input_name,
            output_name,
            input_width: input_width as u32,
            input_height: input_height as u32,
        })
    }
}

impl AiDetector for OrtDetector {
    fn detect(&self, jpeg: &[u8]) -> Result<Vec<Detection>> {
        // 1. JPEG → resized + normalized NCHW f32 (plus the frame's native
        //    dimensions — the bbox output coordinate space, SPEC v1 §4.6).
        let (input, frame_w, frame_h) = preprocess_jpeg(jpeg, self.input_width, self.input_height)?;

        // 2. Build the input tensor.
        let shape = vec![1_i64, 3, self.input_height as i64, self.input_width as i64];
        let tensor = Tensor::from_array((shape, input))
            .map_err(|e| anyhow::anyhow!("failed to build input tensor: {e}"))?;

        // 3. Run inference under the session lock.
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("ONNX session mutex poisoned"))?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("ONNX inference error: {e}"))?;
        let output = outputs
            .get(&self.output_name)
            .ok_or_else(|| anyhow::anyhow!("ONNX model produced no outputs"))?;

        // 4. Extract the flat f32 tensor and run NanoDet GFL post-processing.
        let (_tensor_shape, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("failed to extract output tensor: {e}"))?;

        // 5. Map bboxes from model-input pixels back to video-frame pixels.
        let detections = postprocess(data, PRE_NMS_CONFIDENCE)?;
        Ok(scale_detections_to_frame(
            detections,
            self.input_width,
            self.input_height,
            frame_w,
            frame_h,
        ))
    }

    fn model_name(&self) -> &str {
        &self.model_path
    }
}

impl OrtDetector {
    /// Square model input size in pixels (registry metadata, SPEC §4.6).
    pub fn input_size(&self) -> u32 {
        self.input_width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constructor_error_missing_file() {
        let result = OrtDetector::new("nonexistent_model.onnx");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
