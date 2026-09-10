//! Video watermark (SPEC v1 §5.2): burns custom text + a real-time clock
//! into YUV420p frames before encoding, OSD-style — every downstream
//! consumer (RTSP, MSE, recordings, JPEG snapshots) sees the same burn,
//! exactly like the device-level flips.
//!
//! Rendering is cached: the composed line (text + timestamp) only changes
//! when the formatted timestamp changes (typically once per second), so
//! glyph rasterization runs at ~1 Hz and each frame pays only a small
//! luma/chroma blit. Style is fixed in v1: white text with a 1px black
//! outline (readable on any background), 16 px frame margin.
//!
//! Fonts: the embedded ASCII subset covers the timestamp and ASCII text out
//! of the box; `font_path` loads a full TTF/OTF at runtime (e.g. a CJK
//! font for Chinese text). A failed `font_path` load falls back to the
//! embedded font (fail-open) and logs a warning.

use chrono::{DateTime, Local};
use fontdue::Font;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::encoder::convert::Yuv420p;

/// Embedded default font: DejaVu Sans Mono subsetted to printable ASCII
/// (~8 KB; license: `assets/fonts/LICENSE-DejaVu.txt`). Non-ASCII text needs
/// `font_path` pointing at a font with those glyphs.
pub const EMBEDDED_FONT: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono-ASCII.ttf");

/// BT.601 studio-swing luma for text (white) and outline (black).
const Y_TEXT: u8 = 235;
const Y_OUTLINE: u8 = 16;
/// Neutral chroma painted over every 2×2 block touched by the mask.
const UV_NEUTRAL: u8 = 128;
/// Distance of the mask from the frame edges (SPEC §5.2: fixed, not configurable).
const MARGIN_PX: usize = 16;
/// Alpha threshold above which a rasterized pixel counts as "on".
const ALPHA_THRESHOLD: u8 = 128;

/// Watermark position on the frame (SPEC §5.2; kebab-case wire format).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

fn default_show_timestamp() -> bool {
    true
}
fn default_timestamp_format() -> String {
    "%Y-%m-%d %H:%M:%S".to_string()
}
fn default_font_size() -> u32 {
    24
}

/// Watermark settings as persisted by the web layer
/// (`protocols.watermark`, SPEC §5.2). The JsonSchema derive drives the
/// Web API's field/type/range/enum validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WatermarkSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Custom text (≤128 chars); CJK requires a `font_path` with CJK glyphs.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub text: String,
    #[serde(default = "default_show_timestamp")]
    pub show_timestamp: bool,
    /// strftime subset whitelist: %Y %m %d %H %M %S %F %T %% + literals.
    #[serde(default = "default_timestamp_format")]
    pub timestamp_format: String,
    #[serde(default)]
    pub position: Position,
    /// Pixel height, 12..96.
    #[serde(default = "default_font_size")]
    #[schemars(range(min = 12, max = 96))]
    pub font_size: u32,
    /// Optional TTF/OTF (e.g. CJK); empty = embedded ASCII subset font.
    #[serde(default)]
    pub font_path: String,
}

impl Default for WatermarkSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            text: String::new(),
            show_timestamp: default_show_timestamp(),
            timestamp_format: default_timestamp_format(),
            position: Position::TopLeft,
            font_size: default_font_size(),
            font_path: String::new(),
        }
    }
}

#[derive(Debug)]
pub enum WatermarkError {
    FontLoad(String),
    InvalidFormat(String),
}

impl std::fmt::Display for WatermarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatermarkError::FontLoad(msg) => write!(f, "watermark font load failed: {msg}"),
            WatermarkError::InvalidFormat(fmt) => write!(
                f,
                "watermark.timestamp_format only allows %Y %m %d %H %M %S %F %T %% and literals, got: {fmt}"
            ),
        }
    }
}

impl std::error::Error for WatermarkError {}

/// strftime specifiers accepted in `timestamp_format` (SPEC §5.2).
const ALLOWED_SPECIFIERS: &[u8] = b"YmdFHMST%";

/// True if `fmt` only contains whitelisted strftime specifiers and literals.
pub fn valid_timestamp_format(fmt: &str) -> bool {
    let bytes = fmt.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        let Some(&spec) = bytes.get(i + 1) else {
            return false; // trailing '%'
        };
        if !ALLOWED_SPECIFIERS.contains(&spec) {
            return false;
        }
        i += 2;
    }
    true
}

