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
    /// Detection input edge as ACTUALLY fed to the session — the model's
    /// declared shape when the export is fixed-size, the configured value
    /// when dynamic (see `resolve_detect_input`).
    detect_input: u32,
    /// Graph I/O names captured at load — exports drift between
    /// "input"/"data"/"images"/"input.1" etc.; the detection heads
    /// (cls_*/obj_*/bbox_*) are addressed by their stride-suffixed names.
    detect_input_name: String,
    recog_input_name: String,
    recog_output_name: String,
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
        let (h, w) = detect_input_shape(&detect);
        let detect_input = resolve_detect_input(h, w, config.detect_input);
        if detect_input != config.detect_input {
            tracing::warn!(
                configured = config.detect_input,
                model_declared = detect_input,
                "face: YuNet export has a fixed input shape — conforming (config value ignored)"
            );
        }
        let detect_input_name = detect
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "input".to_string());
        // SFace's zoo export names the image input `data` and the
        // embedding output `fc1` — earlier hard-codes ("input.1" /
        // "embedding") never matched this file.
        let recog_input_name = recog
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "data".to_string());
        let recog_output_name = recog
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| "fc1".to_string());
        Ok(Inner {
            detect: Mutex::new(detect),
            recog: Mutex::new(recog),
            detect_input,
            detect_input_name,
            recog_input_name,
            recog_output_name,
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
            // One metric span per frame match; the YuNet detect + SFace
            // embed inside run_match are the dominant cost (SPEC appendix
            // A #39: model id `face.recog`).
            let call = observability::model_call("face.recog", &self.metric_variant());
            let r = self.run_match(inner, jpeg);
            match &r {
                Ok(_) => call.finish_ok(None, None),
                Err(_) => call.finish_err(),
            }
            r.unwrap_or_default()
        }
        #[cfg(not(feature = "ai"))]
        {
            let _ = jpeg;
            Vec::new()
        }
    }

    /// Per-model metric variant label: the SFace recognition model stem
    /// (SPEC appendix A #39).
    #[cfg_attr(not(feature = "ai"), allow(dead_code))]
    fn metric_variant(&self) -> String {
        std::path::Path::new(&self.config.recog_model)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    /// Embedding for enrollment — one JPEG frame at a time.
    pub fn embed_jpeg(&self, jpeg: &[u8]) -> anyhow::Result<Vec<f32>> {
        #[cfg(feature = "ai")]
        {
            let Some(inner) = &self.inner else {
                anyhow::bail!("face inactive: {}", self.inactive_reason);
            };
            let call = observability::model_call("face.recog", &self.metric_variant());
            match self.embed_with(inner, jpeg) {
                Ok(e) => {
                    call.finish_ok(None, None);
                    Ok(e)
                }
                Err(e) => {
                    call.finish_err();
                    Err(e)
                }
            }
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
        let reg = self.registry.lock().expect("face registry lock");
        run_match_inner(inner, &reg, jpeg, self.config.match_threshold)
    }

    /// Test/diagnostic access to the loaded models (None when inactive).
    #[cfg(all(test, feature = "ai"))]
    fn inner_for_diag(&self) -> Option<&Inner> {
        self.inner.as_ref()
    }

    /// Test/diagnostic: why the engine is inactive.
    #[cfg(all(test, feature = "ai"))]
    fn inactive_reason_for_diag(&self) -> &str {
        &self.inactive_reason
    }
}

/// The match pipeline as a free function (testable without the engine
/// shell): decode → YuNet detect → SFace embed → registry match.
#[cfg(feature = "ai")]
fn run_match_inner(
    inner: &Inner,
    reg: &FaceRegistry,
    jpeg: &[u8],
    match_threshold: f32,
) -> anyhow::Result<Vec<FaceHit>> {
    let (bgr, w, h) = decode_bgr(jpeg)?;
    let (boxes_, scores) = detect_faces(inner, &bgr, w, h, inner.detect_input)?;
    let mut hits = Vec::new();
    for (i, bbox) in boxes_.iter().enumerate() {
        let emb = embed_face(inner, &bgr, w, h, bbox)?;
        let (name, score) = reg.best_match(&emb, match_threshold);
        hits.push(FaceHit {
            name: name.map(String::from),
            score: if name.is_some() { score } else { scores[i] },
            bbox: *bbox,
        });
    }
    Ok(hits)
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

/// Resize interleaved-HWC BGR by nearest neighbour into (dst_w, dst_h)
/// **CHW planes** — the layout NCHW tensors need (the historical version
/// emitted interleaved pixels under a [1,3,H,W] shape declaration, which
/// scrambled every channel and made the heads output noise).
fn resize_planar(src: &[f32], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<f32> {
    let plane = (dst_w * dst_h) as usize;
    let mut dst = vec![0f32; plane * 3];
    for dy in 0..dst_h {
        let sy = dy * src_h / dst_h.max(1);
        for dx in 0..dst_w {
            let sx = dx * src_w / dst_w.max(1);
            let s = (sy as usize * src_w as usize + sx as usize) * 3;
            let d = dy as usize * dst_w as usize + dx as usize;
            dst[d] = src[s];
            dst[plane + d] = src[s + 1];
            dst[plane * 2 + d] = src[s + 2];
        }
    }
    dst
}

/// Source-scale bbox (x, y, w, h).
type Bbox = (f32, f32, f32, f32);

/// The detect session's declared NCHW H/W (−1 each when dynamic or
/// unreadable).
#[cfg(feature = "ai")]
fn detect_input_shape(session: &Session) -> (i64, i64) {
    let Some(input) = session.inputs().first() else {
        return (-1, -1);
    };
    let Some(shape) = input.dtype().tensor_shape() else {
        return (-1, -1);
    };
    (
        shape.get(2).copied().unwrap_or(-1),
        shape.get(3).copied().unwrap_or(-1),
    )
}

/// Fixed-square YuNet exports (e.g. the zoo's 2023mar ONNX ships a
/// hard-coded 640×640 input) reject any other feed shape — conform to
/// the declaration; dynamic exports honor the configured size.
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
fn resolve_detect_input(h: i64, w: i64, configured: u32) -> u32 {
    if h > 0 && h == w {
        h as u32
    } else {
        configured.max(32)
    }
}

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
    let outputs = detect.run(ort::inputs![&inner.detect_input_name => tensor])?;
    // OpenCV-zoo YuNet 2023mar emits anchor-free multi-scale heads: for
    // each stride s ∈ {8,16,32} a cls_s [1,N,1] + obj_s [1,N,1] +
    // bbox_s [1,N,4] (l,t,r,b distances from the cell centre) over the
    // (input/s)² grid. (The historical [1,1,N,15] layout this decoder
    // assumed belongs to older exports — against the shipped 2023mar
    // file every detection failed.)
    let extract = |stem: &str, stride: u32| -> anyhow::Result<Vec<f32>> {
        let name = format!("{stem}_{stride}");
        let out = outputs
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("face: YuNet output {name} missing"))?;
        let (_dims, data) = out.try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    };
    let mut cand: Vec<(f32, [f32; 4])> = Vec::new();
    for stride in [8u32, 16, 32] {
        let grid = input / stride;
        let n = (grid * grid) as usize;
        let cls = extract("cls", stride)?;
        let obj = extract("obj", stride)?;
        let bb = extract("bbox", stride)?;
        anyhow::ensure!(
            cls.len() == n && obj.len() == n && bb.len() == n * 4,
            "face: YuNet head stride {stride} shape mismatch (cls {}, obj {}, bbox {} for {n} cells)",
            cls.len(),
            obj.len(),
            bb.len()
        );
        // Decode per the model author's reference
        // (libfacedetection.train compare_inference.py): scores are
        // cls·obj products AS OUTPUT (no sigmoid — the graph already
        // emits probabilities), and bbox rows are (cx_off, cy_off,
        // log_w, log_h) scaled by the stride against the plain grid
        // anchor (no half-cell offset).
        for i in 0..n {
            let score = cls[i] * obj[i];
            if score < DET_SCORE_THRESHOLD && std::env::var_os("FACE_DIAG_DUMP").is_none() {
                continue;
            }
            let ax = (i as u32 % grid) as f32 * stride as f32;
            let ay = (i as u32 / grid) as f32 * stride as f32;
            let cx = bb[i * 4] * stride as f32 + ax;
            let cy = bb[i * 4 + 1] * stride as f32 + ay;
            let bw = bb[i * 4 + 2].exp() * stride as f32;
            let bh = bb[i * 4 + 3].exp() * stride as f32;
            cand.push((score, [cx - bw / 2.0, cy - bh / 2.0, bw, bh]));
        }
    }
    if std::env::var_os("FACE_DIAG_DUMP").is_some() && !cand.is_empty() {
        let mut dump = cand.clone();
        dump.sort_by(|a, b| b.0.total_cmp(&a.0));
        eprintln!("DIAG top candidates (score, x,y,w,h):");
        for (score, b) in dump.iter().take(8) {
            eprintln!(
                "  {score:.4}  {:.1},{:.1},{:.1},{:.1}",
                b[0], b[1], b[2], b[3]
            );
        }
    }
    let kept = nms(cand, DET_NMS_IOU, 20);
    let sx = w as f32 / input as f32;
    let sy = h as f32 / input as f32;
    let mut boxes_ = Vec::new();
    let mut scores = Vec::new();
    for (score, [x, y, bw, bh]) in kept {
        boxes_.push((x * sx, y * sy, bw * sx, bh * sy));
        scores.push(score);
    }
    Ok((boxes_, scores))
}

/// Detection score threshold (cls·obj as output by the graph; OpenCV's
/// FaceDetectionYN default).
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
const DET_SCORE_THRESHOLD: f32 = 0.6;
/// NMS IoU threshold (reference default).
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
const DET_NMS_IOU: f32 = 0.45;

/// Score-descending greedy NMS over (score, [x, y, w, h]) candidates.
#[cfg_attr(not(feature = "ai"), allow(dead_code))]
fn nms(cand: Vec<(f32, [f32; 4])>, iou_threshold: f32, top: usize) -> Vec<(f32, [f32; 4])> {
    let mut cand = cand;
    cand.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut kept: Vec<(f32, [f32; 4])> = Vec::new();
    'outer: for c in cand {
        for k in &kept {
            if iou(c.1, k.1) > iou_threshold {
                continue 'outer;
            }
        }
        kept.push(c);
        if kept.len() >= top {
            break;
        }
    }
    kept
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let (ax1, ay1, ax2, ay2) = (a[0], a[1], a[0] + a[2], a[1] + a[3]);
    let (bx1, by1, bx2, by2) = (b[0], b[1], b[0] + b[2], b[1] + b[3]);
    let ix1 = ax1.max(bx1);
    let iy1 = ay1.max(by1);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
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
    let outputs = recog.run(ort::inputs![&inner.recog_input_name => tensor])?;
    let emb_out = outputs
        .get(&inner.recog_output_name)
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

    /// Diagnostic against the real model files (run manually with the
    /// models dir present; prints the error the fail-open path swallows):
    /// `cargo test -p streaming --features ai face_real_models -- --ignored --nocapture`
    #[cfg(all(test, feature = "ai"))]
    #[test]
    #[ignore = "needs real model files on disk"]
    fn face_real_models_diagnostic() {
        // Diagnostics explicitly opt in.
        let cfg = FaceConfig {
            enabled: true,
            ..FaceConfig::default()
        };
        if !std::path::Path::new(&cfg.detect_model).exists() {
            eprintln!("models not present: {}", cfg.detect_model);
            return;
        }
        let engine = FaceEngine::from_config(&cfg);
        assert!(
            engine.is_active(),
            "engine must load (inactive: {})",
            engine.inactive_reason_for_diag()
        );
        // JPEG from env (any real frame; no image crate in this crate).
        let path = std::env::var("FACE_DIAG_JPEG").expect("set FACE_DIAG_JPEG");
        let jpeg = std::fs::read(&path).expect("read jpeg");
        // Bypass the fail-open wrapper to surface the real error.
        let inner = engine.inner_for_diag().expect("inner");
        let reg = FaceRegistry::new();
        match run_match_inner(inner, &reg, &jpeg, cfg.match_threshold) {
            Ok(hits) => eprintln!("DIAG ok: {} hits", hits.len()),
            Err(e) => eprintln!("DIAG error: {e:#}"),
        }
    }

    #[test]
    fn detect_input_conforms_to_fixed_exports() {
        use super::resolve_detect_input;
        // The zoo 2023mar YuNet declares a fixed 640×640 input — the
        // configured 320 must be overridden (this exact mismatch made
        // every match_jpeg fail on the live device).
        assert_eq!(resolve_detect_input(640, 640, 320), 640);
        // Dynamic exports (−1) keep the configured size.
        assert_eq!(resolve_detect_input(-1, -1, 320), 320);
        assert_eq!(resolve_detect_input(-1, 640, 320), 320);
        // Degenerate configured values clamp to something sane.
        assert_eq!(resolve_detect_input(-1, -1, 0), 32);
    }

    #[test]
    fn planar_resize_emits_chw_planes() {
        // 2x2 HWC → 4x4 CHW: each quadrant replicates per plane; plane 0
        // holds the first channel of every pixel.
        let src = vec![
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
        ];
        let dst = resize_planar(&src, 2, 2, 4, 4);
        assert_eq!(dst.len(), 4 * 4 * 3);
        let plane = 16;
        // First channel plane: pixels 1,4,7,10 replicated per quadrant.
        assert_eq!(
            &dst[..plane],
            &vec![
                1.0, 1.0, 4.0, 4.0, 1.0, 1.0, 4.0, 4.0, 7.0, 7.0, 10.0, 10.0, 7.0, 7.0, 10.0, 10.0
            ][..]
        );
        // Third channel plane ends with pixel (1,1)'s blue value.
        assert_eq!(dst[dst.len() - 1], 12.0);
    }
}
