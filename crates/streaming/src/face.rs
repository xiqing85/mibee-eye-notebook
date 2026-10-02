//! Face recognition (SPEC appendix A #33): YuNet detection + SFace
//! embedding, both Apache-2.0 models from the OpenCV zoo. Mirrors the
//! speaker-registry product pattern — enroll a name from live frames,
//! then every AI-detection frame also runs face matching and the
//! grounding layer learns "who" is on screen, so 你看到谁 answers with
//! names. Fail-open on every model/path failure like the other engines.

use std::sync::Mutex;

#[cfg(feature = "ai")]
use std::path::Path;

#[cfg(feature = "ai")]
use ort::session::Session;
#[cfg(feature = "ai")]
use ort::value::Tensor;

/// `[face]` configuration section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FaceConfig {
    /// Master switch (off by default — CPU cost is real on old hosts).
    pub enabled: bool,
    /// YuNet detection model (OpenCV zoo, dynamic input).
    pub detect_model: String,
    /// SFace recognition model (128-d embedding).
    pub recog_model: String,
    /// Detection input edge (model is fully convolutional; larger finds
    /// smaller faces at quadratic CPU cost).
    pub detect_input: u32,
    /// Cosine similarity threshold for a name match. The OpenCV SFace
    /// reference value is 0.363.
    pub match_threshold: f32,
    /// Frames accepted into one enrollment (more = stabler template).
    pub enroll_frames: usize,
    /// Skip detection when the frame is older than this (ms).
    pub ttl_ms: u64,
}

impl Default for FaceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            detect_model: "models/face/face_detection_yunet_2023mar.onnx".into(),
            recog_model: "models/face/face_recognition_sface_2021dec.onnx".into(),
            detect_input: 320,
            match_threshold: 0.363,
            enroll_frames: 8,
            ttl_ms: 10_000,
        }
    }
}

/// One matched face on a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceHit {
    /// Enrolled name, or `None` for an unrecognized face.
    pub name: Option<String>,
    /// Similarity to the best gallery entry (0 for unknown).
    pub score: f32,
    /// Source-frame pixel bbox (x, y, w, h).
    pub bbox: Bbox,
}

/// L2 normalization — SFace cosine space.
fn l2_normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

/// Cosine similarity of two normalized vectors.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Enrolled identity: name + averaged (normalized) template.
#[derive(Debug, Clone, PartialEq)]
pub struct EnrolledFace {
    pub name: String,
    pub embedding: Vec<f32>,
}

/// Gallery + enrollment session state (pure logic, unit-tested).
#[derive(Debug, Default)]
pub struct FaceRegistry {
    faces: Vec<EnrolledFace>,
    session: Option<(String, Vec<Vec<f32>>)>,
}

impl FaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn faces(&self) -> &[EnrolledFace] {
        &self.faces
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    pub fn load(&mut self, faces: Vec<EnrolledFace>) {
        self.faces = faces;
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.faces.len();
        self.faces.retain(|f| f.name != name);
        before != self.faces.len()
    }

    /// Begin an enrollment session for `name` (replaces any pending one).
    pub fn begin_enroll(&mut self, name: &str) {
        self.session = Some((name.to_string(), Vec::new()));
    }

    pub fn cancel_enroll(&mut self) {
        self.session = None;
    }

    pub fn enrollment_status(&self, needed: usize) -> Option<(String, usize, usize)> {
        self.session
            .as_ref()
            .map(|(name, emb)| (name.clone(), emb.len(), needed))
    }

    /// Feed one embedding into the pending session.
    pub fn feed_enroll(&mut self, embedding: Vec<f32>, needed: usize) -> usize {
        if let Some((_, emb)) = &mut self.session {
            emb.push(embedding);
            emb.len().min(needed)
        } else {
            0
        }
    }