/// Format `now` with the validated strftime subset (chrono handles rendering).
pub fn format_timestamp(fmt: &str, now: &DateTime<Local>) -> String {
    now.format(fmt).to_string()
}

/// A rasterized text line: binary fill mask plus its 1px dilation (outline).
struct TextMask {
    w: usize,
    h: usize,
    fill: Vec<bool>,
    outline: Vec<bool>,
}

#[allow(dead_code)]
impl TextMask {
    fn painted(&self) -> usize {
        self.fill.iter().filter(|&&on| on).count()
    }
}

/// Rasterize one line of text at `px` height into a binary fill mask plus an
/// 8-neighborhood-dilated outline mask.
fn rasterize_line(font: &Font, line: &str, px: f32) -> TextMask {
    // First pass: pen advance + ascent/descent to size the mask.
    let mut pen = 0.0_f32;
    let mut ascent = 0_i32;
    let mut descent = 0_i32;
    for ch in line.chars() {
        let m = font.metrics(ch, px);
        ascent = ascent.max(-m.ymin);
        descent = descent.max(m.ymin + m.height as i32);
        pen += m.advance_width;
    }
    let w = pen.ceil().max(0.0) as usize;
    let h = (ascent + descent).max(0) as usize;
    if w == 0 || h == 0 {
        return TextMask {
            w,
            h,
            fill: Vec::new(),
            outline: Vec::new(),
        };
    }
    let mut fill = vec![false; w * h];
    // Second pass: rasterize and stamp pixels that cross the alpha threshold.
    let mut pen = 0.0_f32;
    for ch in line.chars() {
        let (m, bitmap) = font.rasterize(ch, px);
        let gx = (pen + m.xmin as f32).round().max(0.0) as usize;
        let gy = (ascent + m.ymin).max(0) as usize;
        for row in 0..m.height {
            let dy = gy + row;
            if dy >= h {
                break;
            }
            for col in 0..m.width {
                let dx = gx + col;
                if dx >= w {
                    break;
                }
                if bitmap[row * m.width + col] >= ALPHA_THRESHOLD {
                    fill[dy * w + dx] = true;
                }
            }
        }
        pen += m.advance_width;
    }
    // Outline = dilation of fill, minus fill itself.
    let mut outline = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if !fill[y * w + x] {
                continue;
            }
            for dy in -1_i64..=1 {
                for dx in -1_i64..=1 {
                    let ny = y as i64 + dy;
                    let nx = x as i64 + dx;
                    if ny < 0 || nx < 0 || ny >= h as i64 || nx >= w as i64 {
                        continue;
                    }
                    outline[ny as usize * w + nx as usize] = true;
                }
            }
        }
    }
    for (o, f) in outline.iter_mut().zip(&fill) {
        if *f {
            *o = false;
        }
    }
    TextMask {
        w,
        h,
        fill,
        outline,
    }
}

/// Top-left origin of the mask for `position` inside a `width`×`height`
/// frame, clamped so at least part of the mask stays visible.
fn origin(position: Position, width: usize, height: usize, mw: usize, mh: usize) -> (usize, usize) {
    let x = match position {
        Position::TopLeft | Position::BottomLeft => MARGIN_PX,
        Position::TopRight | Position::BottomRight => width.saturating_sub(mw + MARGIN_PX),
    };
    let y = match position {
        Position::TopLeft | Position::TopRight => MARGIN_PX,
        Position::BottomLeft | Position::BottomRight => height.saturating_sub(mh + MARGIN_PX),
    };
    (
        x.min(width.saturating_sub(1)),
        y.min(height.saturating_sub(1)),
    )
}

/// Runtime watermark renderer. Owned by the capture task; the mask cache is
/// interior to the renderer (re-rasterizes when the composed line changes).
pub struct Watermark {
    text: String,
    show_timestamp: bool,
    timestamp_format: String,
    position: Position,
    font_size: f32,
    font: Font,
    cache_key: Option<String>,
    cache_mask: Option<TextMask>,
    rasterizations: u64,
}

