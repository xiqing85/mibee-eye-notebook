//! On-device OCR (PP-OCRv5 mobile det + rec via ONNX Runtime).
//!
//! Detection is DBNet (sigmoid probability map), recognition is a CTC
//! CRNN — both post-processed in pure Rust (connected components, greedy
//! CTC). The engine is camera-agnostic: callers hand it an RGB frame (or
//! crop) and get text items back. Fail-open like the other AI engines.

pub mod det;
pub mod rec;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// One recognized text region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextItem {
    pub text: String,
    /// Mean recognizer confidence of the decoded characters.
    pub score: f32,
    /// Axis-aligned bounding box `[x, y, w, h]` in the input image's pixel
    /// coordinates.
    pub bbox: [u32; 4],
}

/// `[ocr]` configuration section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrConfig {
    pub enabled: bool,
    pub det_path: String,
    pub rec_path: String,
    pub dict_path: String,
    /// Longest image side fed to the detector (multiples of 32).
    pub max_side: u32,
    /// DBNet binarization threshold.
    pub det_threshold: f32,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            det_path: "models/ocr/ch_PP-OCRv4_det_infer.onnx".into(),
            rec_path: "models/ocr/ppocrv5_mobile_rec.onnx".into(),
            dict_path: "models/ocr/ppocrv5_dict.txt".into(),
            max_side: 960,
            det_threshold: 0.3,
        }
    }
}

