//! Digest authentication helpers (RFC 2617) and inline MD5 implementation.
//!
//! Provides:
//! - Constant-time string comparison
//! - Minimal MD5 hash (no external crypto crate)
//! - Digest auth challenge/response building and verification

use anyhow::{Result, bail};
use std::collections::HashMap;

/// Constant-time string comparison to prevent timing attacks on Digest auth.
/// Returns true if both strings have the same length and all bytes match.
/// The comparison always processes all bytes regardless of early mismatches.
pub(super) fn constant_time_str_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        // Still do a dummy comparison to avoid leaking length information
        let _ = a
            .as_bytes()
            .iter()
            .zip(b.as_bytes().iter())
            .fold(0u8, |acc, (x, y)| acc ^ x ^ y);
        return false;
    }
    a.as_bytes()
        .iter()
        .zip(b.as_bytes().iter())
        .fold(0u8, |acc, (x, y)| acc ^ x ^ y)
        == 0
}

// ═══════════════════════════════════════════════════════════════════════════════
// MD5 Hash Implementation (inline, ~90 LOC)
// ═══════════════════════════════════════════════════════════════════════════════

/// Minimal MD5 hash implementation for Digest auth verification.
pub(super) struct Md5 {
    state: [u32; 4],
    count: u64,
    buffer: [u8; 64],
}

impl Md5 {
    fn new() -> Self {
        Self {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476],
            count: 0,
            buffer: [0u8; 64],
        }
    }

    fn update(&mut self, data: &[u8]) {
        let len = data.len();
        let buffer_used = (self.count as usize) & 0x3F;
        self.count += len as u64;
        let mut offset = 0;

        if buffer_used != 0 {
            let space = 64 - buffer_used;
            let copy_len = space.min(len);
            self.buffer[buffer_used..buffer_used + copy_len].copy_from_slice(&data[..copy_len]);
            offset = copy_len;
            if buffer_used + copy_len == 64 {
                Self::process_block(&mut self.state, &self.buffer);
            } else {
                return;
            }
        }

        while offset + 64 <= len {
            let block: &[u8; 64] = match data[offset..offset + 64].try_into() {
                Ok(b) => b,
                Err(_) => break, // unreachable: loop bound guarantees 64-byte slice
            };
            Self::process_block(&mut self.state, block);
            offset += 64;
        }

        if offset < len {
            let remaining = len - offset;
            self.buffer[..remaining].copy_from_slice(&data[offset..]);
        }
    }

    fn finalize(self) -> [u8; 16] {
        let mut state = self.state;
        let mut buffer = self.buffer;
        let count = self.count;

        let buffer_used = (count as usize) & 0x3F;
        buffer[buffer_used] = 0x80;
        if buffer_used < 56 {
            buffer[buffer_used + 1..56].fill(0);
        } else {
            buffer[buffer_used + 1..64].fill(0);
            Self::process_block(&mut state, &buffer);
            buffer[..56].fill(0);
        }

        let bits = count.wrapping_mul(8);
        buffer[56..64].copy_from_slice(&bits.to_le_bytes());
        Self::process_block(&mut state, &buffer);

        let mut digest = [0u8; 16];
        for (i, &s) in state.iter().enumerate() {
            digest[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
        }
        digest
    }

    #[rustfmt::skip]
    fn process_block(state: &mut [u32; 4], block: &[u8; 64]) {
        const K: [u32; 64] = [
            0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
            0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
            0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
            0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
            0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
            0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
            0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
            0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
        ];
        const S: [u32; 64] = [
            7, 12, 17, 22,  7, 12, 17, 22,  7, 12, 17, 22,  7, 12, 17, 22,
            5,  9, 14, 20,  5,  9, 14, 20,  5,  9, 14, 20,  5,  9, 14, 20,
            4, 11, 16, 23,  4, 11, 16, 23,  4, 11, 16, 23,  4, 11, 16, 23,
            6, 10, 15, 21,  6, 10, 15, 21,  6, 10, 15, 21,  6, 10, 15, 21,
        ];

        let mut x = [0u32; 16];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            if let Ok(arr) = chunk.try_into() {
                x[i] = u32::from_le_bytes(arr);
            }
        }

        let [mut a, mut b, mut c, mut d] = *state;

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(x[g])
                    .rotate_left(S[i]),
            );
            a = temp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
}

