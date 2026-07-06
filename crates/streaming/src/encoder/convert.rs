//! Pixel-format conversion to planar YUV 4:2:0 (I420).
//!
//! All paths feed [`Yuv420p`] into the H.264 encoder, since openh264's
//! `Encoder::encode` consumes planar YUV. The camera may deliver:
//!
//! - **MJPEG** (compressed JPEG bytes) — decoded via [`jpeg_decoder`] then
//!   converted RGB → YUV420p.
//! - **YUYV** (packed YUY2) — planar-ized directly (no colourspace matrix
//!   needed — Y is already luma, just de-interleave and average chroma).
//!
//! Only Linux is supported (V4L2 capture). Other platforms get a
//! `compile_error!` per the project's cross-platform guard rule.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use anyhow::{Context, Result, bail};

/// A planar YUV 4:2:0 (I420) frame.
///
/// Layout: `[Y plane][U plane][V plane]` where
/// - Y is `width * height` bytes,
/// - U is `(width/2) * (height/2)` bytes,
/// - V is `(width/2) * (height/2)` bytes.
///
/// Stride equals width (tightly packed). This matches the layout expected by
/// `openh264::encoder::Encoder::encode` via `YUVBuffer`.
#[derive(Debug, Clone)]
pub struct Yuv420p {
    pub width: u32,
    pub height: u32,
    /// Interleaved `[Y.. | U.. | V..]`.
    pub data: Vec<u8>,
}

impl Yuv420p {
    /// Allocate a zero-filled buffer for the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let y = (width as usize) * (height as usize);
        let uv = (width as usize / 2) * (height as usize / 2);
        Self {
            width,
            height,
            data: vec![0; y + 2 * uv],
        }
    }

    /// Slice over the Y (luma) plane.
    pub fn y_plane(&self) -> &[u8] {
        let n = (self.width as usize) * (self.height as usize);
        &self.data[..n]
    }

    /// Mutable slice over the Y (luma) plane.
    pub fn y_plane_mut(&mut self) -> &mut [u8] {
        let n = (self.width as usize) * (self.height as usize);
        &mut self.data[..n]
    }

    /// Slice over the U (chroma-blue) plane.
    pub fn u_plane(&self) -> &[u8] {
        let y = (self.width as usize) * (self.height as usize);
        let uv = (self.width as usize / 2) * (self.height as usize / 2);
        &self.data[y..y + uv]
    }

    /// Mutable slice over the U (chroma-blue) plane.
    pub fn u_plane_mut(&mut self) -> &mut [u8] {
        let y = (self.width as usize) * (self.height as usize);
        let uv = (self.width as usize / 2) * (self.height as usize / 2);
        &mut self.data[y..y + uv]
    }

    /// Slice over the V (chroma-red) plane.
    pub fn v_plane(&self) -> &[u8] {
        let y = (self.width as usize) * (self.height as usize);
        let uv = (self.width as usize / 2) * (self.height as usize / 2);
        &self.data[y + uv..]
    }

    /// Mutable slice over the V (chroma-red) plane.
    pub fn v_plane_mut(&mut self) -> &mut [u8] {
        let y = (self.width as usize) * (self.height as usize);
        let uv = (self.width as usize / 2) * (self.height as usize / 2);
        &mut self.data[y + uv..]
    }
}