    /// Commit the session: averages the collected embeddings into one
    /// normalized template. `Err(reason)` when nothing was collected.
    pub fn commit_enroll(&mut self, needed: usize) -> Result<EnrolledFace, String> {
        let (name, embeddings) = self
            .session
            .take()
            .ok_or_else(|| "没有进行中的注册会话".to_string())?;
        if embeddings.is_empty() {
            return Err("注册会话没有采集到任何帧".into());
        }
        if embeddings.len() < needed {
            let got = embeddings.len();
            self.session = Some((name, embeddings));
            return Err(format!("采集不足：{got}/{needed}"));
        }
        let mut template = vec![0f32; embeddings[0].len()];
        for e in &embeddings {
            for (t, v) in template.iter_mut().zip(e) {
                *t += v;
            }
        }
        l2_normalize(&mut template);
        let face = EnrolledFace {
            name: name.clone(),
            embedding: template,
        };
        // Replace any earlier enrollment of the same name.
        self.faces.retain(|f| f.name != name);
        self.faces.push(face.clone());
        Ok(face)
    }

    /// Best gallery match for a normalized embedding.
    pub fn best_match(&self, embedding: &[f32], threshold: f32) -> (Option<&str>, f32) {
        let mut best: Option<(&str, f32)> = None;
        for f in &self.faces {
            let s = cosine(embedding, &f.embedding);
            if best.is_none_or(|(_, b)| s > b) {
                best = Some((f.name.as_str(), s));
            }
        }
        match best {
            Some((name, s)) if s >= threshold => (Some(name), s),
            Some((_, s)) => (None, s),
            None => (None, 0.0),
        }
    }
}

/// Process-wide face engine (fail-open).
pub struct FaceEngine {
    config: FaceConfig,
    active: bool,
    inactive_reason: String,
    registry: Mutex<FaceRegistry>,
    #[cfg(feature = "ai")]
    inner: Option<Inner>,
}

#[cfg(feature = "ai")]
struct Inner {
    detect: Mutex<Session>,
    recog: Mutex<Session>,
}

impl FaceEngine {
    pub fn from_config(config: &FaceConfig) -> Self {
        #[cfg(feature = "ai")]
        let loaded = Self::load(config);
        #[cfg(not(feature = "ai"))]
        let loaded: anyhow::Result<std::convert::Infallible> =
            Err(anyhow::anyhow!("built without the ai feature"));
        match loaded {
            #[cfg(feature = "ai")]
            Ok(inner) => {
                tracing::info!(
                    detect_model = %config.detect_model,
                    recog_model = %config.recog_model,
                    detect_input = config.detect_input,
                    "face: engine loaded"
                );
                Self {
                    config: config.clone(),
                    active: true,
                    inactive_reason: String::new(),
                    registry: Mutex::new(FaceRegistry::new()),
                    #[cfg(feature = "ai")]
                    inner: Some(inner),
                }
            }
            #[cfg(not(feature = "ai"))]
            Ok(never) => match never {},
            Err(reason) => {
                tracing::info!(%reason, "face: disabled");
                Self {
                    config: config.clone(),
                    active: false,
                    inactive_reason: format!("{reason:#}"),
                    registry: Mutex::new(FaceRegistry::new()),
                    #[cfg(feature = "ai")]
                    inner: None,
                }
            }
        }
    }