pub(super) fn md5_hash(data: &[u8]) -> [u8; 16] {
    let mut md5 = Md5::new();
    md5.update(data);
    md5.finalize()
}

pub(super) fn md5_hex(data: &[u8]) -> String {
    hex::encode(md5_hash(data))
}

pub(super) fn hex_encode(data: &[u8]) -> String {
    hex::encode(data)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Auth helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Generate a random nonce string for Digest auth challenges.
pub(super) fn generate_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

/// Generate a random session ID string.
pub(super) fn generate_session_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

/// Build a `WWW-Authenticate: Digest` challenge header value.
pub(super) fn build_digest_challenge(realm: &str, nonce: &str) -> String {
    format!(r#"Digest realm="{realm}", nonce="{nonce}", algorithm=MD5, stale=FALSE"#)
}

/// Verify a Digest `Authorization` header against stored credentials.
///
/// Returns `true` if the response value matches the expected hash.
pub(super) fn verify_digest_auth(
    auth_header: &str,
    method: &str,
    uri: &str,
    username: &str,
    password: &str,
    realm: &str,
) -> bool {
    // Parse the Authorization header parameters
    let header = auth_header.trim();
    let params_str = if let Some(digest_str) = header.strip_prefix("Digest") {
        digest_str.trim()
    } else if let Some(digest_str) = header.strip_prefix("digest") {
        digest_str.trim()
    } else {
        return false;
    };

    let params = match parse_auth_params(params_str) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let client_username = match params.get("username") {
        Some(u) => u,
        None => return false,
    };

    // Verify username matches
    if client_username != username {
        return false;
    }

    let client_realm = match params.get("realm") {
        Some(r) => r,
        None => return false,
    };

    // Realm should match (or at least be present)
    let client_nonce = match params.get("nonce") {
        Some(n) => n,
        None => return false,
    };

    let client_uri = match params.get("uri") {
        Some(u) => u,
        None => return false,
    };

    let client_response = match params.get("response") {
        Some(r) => r,
        None => return false,
    };

    let ha1 = md5_hex(format!("{username}:{realm}:{password}").as_bytes());
    let ha2 = md5_hex(format!("{method}:{uri}").as_bytes());

    let expected_response = match params.get("qop") {
        Some(qop) => {
            let nc = params.get("nc").map(|s| s.as_str()).unwrap_or("00000001");
            let cnonce = params.get("cnonce").map(|s| s.as_str()).unwrap_or("");
            md5_hex(format!("{ha1}:{client_nonce}:{nc}:{cnonce}:{qop}:{ha2}").as_bytes())
        }
        None => md5_hex(format!("{ha1}:{client_nonce}:{ha2}").as_bytes()),
    };

    constant_time_str_eq(client_response.as_str(), &expected_response)
        && constant_time_str_eq(client_realm.as_str(), realm)
        && constant_time_str_eq(client_uri.as_str(), uri)
}

/// Parse auth params from the Authorization or WWW-Authenticate header value.
pub(super) fn parse_auth_params(input: &str) -> Result<HashMap<String, String>> {
    let mut params = HashMap::new();
    let mut remaining = input.trim();

    while !remaining.is_empty() {
        remaining = remaining.trim();
        if let Some(eq_pos) = remaining.find('=') {
            let key = remaining[..eq_pos].trim().to_string();
            remaining = remaining[eq_pos + 1..].trim();

            if remaining.starts_with('"') {
                remaining = &remaining[1..];
                if let Some(end_quote) = remaining.find('"') {
                    let value = remaining[..end_quote].to_string();
                    params.insert(key, value);
                    remaining = remaining[end_quote + 1..].trim();
                    remaining = remaining.strip_prefix(',').unwrap_or(remaining).trim();
                } else {
                    bail!("Unterminated quoted string in auth params");
                }
            } else {
                let end = remaining.find([',', ' ']).unwrap_or(remaining.len());
                let value = remaining[..end].to_string();
                params.insert(key, value);
                remaining = remaining[end..].trim();
                remaining = remaining.strip_prefix(',').unwrap_or(remaining).trim();
            }
        } else {
            break;
        }
    }

    Ok(params)
}