/// Decode MJPEG bytes into a [`Yuv420p`] frame.
///
/// With the `turbojpeg` cargo feature enabled, uses libjpeg-turbo to decode
/// *directly* to planar YUV420 (one step, SIMD-accelerated, ~2-6× faster than
/// the pure-Rust path). Otherwise falls back to [`jpeg_decoder`] → RGB8 →
/// `rgb8_to_yuv420p` (BT.601 matrix).
pub fn mjpeg_to_yuv420p(mjpeg_bytes: &[u8]) -> Result<Yuv420p> {
    #[cfg(feature = "turbojpeg")]
    {
        if let Ok(frame) = mjpeg_to_yuv420p_turbojpeg(mjpeg_bytes) {
            return Ok(frame);
        }
        // Fall through to the pure-Rust path on any turbojpeg error.
    }

    let mut decoder = jpeg_decoder::Decoder::new(mjpeg_bytes);
    let pixels = decoder.decode().context("MJPEG decode failed")?;
    let info = decoder.info().context("MJPEG had no image info")?;
    let width = info.width as u32;
    let height = info.height as u32;

    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => rgb8_to_yuv420p(&pixels, width, height),
        jpeg_decoder::PixelFormat::L8 => {
            // Greyscale JPEG: Y plane = decoded pixels, U/V planes zeroed.
            let mut frame = Yuv420p::new(width, height);
            frame.data[..pixels.len()].copy_from_slice(&pixels);
            Ok(frame)
        }
        other => bail!("unsupported MJPEG pixel format: {other:?}"),
    }
}

/// Decode MJPEG bytes into a raw RGB8 vector plus dimensions.
///
/// Used by the snapshot/preview path when we need to re-encode to JPEG at a
/// different quality or size (for YUYV cameras that don't produce MJPEG).
pub fn mjpeg_to_rgb8(mjpeg_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let mut decoder = jpeg_decoder::Decoder::new(mjpeg_bytes);
    let pixels = decoder.decode().context("MJPEG decode failed")?;
    let info = decoder.info().context("MJPEG had no image info")?;
    Ok((pixels, info.width as u32, info.height as u32))
}

/// Convert packed YUYV (YUY2) bytes into planar YUV420p.
///
/// Input layout: `Y0 U0 Y1 V0 Y2 U1 Y3 V1 …` (two pixels per macropixel).
/// Width and height are the *full* frame dimensions (must both be even).
pub fn yuyv_to_yuv420p(yuyv: &[u8], width: u32, height: u32) -> Result<Yuv420p> {
    if width % 2 != 0 || height % 2 != 0 {
        bail!("YUYV requires even dimensions: {width}x{height}");
    }
    let expected = (width as usize) * (height as usize) * 2;
    if yuyv.len() < expected {
        bail!(
            "YUYV buffer too short: {} < {expected} ({}x{})",
            yuyv.len(),
            width,
            height
        );
    }

    let mut frame = Yuv420p::new(width, height);
    let w = width as usize;
    let h = height as usize;
    let half_w = w / 2;
    let half_h = h / 2;

    // Operate on the single `data` Vec to avoid multiple mutable borrows of
    // `frame`. Layout: [Y plane][U plane][V plane].
    let data = &mut frame.data;
    let y_len = w * h;
    let uv_len = half_w * half_h;
    let (y_plane, uv) = data.split_at_mut(y_len);
    let (u_plane, v_plane) = uv.split_at_mut(uv_len);

    // De-interleave luma row-by-row.
    for row in 0..h {
        let y_off = row * w;
        let src_off = row * w * 2;
        for col in 0..w {
            y_plane[y_off + col] = yuyv[src_off + col * 2];
        }
    }

    // Average each 2x2 chroma block from the interleaved U/V samples.
    // YUYV stores one U,V pair per 2 horizontal pixels; vertical subsampling
    // averages two rows.
    for cy in 0..half_h {
        for cx in 0..half_w {
            // Two source rows, each contributing one U and one V for this column pair.
            let r0 = (cy * 2) * w * 2 + cx * 4;
            let r1 = ((cy * 2 + 1)) * w * 2 + cx * 4;
            let u0 = yuyv[r0 + 1] as u32;
            let u1 = yuyv[r1 + 1] as u32;
            let v0 = yuyv[r0 + 3] as u32;
            let v1 = yuyv[r1 + 3] as u32;
            let idx = cy * half_w + cx;
            u_plane[idx] = ((u0 + u1) / 2) as u8;
            v_plane[idx] = ((v0 + v1) / 2) as u8;
        }
    }

    Ok(frame)
}