    #[cfg(feature = "ai")]
    fn load(config: &FaceConfig) -> anyhow::Result<Inner> {
        if !config.enabled {
            anyhow::bail!("face.enabled = false");
        }
        let detect = load_session(&config.detect_model, "face detect")?;
        let recog = load_session(&config.recog_model, "face recog")?;
        Ok(Inner {
            detect: Mutex::new(detect),
            recog: Mutex::new(recog),
        })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn inactive_reason(&self) -> &str {
        &self.inactive_reason
    }

    pub fn config(&self) -> &FaceConfig {
        &self.config
    }

    /// Registry access (works while inactive too — the web routes list
    /// and manage enrollments even when the models are missing).
    pub fn with_registry<R>(&self, f: impl FnOnce(&mut FaceRegistry) -> R) -> R {
        let mut reg = self.registry.lock().expect("face registry lock");
        f(&mut reg)
    }

    pub fn match_threshold(&self) -> f32 {
        self.config.match_threshold
    }

    pub fn enroll_frames(&self) -> usize {
        self.config.enroll_frames
    }

    /// Detect + recognize faces on a JPEG frame. Empty when inactive or
    /// anything fails (fail-open — callers treat it as "no face info").
    pub fn match_jpeg(&self, jpeg: &[u8]) -> Vec<FaceHit> {
        #[cfg(feature = "ai")]
        {
            let Some(inner) = &self.inner else {
                return Vec::new();
            };
            self.run_match(inner, jpeg).unwrap_or_default()
        }
        #[cfg(not(feature = "ai"))]
        {
            let _ = jpeg;
            Vec::new()
        }
    }

    /// Embedding for enrollment — one JPEG frame at a time.
    pub fn embed_jpeg(&self, jpeg: &[u8]) -> anyhow::Result<Vec<f32>> {
        #[cfg(feature = "ai")]
        {
            let Some(inner) = &self.inner else {
                anyhow::bail!("face inactive: {}", self.inactive_reason);
            };
            self.embed_with(inner, jpeg)
        }
        #[cfg(not(feature = "ai"))]
        {
            let _ = jpeg;
            anyhow::bail!("face inactive: built without the ai feature")
        }
    }

    #[cfg(feature = "ai")]
    fn embed_with(&self, inner: &Inner, jpeg: &[u8]) -> anyhow::Result<Vec<f32>> {
        let (bgr, w, h) = decode_bgr(jpeg)?;
        let (mut boxes_, _) = detect_faces(inner, &bgr, w, h, self.config.detect_input)?;
        let face = boxes_
            .drain(..)
            .next()
            .ok_or_else(|| anyhow::anyhow!("画面中没有检测到人脸"))?;
        embed_face(inner, &bgr, w, h, &face)
    }

    #[cfg(feature = "ai")]
    fn run_match(&self, inner: &Inner, jpeg: &[u8]) -> anyhow::Result<Vec<FaceHit>> {
        let (bgr, w, h) = decode_bgr(jpeg)?;
        let (boxes_, scores) = detect_faces(inner, &bgr, w, h, self.config.detect_input)?;
        let reg = self.registry.lock().expect("face registry lock");
        let mut hits = Vec::new();
        for (i, bbox) in boxes_.iter().enumerate() {
            let emb = embed_face(inner, &bgr, w, h, bbox)?;
            let (name, score) = reg.best_match(&emb, self.config.match_threshold);
            hits.push(FaceHit {
                name: name.map(String::from),
                score: if name.is_some() { score } else { scores[i] },
                bbox: *bbox,
            });
        }
        Ok(hits)
    }
}

#[cfg(feature = "ai")]
fn load_session(path: &str, what: &str) -> anyhow::Result<Session> {
    let p = Path::new(path);
    if !p.exists() {
        anyhow::bail!("{what} model missing: {path}");
    }
    Session::builder()
        .map_err(|e| anyhow::anyhow!("{what} session builder: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("{what} threads: {e}"))?
        .commit_from_file(p)
        .map_err(|e| anyhow::anyhow!("{what} load {path}: {e}"))
}

/// Decode JPEG to planar BGR f32 (0-255) at native resolution —
/// YuNet/SFace are OpenCV-zoo models and expect BGR.
#[cfg(feature = "ai")]
fn decode_bgr(jpeg: &[u8]) -> anyhow::Result<(Vec<f32>, u32, u32)> {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    let pixels = decoder.decode().context("face: JPEG decode failed")?;
    let info = decoder
        .info()
        .context("face: JPEG header missing after decode")?;
    let (w, h) = (info.width as u32, info.height as u32);
    let src = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => {
            let mut rgb = Vec::with_capacity(pixels.len() * 3);
            for &y in &pixels {
                rgb.extend_from_slice(&[y, y, y]);
            }
            rgb
        }
        other => anyhow::bail!("face: unsupported JPEG pixel format {other:?}"),
    };
    let mut bgr = vec![0f32; (w as usize) * (h as usize) * 3];
    for (dst, px) in bgr
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<3>().0)
    {
        dst[0] = px[2] as f32;
        dst[1] = px[1] as f32;
        dst[2] = px[0] as f32;
    }
    Ok((bgr, w, h))
}

/// Resize planar BGR by nearest neighbour into (dst_w, dst_h).
fn resize_planar(src: &[f32], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<f32> {
    let mut dst = vec![0f32; (dst_w as usize) * (dst_h as usize) * 3];
    let n = dst.len() / 3;
    for i in 0..n {
        let dx = (i % dst_w as usize) as u32;
        let dy = (i / dst_w as usize) as u32;
        let sx = dx * src_w / dst_w.max(1);
        let sy = dy * src_h / dst_h.max(1);
        let s = (sy as usize * src_w as usize + sx as usize) * 3;
        let d = i * 3;
        dst[d] = src[s];
        dst[d + 1] = src[s + 1];
        dst[d + 2] = src[s + 2];
    }
    dst
}

/// Source-scale bbox (x, y, w, h).
type Bbox = (f32, f32, f32, f32);

/// YuNet: returns source-scale bboxes with detection scores.
#[cfg(feature = "ai")]
fn detect_faces(
    inner: &Inner,
    bgr: &[f32],
    w: u32,
    h: u32,
    input: u32,
) -> anyhow::Result<(Vec<Bbox>, Vec<f32>)> {
    let planar = resize_planar(bgr, w, h, input, input);
    let tensor = Tensor::from_array((vec![1, 3, input as usize, input as usize], planar))?;
    let mut detect = inner.detect.lock().expect("face detect session lock");
    let outputs = detect.run(ort::inputs!["input" => tensor])?;
    let faces_out = outputs
        .get("output")
        .ok_or_else(|| anyhow::anyhow!("face: YuNet output missing"))?;
    let (dims, data) = faces_out.try_extract_tensor::<f32>()?;
    // OpenCV-zoo YuNet emits [1, 1, N, 15]: x y w h, 5 keypoints, score.
    let rows = *dims.get(2).unwrap_or(&0) as usize;
    let row_len = *dims.get(3).unwrap_or(&0) as usize;
    anyhow::ensure!(
        dims.len() == 4 && row_len == 15 && data.len() == rows * row_len,
        "face: unexpected YuNet output {dims:?}"
    );
    let mut boxes_ = Vec::new();
    let mut scores = Vec::new();
    for r in 0..rows {
        let row = &data[r * row_len..(r + 1) * row_len];
        let score = row[14];
        if score < 0.7 {
            continue;
        }
        let sx = w as f32 / input as f32;
        let sy = h as f32 / input as f32;
        boxes_.push((row[0] * sx, row[1] * sy, row[2] * sx, row[3] * sy));
        scores.push(score);
    }
    Ok((boxes_, scores))
}

/// SFace: crop (source-scale bbox), resize to 112×112 BGR, embed,
/// L2-normalize.
#[cfg(feature = "ai")]
fn embed_face(inner: &Inner, bgr: &[f32], w: u32, h: u32, bbox: &Bbox) -> anyhow::Result<Vec<f32>> {
    const SIZE: u32 = 112;
    let (x, y, bw, bh) = *bbox;
    let x0 = x.clamp(0.0, w as f32 - 1.0) as u32;
    let y0 = y.clamp(0.0, h as f32 - 1.0) as u32;
    let x1 = (x + bw).clamp(x0 as f32 + 1.0, w as f32) as u32;
    let y1 = (y + bh).clamp(y0 as f32 + 1.0, h as f32) as u32;
    let cw = (x1 - x0).max(1);
    let ch = (y1 - y0).max(1);
    let mut crop = vec![0f32; (cw as usize) * (ch as usize) * 3];
    for cy in 0..ch {
        for cx in 0..cw {
            let s = ((y0 + cy) as usize * w as usize + (x0 + cx) as usize) * 3;
            let d = (cy as usize * cw as usize + cx as usize) * 3;
            crop[d..d + 3].copy_from_slice(&bgr[s..s + 3]);
        }
    }
    let planar = resize_planar(&crop, cw, ch, SIZE, SIZE);
    let tensor = Tensor::from_array((vec![1, 3, SIZE as usize, SIZE as usize], planar))?;
    let mut recog = inner.recog.lock().expect("face recog session lock");
    let outputs = recog.run(ort::inputs!["input.1" => tensor])?;
    let emb_out = outputs
        .get("embedding")
        .ok_or_else(|| anyhow::anyhow!("face: SFace output missing"))?;
    let (_, emb_data) = emb_out.try_extract_tensor::<f32>()?;
    let mut embedding: Vec<f32> = emb_data.to_vec();
    anyhow::ensure!(!embedding.is_empty(), "face: empty SFace embedding");
    l2_normalize(&mut embedding);
    Ok(embedding)
}

#[cfg(feature = "ai")]
use anyhow::Context as _;

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let mut v = v;
        l2_normalize(&mut v);
        v
    }