impl Watermark {
    /// Build from settings. Rejects a non-whitelisted `timestamp_format`;
    /// `font_path` failures fall back to the embedded ASCII font with a
    /// warning (fail-open).
    pub fn new(cfg: &WatermarkSettings) -> Result<Self, WatermarkError> {
        if !valid_timestamp_format(&cfg.timestamp_format) {
            return Err(WatermarkError::InvalidFormat(cfg.timestamp_format.clone()));
        }
        let font = if cfg.font_path.is_empty() {
            Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
                .map_err(|e| WatermarkError::FontLoad(format!("embedded font: {e}")))?
        } else {
            match std::fs::read(&cfg.font_path) {
                Ok(data) => match Font::from_bytes(&data[..], fontdue::FontSettings::default()) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(
                            path = %cfg.font_path,
                            error = %e,
                            "watermark: font unparseable — falling back to embedded ASCII font"
                        );
                        Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
                            .map_err(|e| WatermarkError::FontLoad(format!("embedded font: {e}")))?
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %cfg.font_path,
                        error = %e,
                        "watermark: font unreadable — falling back to embedded ASCII font"
                    );
                    Font::from_bytes(EMBEDDED_FONT, fontdue::FontSettings::default())
                        .map_err(|e| WatermarkError::FontLoad(format!("embedded font: {e}")))?
                }
            }
        };
        if !cfg.text.is_empty() {
            let missing: Vec<char> = cfg
                .text
                .chars()
                .filter(|&c| !c.is_whitespace() && !font.has_glyph(c))
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    missing = ?missing,
                    "watermark: font lacks glyphs — they render as missing-glyph boxes; set font_path to a font covering them"
                );
            }
        }
        Ok(Self {
            text: cfg.text.clone(),
            show_timestamp: cfg.show_timestamp,
            timestamp_format: cfg.timestamp_format.clone(),
            position: cfg.position,
            font_size: cfg.font_size as f32,
            font,
            cache_key: None,
            cache_mask: None,
            rasterizations: 0,
        })
    }

    /// Whether anything would be painted.
    pub fn has_content(&self) -> bool {
        self.show_timestamp || !self.text.is_empty()
    }

    /// The full composed line for `now`: `text` + two spaces + timestamp,
    /// with either half omitted when disabled/empty.
    fn line(&self, now: &DateTime<Local>) -> String {
        let ts = if self.show_timestamp {
            format_timestamp(&self.timestamp_format, now)
        } else {
            String::new()
        };
        match (self.text.is_empty(), ts.is_empty()) {
            (true, true) => String::new(),
            (true, false) => ts,
            (false, true) => self.text.clone(),
            (false, false) => format!("{}  {}", self.text, ts),
        }
    }

    /// Burn the watermark into the frame in place. The mask is
    /// re-rasterized only when the composed line changes (typically once
    /// per second). Malformed (too short) buffers are left untouched.
    pub fn render_into(&mut self, yuv: &mut Yuv420p) {
        if !self.has_content() {
            return;
        }
        let line = self.line(&Local::now());
        if line.is_empty() {
            return;
        }
        if self.cache_key.as_deref() != Some(line.as_str()) {
            self.cache_mask = Some(rasterize_line(&self.font, &line, self.font_size));
            self.cache_key = Some(line);
            self.rasterizations += 1;
        }
        let Some(mask) = &self.cache_mask else {
            return;
        };
        blit_mask(yuv, mask, self.position);
    }

    /// Number of rasterizations performed (cache efficiency observable in tests).
    #[cfg(test)]
    fn rasterization_count(&self) -> u64 {
        self.rasterizations
    }
}