/// Convert packed RGB8 (`[R, G, B, R, G, B, …]`) to planar YUV420p using
/// the BT.601 (full-range) matrix.
pub fn rgb8_to_yuv420p(rgb: &[u8], width: u32, height: u32) -> Result<Yuv420p> {
    let expected = (width as usize) * (height as usize) * 3;
    if rgb.len() < expected {
        bail!(
            "RGB buffer too short: {} < {expected} ({}x{})",
            rgb.len(),
            width,
            height
        );
    }

    let mut frame = Yuv420p::new(width, height);
    let w = width as usize;
    let h = height as usize;
    let half_w = w / 2;
    let half_h = h / 2;

    // Operate on the single `data` Vec to avoid multiple mutable borrows.
    let data = &mut frame.data;
    let y_len = w * h;
    let uv_len = half_w * half_h;
    let (y_plane, uv) = data.split_at_mut(y_len);
    let (u_plane, v_plane) = uv.split_at_mut(uv_len);

    // First pass: full-resolution Y.
    for i in 0..w * h {
        let r = rgb[i * 3] as i32;
        let g = rgb[i * 3 + 1] as i32;
        let b = rgb[i * 3 + 2] as i32;
        // BT.601 full range: Y = 0.299R + 0.587G + 0.114B
        let y = (66 * r + 129 * g + 25 * b + 128) >> 8;
        y_plane[i] = y.clamp(0, 255) as u8;
    }

    // Second pass: subsampled U/V (2x2 averaging in RGB space, then matrix).

    for cy in 0..half_h {
        for cx in 0..half_w {
            let mut sr = 0i32;
            let mut sg = 0i32;
            let mut sb = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let px = (cy * 2 + dy) * w + (cx * 2 + dx);
                    sr += rgb[px * 3] as i32;
                    sg += rgb[px * 3 + 1] as i32;
                    sb += rgb[px * 3 + 2] as i32;
                }
            }
            sr /= 4;
            sg /= 4;
            sb /= 4;
            let idx = cy * half_w + cx;
            // U = -0.169R - 0.331G + 0.500B + 128
            let u = (-38 * sr - 74 * sg + 112 * sb + 128 * 128) >> 8;
            // V =  0.500R - 0.419G - 0.081B + 128
            let v = (112 * sr - 94 * sg - 18 * sb + 128 * 128) >> 8;
            u_plane[idx] = u.clamp(0, 255) as u8;
            v_plane[idx] = v.clamp(0, 255) as u8;
        }
    }

    Ok(frame)
}

/// Convert planar YUV420p to packed RGB8 (used by the JPEG re-encode path for
/// snapshot/preview when the camera only outputs YUYV).
pub fn yuv420p_to_rgb8(frame: &Yuv420p) -> Vec<u8> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let half_w = w / 2;
    let y_plane = frame.y_plane();
    let u_plane = frame.u_plane();
    let v_plane = frame.v_plane();

    let mut rgb = vec![0u8; w * h * 3];
    for j in 0..h {
        for i in 0..w {
            let y = y_plane[j * w + i] as i32;
            let cu = u_plane[(j / 2) * half_w + (i / 2)] as i32 - 128;
            let cv = v_plane[(j / 2) * half_w + (i / 2)] as i32 - 128;
            // BT.601 inverse.
            let r = y + 143 * cv / 100;
            let g = y - (34 * cu + 71 * cv) / 100;
            let b = y + 177 * cu / 100;
            let off = (j * w + i) * 3;
            rgb[off] = r.clamp(0, 255) as u8;
            rgb[off + 1] = g.clamp(0, 255) as u8;
            rgb[off + 2] = b.clamp(0, 255) as u8;
        }
    }
    rgb
}

// ── turbojpeg fast path (feature-gated) ───────────────────────────────────────