    #[test]
    fn cosine_and_threshold_semantics() {
        let a = unit(vec![1.0, 0.0, 0.0]);
        let b = unit(vec![0.9, 0.1, 0.0]);
        let c = unit(vec![0.0, 1.0, 0.0]);
        assert!(cosine(&a, &b) > 0.99);
        assert!(cosine(&a, &c).abs() < 1e-6);
    }

    #[test]
    fn registry_enroll_commit_and_match() {
        let mut reg = FaceRegistry::new();
        reg.begin_enroll("张三");
        let template = unit(vec![1.0, 0.0, 0.0]);
        for _ in 0..3 {
            reg.feed_enroll(unit(vec![1.0, 0.0, 0.0]), 3);
        }
        let face = reg.commit_enroll(3).expect("committed");
        assert_eq!(face.name, "张三");
        assert_eq!(reg.len(), 1);

        // Same person → named; orthogonal stranger → unknown with the
        // raw best score still reported for diagnostics.
        let (name, score) = reg.best_match(&template, 0.363);
        assert_eq!(name, Some("张三"));
        assert!(score > 0.99);
        let (name, _) = reg.best_match(&unit(vec![0.0, 1.0, 0.0]), 0.363);
        assert_eq!(name, None);

        // Re-enroll replaces, remove deletes.
        reg.begin_enroll("张三");
        for _ in 0..2 {
            reg.feed_enroll(unit(vec![1.0, 0.0, 0.0]), 2);
        }
        reg.commit_enroll(2).expect("replaced");
        assert_eq!(reg.len(), 1);
        assert!(reg.remove("张三"));
        assert!(reg.is_empty());
        assert_eq!(reg.best_match(&template, 0.363), (None, 0.0));
    }