/// Deterministic offline self-test: OCR one image file (JPEG), report the
/// recognized items. Used by `mibee-eye --selftest-ocr`.
///
/// # Errors
///
/// File errors, decode failure, or inactive engine.
pub fn selftest_image(path: &str) -> anyhow::Result<serde_json::Value> {
    let jpeg = std::fs::read(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?;
    let engine = OcrEngine::from_config(&OcrConfig {
        enabled: true,
        ..OcrConfig::default()
    });
    if !engine.is_active() {
        anyhow::bail!("ocr inactive: {}", engine.inactive_reason());
    }
    let items = engine.recognize_jpeg(&jpeg)?;
    Ok(serde_json::json!({
        "file": path,
        "items": items,
    }))
}

/// Inference backends (ai-feature builds only).
#[cfg(feature = "ai")]
struct OcrInner {
    det: det::DetModel,
    rec: rec::RecModel,
}

/// OCR engine (fail-open: inactive without models, the `ai` feature, or
/// the ONNX Runtime library — `recognize` then reports the reason).
pub struct OcrEngine {
    config: OcrConfig,
    active: bool,
    inactive_reason: String,
    #[cfg(feature = "ai")]
    inner: Option<OcrInner>,
}

impl OcrEngine {
    /// Load models and dictionary (fail-open upstream on error).
    #[must_use]
    pub fn from_config(config: &OcrConfig) -> Self {
        #[cfg(feature = "ai")]
        let loaded = Self::try_load(config).map(Some);
        #[cfg(not(feature = "ai"))]
        let loaded = Self::try_load(config).map(|()| None::<()>);
        match loaded {
            #[allow(clippy::redundant_closure)]
            Ok(inner) => Self {
                config: config.clone(),
                active: true,
                inactive_reason: String::new(),
                #[cfg(feature = "ai")]
                inner,
            },
            Err(reason) => {
                tracing::info!(%reason, "ocr: disabled");
                Self {
                    config: config.clone(),
                    active: false,
                    inactive_reason: format!("{reason:#}"),
                    #[cfg(feature = "ai")]
                    inner: None,
                }
            }
        }
    }

    #[cfg(feature = "ai")]
    fn try_load(config: &OcrConfig) -> Result<OcrInner> {
        for p in [&config.det_path, &config.rec_path, &config.dict_path] {
            if !std::path::Path::new(p).exists() {
                bail!("OCR asset not found: {p}");
            }
        }
        let dict_raw = std::fs::read_to_string(&config.dict_path)
            .with_context(|| format!("read {}", config.dict_path))?;
        let dict: Vec<String> = dict_raw.lines().map(|l| l.to_string()).collect::<Vec<_>>();
        // CTC layout: class 0 = blank, 1..=dict.len() = dict chars, last = space.
        if dict.len() < 100 {
            bail!("OCR dictionary looks wrong ({} entries)", dict.len());
        }
        Ok(OcrInner {
            det: det::DetModel::new(&config.det_path)?,
            rec: rec::RecModel::new(&config.rec_path, dict)?,
        })
    }

    #[cfg(not(feature = "ai"))]
    fn try_load(_config: &OcrConfig) -> Result<()> {
        bail!("built without the `ai` feature")
    }

    /// Whether OCR is usable.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Why OCR is inactive (empty when active).
    #[must_use]
    pub fn inactive_reason(&self) -> &str {
        &self.inactive_reason
    }
}

impl OcrEngine {
    /// Recognize text in a JPEG image (decode + [`OcrEngine::recognize`]).
    ///
    /// # Errors
    ///
    /// JPEG decode failure or the same conditions as `recognize`.
    pub fn recognize_jpeg(&self, jpeg: &[u8]) -> Result<Vec<TextItem>> {
        let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
        let pixels = decoder
            .decode()
            .map_err(|e| anyhow::anyhow!("jpeg decode: {e}"))?;
        let info = decoder
            .info()
            .ok_or_else(|| anyhow::anyhow!("jpeg decode: no info"))?;
        let (w, h) = (info.width as u32, info.height as u32);
        let rgb: Vec<u8> = match info.pixel_format {
            jpeg_decoder::PixelFormat::L8 => {
                let mut v = Vec::with_capacity(pixels.len() * 3);
                for p in &pixels {
                    v.extend_from_slice(&[*p, *p, *p]);
                }
                v
            }
            jpeg_decoder::PixelFormat::RGB24 => pixels,
            other => bail!("unsupported jpeg pixel format {other:?}"),
        };
        self.recognize(&rgb, w, h)
    }

    /// Recognize text in an RGB frame (`rgb` is tightly packed `w×h`).
    ///
    /// # Errors
    ///
    /// Inactive engine or inference failure at either stage.
    pub fn recognize(&self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<TextItem>> {
        if rgb.len() != (w as usize) * (h as usize) * 3 {
            bail!("frame size mismatch: {} bytes for {w}x{h}", rgb.len());
        }
        #[cfg(feature = "ai")]
        let Some(inner) = &self.inner else {
            bail!("ocr inactive: {}", self.inactive_reason);
        };
        #[cfg(feature = "ai")]
        let (det, rec) = (&inner.det, &inner.rec);
        #[cfg(not(feature = "ai"))]
        {
            let _ = (rgb, w, h);
            bail!("ocr inactive: {}", self.inactive_reason);
        }
        #[cfg(feature = "ai")]
        {
            let boxes = det.detect(rgb, w, h, self.config.max_side, self.config.det_threshold)?;
            let mut out = Vec::new();
            for bbox in boxes {
                let crop = crop_rgb(rgb, w, h, bbox);
                if crop.1 < 4 || crop.2 < 4 {
                    continue;
                }
                if let Some((text, score)) = rec.recognize(&crop.0, crop.1, crop.2)?
                    && !text.trim().is_empty()
                {
                    out.push(TextItem { text, score, bbox });
                }
            }
            // Reading order: top-to-bottom, then left-to-right.
            out.sort_by(|a, b| (a.bbox[1], a.bbox[0]).cmp(&(b.bbox[1], b.bbox[0])));
            Ok(out)
        }
    }
}

/// Crop with clamping, returning (pixels, w, h).
fn crop_rgb(rgb: &[u8], w: u32, h: u32, bbox: [u32; 4]) -> (Vec<u8>, u32, u32) {
    let x0 = bbox[0].min(w.saturating_sub(1));
    let y0 = bbox[1].min(h.saturating_sub(1));
    let x1 = (bbox[0] + bbox[2]).min(w);
    let y1 = (bbox[1] + bbox[3]).min(h);
    let cw = x1.saturating_sub(x0).max(1);
    let ch = y1.saturating_sub(y0).max(1);
    let mut out = Vec::with_capacity((cw * ch * 3) as usize);
    for row in 0..ch {
        let base = (((y0 + row) * w + x0) * 3) as usize;
        let len = (cw * 3) as usize;
        let end = (base + len).min(rgb.len());
        let default: &[u8] = &[];
        out.extend_from_slice(rgb.get(base..end).unwrap_or(default));
    }
    (out, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let c = OcrConfig::default();
        assert!(!c.enabled);
        assert!(c.det_path.contains("PP-OCRv4"));
        assert!((c.det_threshold - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn crop_clamps_to_bounds() {
        // 4x4 white image, crop [2,2,8,8] clamps to the 2x2 corner.
        let rgb = vec![255_u8; 4 * 4 * 3];
        let (px, w, h) = crop_rgb(&rgb, 4, 4, [2, 2, 8, 8]);
        assert_eq!((w, h), (2, 2));
        assert_eq!(px.len(), 2 * 2 * 3);
        assert!(px.iter().all(|&v| v == 255));
    }

    /// End-to-end against the real models: render text with the embedded
    /// font, recognize, expect the same text back. Skips without
    /// models/ORT (CI).
    fn workspace_path(rel: &str) -> String {
        format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
    }

    #[cfg(feature = "ai")]
    #[test]
    fn recognize_rendered_text_roundtrip() {
        let cfg = OcrConfig {
            det_path: workspace_path("models/ocr/ch_PP-OCRv4_det_infer.onnx"),
            rec_path: workspace_path("models/ocr/ppocrv5_server_rec.onnx"),
            dict_path: workspace_path("models/ocr/ppocrv5_dict.txt"),
            ..OcrConfig::default()
        };
        let assets_ok = [&cfg.det_path, &cfg.rec_path, &cfg.dict_path]
            .iter()
            .all(|p| std::path::Path::new(p).exists());
        if !assets_ok || !crate::audio_ai::ort_available_for_test() {
            println!("skipping — OCR models or ONNX Runtime unavailable");
            return;
        }
        let engine = OcrEngine::from_config(&cfg);
        assert!(
            engine.is_active(),
            "engine loads: {}",
            engine.inactive_reason()
        );
        // Render "MiBee 2026" in black on white at a natural size for DB.
        let (w, h) = (480_u32, 128_u32);
        let mut rgb = vec![255_u8; (w * h * 3) as usize];
        let font = fontdue::Font::from_bytes(
            crate::watermark::EMBEDDED_FONT,
            fontdue::FontSettings::default(),
        )
        .expect("embedded font");
        let mut x = 24_i32;
        for ch in "MiBee 2026".chars() {
            let (metrics, bitmap) = font.rasterize(ch, 28.0);
            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let v = bitmap[gy * metrics.width + gx];
                    if v == 0 {
                        continue;
                    }
                    let px = x + gx as i32;
                    let py = 48 + gy as i32;
                    if px < 0 || px >= w as i32 || py < 0 || py >= h as i32 {
                        continue;
                    }
                    let base = ((py as u32 * w + px as u32) * 3) as usize;
                    rgb[base] = 255 - v;
                    rgb[base + 1] = 255 - v;
                    rgb[base + 2] = 255 - v;
                }
            }
            x += (metrics.advance_width + 1.0) as i32;
        }
        // ASCII visualize the rendered frame (1 char per 4px cell).
        for y in (0..h).step_by(4) {
            let mut row = String::new();
            for x in (0..w).step_by(4) {
                let base = ((y * w + x) * 3) as usize;
                let lum = rgb[base];
                row.push(if lum < 128 { '#' } else { '.' });
            }
            println!("img|{row}");
        }
        let items = engine.recognize(&rgb, w, h).expect("recognize");
        let joined: String = items
            .iter()
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join(" ");
        println!("OCR roundtrip got: {joined:?}");
        // OCR confuses i/j at this size; accept ≥80% character similarity
        // to the rendered ground truth rather than an exact match.
        let sim = similarity("MiBee 2026", &joined);
        assert!(sim >= 0.8, "similarity {sim} too low: {joined:?}");
        assert!(
            joined.contains("2026"),
            "expected the digit run in {joined:?}"
        );
    }

    /// Character-level similarity (matches / max length) — good enough for
    /// asserting an OCR roundtrip on rendered text.
    fn similarity(expected: &str, got: &str) -> f64 {
        let a: Vec<char> = expected.chars().collect();
        let b: Vec<char> = got.chars().collect();
        let matches = a
            .iter()
            .zip(b.iter())
            .filter(|(x, y)| x.eq_ignore_ascii_case(y))
            .count();
        matches as f64 / a.len().max(b.len()).max(1) as f64
    }
}