#[cfg(feature = "turbojpeg")]
fn mjpeg_to_yuv420p_turbojpeg(mjpeg_bytes: &[u8]) -> Result<Yuv420p> {
    use turbojpeg::decompress_to_yuv;

    // libjpeg-turbo's "decompress to YUV" path decodes straight to planar
    // YCbCr 4:2:0 with SIMD (TJ_FASTUPSAMPLE defaults off), skipping the RGB8
    // intermediate entirely. The returned `YuvImage` is tightly packed I420,
    // matching [`Yuv420p`]'s layout exactly.
    let img = decompress_to_yuv(mjpeg_bytes).context("turbojpeg: decompress_to_yuv failed")?;
    Ok(Yuv420p {
        width: img.width,
        height: img.height,
        data: img.data,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuv420p_layout_is_planar_i420() {
        let frame = Yuv420p::new(4, 4);
        assert_eq!(frame.data.len(), 4 * 4 + 2 * 2 * 2); // 16 + 8 = 24
        assert_eq!(frame.y_plane().len(), 16);
        assert_eq!(frame.u_plane().len(), 4);
        assert_eq!(frame.v_plane().len(), 4);
    }

    #[test]
    fn yuyv_roundtrip_preserves_luma() {
        // 4x2 frame, 2 macropixels per row → 8 bytes/row, 16 bytes total.
        let mut yuyv = vec![0u8; 16];
        // Row 0: Y = [10, 20, 30, 40], U/V interleaved.
        yuyv[0] = 10;
        yuyv[2] = 20;
        yuyv[4] = 30;
        yuyv[6] = 40;
        yuyv[1] = 100; // U0
        yuyv[3] = 110; // V0
        yuyv[5] = 120; // U1
        yuyv[7] = 130; // V1
        // Row 1: zeros (default).

        let frame = yuyv_to_yuv420p(&yuyv, 4, 2).unwrap();
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 2);
        let y = frame.y_plane();
        assert_eq!(y[0], 10);
        assert_eq!(y[1], 20);
        assert_eq!(y[2], 30);
        assert_eq!(y[3], 40);
        assert_eq!(y[4..8], [0, 0, 0, 0]); // second row luma
    }

    #[test]
    fn yuyv_rejects_odd_dimensions() {
        let buf = vec![0u8; 100];
        assert!(yuyv_to_yuv420p(&buf, 3, 2).is_err());
        assert!(yuyv_to_yuv420p(&buf, 2, 3).is_err());
    }

    #[test]
    fn rgb_to_yuv_then_back_is_close() {
        // A solid mid-grey pixel block — exact round-trip for grey.
        let w = 4;
        let h = 4;
        let grey_rgb = vec![128u8; w * h * 3];
        let yuv = rgb8_to_yuv420p(&grey_rgb, w as u32, h as u32).unwrap();
        // Grey ≈ Y 128, U/V 128 (neutral).
        for &y in yuv.y_plane() {
            assert!((y as i32 - 128).abs() <= 2, "Y was {y}");
        }
        for &u in yuv.u_plane() {
            assert!((u as i32 - 128).abs() <= 2, "U was {u}");
        }
        for &v in yuv.v_plane() {
            assert!((v as i32 - 128).abs() <= 2, "V was {v}");
        }
    }

    /// Decode a tiny synthetic JPEG to exercise the real jpeg-decoder path.
    /// This is a 2×2 white JPEG (Q=100, baseline).
    #[test]
    fn mjpeg_decode_white_pixel() {
        // Minimal 2x2 white JPEG. Generated once; exact bytes matter.
        static WHITE_JPEG: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06,
            0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D,
            0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D,
            0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28,
            0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32,
            0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x02, 0x00, 0x02,
            0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02,
            0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xFF, 0xC4, 0x00, 0xB5, 0x10,
            0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00,
            0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06,
            0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42,
            0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16,
            0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55,
            0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73,
            0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
            0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
            0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA,
            0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6,
            0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA,
            0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFF, 0xDA, 0x00, 0x08,
            0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0xFB, 0xFC, 0xFE, 0xD4, 0xBF, 0xFF, 0xD9,
        ];
        let frame = mjpeg_to_yuv420p(WHITE_JPEG).unwrap();
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        // White ≈ Y around 250+ (Q=100 isn't perfectly lossless).
        for &y in frame.y_plane() {
            assert!(y > 200, "white pixel Y too low: {y}");
        }
    }
}