/// Paint `mask` into the frame at the position-derived origin, clipping at
/// frame edges. Luma: fill → white (235), outline → black (16); every
/// touched 2×2 chroma block → neutral (128, 128).
fn blit_mask(yuv: &mut Yuv420p, mask: &TextMask, position: Position) {
    let (width, height) = (yuv.width as usize, yuv.height as usize);
    if mask.w == 0 || mask.h == 0 || width == 0 || height == 0 {
        return;
    }
    let chroma_stride = width / 2;
    let expected = width * height + 2 * chroma_stride * (height / 2);
    if yuv.data.len() < expected {
        return; // malformed buffer — leave untouched, like `Yuv420p::flip`
    }
    let (x0, y0) = origin(position, width, height, mask.w, mask.h);
    let (y_plane, rest) = yuv.data.split_at_mut(width * height);
    let (u_plane, v_plane) = rest.split_at_mut(chroma_stride * (height / 2));
    for my in 0..mask.h {
        let dy = y0 + my;
        if dy >= height {
            break;
        }
        for mx in 0..mask.w {
            let dx = x0 + mx;
            if dx >= width {
                break;
            }
            let (fill, outline) = (mask.fill[my * mask.w + mx], mask.outline[my * mask.w + mx]);
            if !fill && !outline {
                continue;
            }
            y_plane[dy * width + dx] = if fill { Y_TEXT } else { Y_OUTLINE };
            let cidx = (dy / 2) * chroma_stride + (dx / 2);
            u_plane[cidx] = UV_NEUTRAL;
            v_plane[cidx] = UV_NEUTRAL;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(
        text: &str,
        show_ts: bool,
        position: Position,
        font_size: u32,
    ) -> WatermarkSettings {
        WatermarkSettings {
            enabled: true,
            text: text.to_string(),
            show_timestamp: show_ts,
            timestamp_format: "%Y-%m-%d %H:%M:%S".to_string(),
            position,
            font_size,
            font_path: String::new(),
        }
    }

    /// 64×32 frame: mid-gray luma, non-neutral chroma so paints stand out.
    fn frame(w: u32, h: u32) -> Yuv420p {
        let mut yuv = Yuv420p::new(w, h);
        for b in yuv.y_plane_mut() {
            *b = 128;
        }
        for b in yuv.u_plane_mut() {
            *b = 90;
        }
        for b in yuv.v_plane_mut() {
            *b = 180;
        }
        yuv
    }

    fn wm_or_fail(cfg: &WatermarkSettings) -> Watermark {
        Watermark::new(cfg).expect("embedded font always loads")
    }

    #[test]
    fn timestamp_format_whitelist() {
        assert!(valid_timestamp_format("%Y-%m-%d %H:%M:%S"));
        assert!(valid_timestamp_format("%F %T"));
        assert!(valid_timestamp_format("cam %% %Y"));
        assert!(valid_timestamp_format("literal text"));
        assert!(!valid_timestamp_format("%y"));
        assert!(!valid_timestamp_format("%Q"));
        assert!(!valid_timestamp_format("100%"));
        assert!(!valid_timestamp_format("trailing %"));
    }

    #[test]
    fn new_rejects_non_whitelisted_format() {
        let cfg = WatermarkSettings {
            timestamp_format: "%y".to_string(),
            ..settings("", true, Position::TopLeft, 16)
        };
        assert!(matches!(
            Watermark::new(&cfg),
            Err(WatermarkError::InvalidFormat(_))
        ));
    }

    #[test]
    fn settings_defaults_roundtrip() {
        let s: WatermarkSettings = serde_json::from_str("{}").unwrap();
        assert!(!s.enabled);
        assert!(s.show_timestamp);
        assert_eq!(s.timestamp_format, "%Y-%m-%d %H:%M:%S");
        assert_eq!(s.position, Position::TopLeft);
        assert_eq!(s.font_size, 24);
        assert_eq!(s.font_path, "");
        // Kebab-case wire format for the position enum.
        let s: WatermarkSettings = serde_json::from_str(r#"{"position": "bottom-right"}"#).unwrap();
        assert_eq!(s.position, Position::BottomRight);
    }

    #[test]
    fn blit_paints_white_text_and_black_outline() {
        let wm = wm_or_fail(&settings("ABC", false, Position::TopLeft, 16));
        let mask = rasterize_line(&wm.font, "ABC", 16.0);
        assert!(mask.painted() > 0);
        let mut yuv = frame(64, 32);
        blit_mask(&mut yuv, &mask, Position::TopLeft);
        assert!(
            yuv.y_plane().contains(&Y_TEXT),
            "no white text pixels painted"
        );
        assert!(
            yuv.y_plane().contains(&Y_OUTLINE),
            "no black outline pixels painted"
        );
        assert_eq!(yuv.y_plane()[31 * 64 + 63], 128, "background must survive");
    }

    #[test]
    fn blit_neutralizes_chroma_of_painted_blocks_only() {
        let wm = wm_or_fail(&settings("ABC", false, Position::TopLeft, 16));
        let mask = rasterize_line(&wm.font, "ABC", 16.0);
        let mut yuv = frame(64, 32);
        blit_mask(&mut yuv, &mask, Position::TopLeft);
        assert!(yuv.u_plane().contains(&128), "no chroma blocks neutralized");
        // Last chroma block (row 15 of 16, col 31 of 32) must stay untouched.
        assert_eq!(
            yuv.v_plane()[15 * 32 + 31],
            180,
            "far chroma block must stay untouched"
        );
    }

    #[test]
    fn blit_positions_and_clamps() {
        for pos in [
            Position::TopLeft,
            Position::TopRight,
            Position::BottomLeft,
            Position::BottomRight,
        ] {
            let wm = wm_or_fail(&settings("ABC", false, pos, 16));
            let mask = rasterize_line(&wm.font, "ABC", 16.0);
            let mut yuv = frame(64, 32);
            blit_mask(&mut yuv, &mask, pos);
            assert!(yuv.y_plane().contains(&Y_TEXT), "{pos:?} painted nothing");
        }
        // Oversized mask on a tiny frame must clip without panicking.
        let wm = wm_or_fail(&settings("ABC", false, Position::TopLeft, 96));
        let mask = rasterize_line(&wm.font, "ABC", 96.0);
        let mut yuv = frame(8, 8);
        blit_mask(&mut yuv, &mask, Position::TopRight);
    }

    #[test]
    fn render_caches_mask_until_line_changes() {
        // Static text → constant line → exactly one rasterization ever.
        let mut wm = wm_or_fail(&settings("FIXED", false, Position::TopLeft, 16));
        let mut yuv = frame(64, 32);
        wm.render_into(&mut yuv);
        wm.render_into(&mut yuv);
        wm.render_into(&mut yuv);
        assert_eq!(
            wm.rasterization_count(),
            1,
            "constant line must rasterize once"
        );
        wm.cache_key = Some("different".to_string());
        wm.render_into(&mut yuv);
        assert_eq!(wm.rasterization_count(), 2);
    }

    #[test]
    fn render_paints_timestamp() {
        let mut wm = wm_or_fail(&settings("", true, Position::TopLeft, 16));
        let mut yuv = frame(64, 32);
        wm.render_into(&mut yuv);
        assert!(yuv.y_plane().contains(&Y_TEXT), "timestamp not painted");
    }

    #[test]
    fn no_content_is_noop() {
        let mut wm = wm_or_fail(&settings("", false, Position::TopLeft, 16));
        assert!(!wm.has_content());
        let mut yuv = frame(64, 32);
        let before = yuv.data.clone();
        wm.render_into(&mut yuv);
        assert_eq!(yuv.data, before, "frame must be untouched without content");
    }

    #[test]
    fn cjk_text_renders_with_cjk_font() {
        let cfg = WatermarkSettings {
            font_path: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/fonts/NotoSansSC-Common.otf"
            )
            .to_string(),
            ..settings("水印测试", false, Position::TopLeft, 24)
        };
        let wm = wm_or_fail(&cfg);
        let mask = rasterize_line(&wm.font, "水印测试", 24.0);
        assert!(mask.painted() > 0, "CJK glyphs produced no pixels");
    }

    #[test]
    fn missing_font_path_falls_back_to_embedded() {
        let cfg = WatermarkSettings {
            font_path: "/nonexistent/font.ttf".to_string(),
            ..settings("ABC", false, Position::TopLeft, 16)
        };
        let mut wm = wm_or_fail(&cfg);
        let mut yuv = frame(64, 32);
        wm.render_into(&mut yuv);
        assert!(
            yuv.y_plane().contains(&Y_TEXT),
            "fallback embedded font painted nothing"
        );
    }

    #[test]
    fn short_buffer_is_skipped_not_panicked() {
        let mut wm = wm_or_fail(&settings("ABC", false, Position::TopLeft, 16));
        let mut yuv = Yuv420p {
            width: 64,
            height: 32,
            data: vec![0u8; 10],
        };
        wm.render_into(&mut yuv);
    }
}