    #[test]
    fn commit_requires_enough_frames() {
        let mut reg = FaceRegistry::new();
        reg.begin_enroll("李四");
        reg.feed_enroll(unit(vec![1.0, 0.0]), 4);
        let err = reg.commit_enroll(4).unwrap_err();
        assert!(err.contains("1/4"), "{err}");
        // Session survives the failed commit.
        reg.feed_enroll(unit(vec![1.0, 0.0]), 4);
        reg.feed_enroll(unit(vec![1.0, 0.0]), 4);
        reg.feed_enroll(unit(vec![1.0, 0.0]), 4);
        reg.commit_enroll(4).expect("committed after enough frames");
    }

    #[test]
    fn inactive_engine_is_fail_open() {
        let e = FaceEngine::from_config(&FaceConfig::default());
        assert!(!e.is_active());
        assert!(e.match_jpeg(&[]).is_empty());
        assert!(e.embed_jpeg(&[]).is_err());
        // Registry management still works while inactive.
        e.with_registry(|r| r.begin_enroll("张三"));
    }

    #[test]
    fn planar_resize_samples_source_pixels() {
        // 2x2 → 4x4 nearest neighbour: each quadrant replicates.
        let src = vec![
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
        ];
        let dst = resize_planar(&src, 2, 2, 4, 4);
        assert_eq!(dst.len(), 4 * 4 * 3);
        assert_eq!(dst[0], 1.0);
        assert_eq!(dst[dst.len() - 1], 12.0);
    }
}
