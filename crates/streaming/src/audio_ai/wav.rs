//! Minimal RIFF/WAVE reader for self-test fixtures and user-supplied test
//! audio (PCM i16 / i8 / f32, any sample rate / channel count). Decodes to
//! mono f32 plus the declared sample rate; the caller resamples if needed.

use anyhow::{Context, Result, bail};

/// A decoded WAV file.
#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    /// Mono samples, normalized to ±1.0.
    pub samples: Vec<f32>,
}

/// Parse a WAV buffer.
///
/// # Errors
///
/// Not a RIFF/WAVE file, unsupported codec (only PCM / IEEE float), or a
/// truncated chunk layout.
pub fn parse_wav(bytes: &[u8]) -> Result<WavData> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let mut pos = 12;
    let mut format: Option<(u16, u16, u32)> = None; // (codec, channels, rate)
    let mut bits: Option<u16> = None;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().context("chunk header")?)
            as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " => {
                if body.len() < 16 {
                    bail!("truncated fmt chunk");
                }
                let codec = u16::from_le_bytes(body[0..2].try_into()?);
                let channels = u16::from_le_bytes(body[2..4].try_into()?);
                let rate = u32::from_le_bytes(body[4..8].try_into()?);
                bits = Some(u16::from_le_bytes(body[14..16].try_into()?));
                format = Some((codec, channels, rate));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos = body_start + size + (size & 1); // chunks are word-aligned
    }
    let Some((codec, channels, rate)) = format else {
        bail!("missing fmt chunk");
    };
    let Some(data) = data else {
        bail!("missing data chunk");
    };
    let bits = bits.unwrap_or(16);
    let samples = match (codec, bits) {
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b) as f32 / 32_768.0)
            .collect::<Vec<_>>(),
        (1, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect(),
        other => bail!("unsupported WAV codec/bits: {other:?}"),
    };
    if channels == 0 {
        bail!("zero channels");
    }
    let mono = if channels == 1 {
        samples
    } else {
        samples
            .chunks(channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok(WavData {
        sample_rate: rate,
        samples: mono,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory 16-bit PCM WAV.
    fn wav16(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let data_len = data.len() as u32;
        let byte_rate = rate * u32::from(channels) * 2;
        let block_align = channels * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16_u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn parses_mono_pcm16() {
        let bytes = wav16(&[0, 16_384, -16_384, 0], 16_000, 1);
        let wav = parse_wav(&bytes).expect("parse");
        assert_eq!(wav.sample_rate, 16_000);
        assert_eq!(wav.samples.len(), 4);
        assert!((wav.samples[1] - 0.5).abs() < 0.01);
        assert!((wav.samples[2] + 0.5).abs() < 0.01);
    }

    #[test]
    fn parses_stereo_and_downmixes() {
        let bytes = wav16(&[16_384, 0, 0, 16_384], 48_000, 2);
        let wav = parse_wav(&bytes).expect("parse");
        assert_eq!(wav.sample_rate, 48_000);
        assert_eq!(wav.samples.len(), 2);
        assert!((wav.samples[0] - 0.25).abs() < 0.01);
        assert!((wav.samples[1] - 0.25).abs() < 0.01);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_wav(b"not a wav at all").is_err());
        assert!(parse_wav(&[]).is_err());
    }
}
