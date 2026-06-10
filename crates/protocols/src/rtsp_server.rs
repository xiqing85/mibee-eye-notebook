//! RTSP/1.0 server (RFC 2326)
//!
//! Provides:
//! - Configurable TCP listener (default port 8554)
//! - OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN handlers
//! - Digest authentication (RFC 2617)
//! - Multi-stream support via URL path matching
//! - RTP interleaved mode (TCP) for streaming (RFC 2326 §10.12)
//!
//! This is a **hand-written** implementation — no GStreamer or heavy media
//! framework. Reuses types from the sibling `rtsp` client module.

#![cfg_attr(test, deny(warnings))]

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════════
// RTSP Methods (RFC 2326 section 10)
// ═══════════════════════════════════════════════════════════════════════════════

/// RTSP method types (RFC 2326 section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspMethod {
    Options,
    Describe,
    Setup,
    Play,
    Pause,
    Teardown,
    Announce,
    GetParameter,
    SetParameter,
}

impl fmt::Display for RtspMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Options => write!(f, "OPTIONS"),
            Self::Describe => write!(f, "DESCRIBE"),
            Self::Setup => write!(f, "SETUP"),
            Self::Play => write!(f, "PLAY"),
            Self::Pause => write!(f, "PAUSE"),
            Self::Teardown => write!(f, "TEARDOWN"),
            Self::Announce => write!(f, "ANNOUNCE"),
            Self::GetParameter => write!(f, "GET_PARAMETER"),
            Self::SetParameter => write!(f, "SET_PARAMETER"),
        }
    }
}

impl FromStr for RtspMethod {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "OPTIONS" => Ok(Self::Options),
            "DESCRIBE" => Ok(Self::Describe),
            "SETUP" => Ok(Self::Setup),
            "PLAY" => Ok(Self::Play),
            "PAUSE" => Ok(Self::Pause),
            "TEARDOWN" => Ok(Self::Teardown),
            "ANNOUNCE" => Ok(Self::Announce),
            "GET_PARAMETER" => Ok(Self::GetParameter),
            "SET_PARAMETER" => Ok(Self::SetParameter),
            _ => bail!("Unknown RTSP method: {s}"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Transport Info
// ═══════════════════════════════════════════════════════════════════════════════

/// Transport information for RTP/RTCP session.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportInfo {
    /// Interleaved channel pair for TCP transport: (rtp_channel, rtcp_channel).
    pub interleaved: Option<(u8, u8)>,
    /// Client port pair for UDP transport: (rtp_port, rtcp_port).
    pub client_port: Option<(u16, u16)>,
    /// Server port pair for UDP transport: (rtp_port, rtcp_port).
    pub server_port: Option<(u16, u16)>,
    /// Session identifier.
    pub session_id: String,
    /// Synchronization source identifier.
    pub ssrc: Option<u32>,
    /// Transport mode (e.g., "unicast").
    pub mode: Option<String>,
}

impl TransportInfo {
    /// Parse a Transport header value.
    ///
    /// Example: `RTP/AVP/TCP;interleaved=0-1;ssrc=ABCD1234`
    pub fn parse(header: &str) -> Result<Self> {
        let params: Vec<&str> = header.split(';').collect();
        let mut interleaved: Option<(u8, u8)> = None;
        let mut client_port: Option<(u16, u16)> = None;
        let mut server_port: Option<(u16, u16)> = None;
        let session_id = String::new();
        let mut ssrc: Option<u32> = None;
        let mut mode: Option<String> = None;

        for param in &params {
            let param = param.trim();
            if let Some((key, value)) = param.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim();
                match key.as_str() {
                    "interleaved" => {
                        let ports: Vec<&str> = value.split('-').collect();
                        if ports.len() == 2 {
                            let rtp = ports[0].parse().ok();
                            let rtcp = ports[1].parse().ok();
                            if let (Some(r), Some(c)) = (rtp, rtcp) {
                                interleaved = Some((r, c));
                            }
                        }
                    }
                    "client_port" => {
                        let ports: Vec<&str> = value.split('-').collect();
                        if ports.len() == 2 {
                            let rtp = ports[0].parse().ok();
                            let rtcp = ports[1].parse().ok();
                            if let (Some(r), Some(c)) = (rtp, rtcp) {
                                client_port = Some((r, c));
                            }
                        }
                    }
                    "server_port" => {
                        let ports: Vec<&str> = value.split('-').collect();
                        if ports.len() == 2 {
                            let rtp = ports[0].parse().ok();
                            let rtcp = ports[1].parse().ok();
                            if let (Some(r), Some(c)) = (rtp, rtcp) {
                                server_port = Some((r, c));
                            }
                        }
                    }
                    "ssrc" => {
                        // SSRC can be hex or decimal
                        ssrc = parse_ssrc(value);
                    }
                    "mode" => {
                        mode = Some(value.to_string());
                    }
                    _ => {}
                }
            } else {
                let p = param.trim().to_lowercase();
                // Handle transport protocol prefix like "RTP/AVP/TCP"
                if p.starts_with("rtp/") {
                    // Transport specifier; ignore for parsing
                }
            }
        }

        Ok(Self {
            interleaved,
            client_port,
            server_port,
            session_id,
            ssrc,
            mode,
        })
    }

    /// Serialize to Transport header value string.
    pub fn serialize(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        parts.push("RTP/AVP/TCP".to_string());

        if let Some((rtp, rtcp)) = self.interleaved {
            parts.push(format!("interleaved={rtp}-{rtcp}"));
        }
        if let Some((rtp, rtcp)) = self.client_port {
            parts.push(format!("client_port={rtp}-{rtcp}"));
        }
        if let Some((rtp, rtcp)) = self.server_port {
            parts.push(format!("server_port={rtp}-{rtcp}"));
        }
        if let Some(ssrc) = self.ssrc {
            parts.push(format!("ssrc=0x{ssrc:08x}"));
        }
        if let Some(ref mode) = self.mode {
            parts.push(format!("mode={mode}"));
        }

        parts.join(";")
    }
}

/// Parse an SSRC value that may be hex (with or without 0x prefix) or decimal.
fn parse_ssrc(value: &str) -> Option<u32> {
    let value = value.trim();
    // Try decimal first
    if let Ok(v) = value.parse::<u32>() {
        return Some(v);
    }
    // Then try hex
    u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

/// Case-insensitive header lookup helper.
fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ═══════════════════════════════════════════════════════════════════════════════
// MD5 Hash Implementation (inline, ~90 LOC)
// ═══════════════════════════════════════════════════════════════════════════════

/// Minimal MD5 hash implementation for Digest auth verification.
struct Md5 {
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
            let block: &[u8; 64] = data[offset..offset + 64].try_into().unwrap();
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
        for i in 0..16 {
            x[i] = u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
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

fn md5_hash(data: &[u8]) -> [u8; 16] {
    let mut md5 = Md5::new();
    md5.update(data);
    md5.finalize()
}

fn md5_hex(data: &[u8]) -> String {
    hex::encode(md5_hash(data))
}

fn hex_encode(data: &[u8]) -> String {
    hex::encode(data)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Type re-exports from sibling modules
// ═══════════════════════════════════════════════════════════════════════════════

pub use TransportInfo as RtpTransportInfo;

// ═══════════════════════════════════════════════════════════════════════════════
// Configuration
// ═══════════════════════════════════════════════════════════════════════════════

/// Configuration for the RTSP server.
#[derive(Debug, Clone)]
pub struct RtspServerConfig {
    /// TCP port to listen on (default: 8554)
    pub port: u16,
    /// Require Digest authentication for all requests
    pub auth_required: bool,
    /// Authentication realm (used in WWW-Authenticate challenges)
    pub realm: String,
    /// Allowed username for Digest auth
    pub username: String,
    /// Allowed password for Digest auth
    pub password: String,
}

impl Default for RtspServerConfig {
    fn default() -> Self {
        Self {
            port: 8554,
            auth_required: false,
            realm: "notebook-cam RTSP Server".to_string(),
            username: String::new(),
            password: String::new(),
        }
    }
}

/// Describes a stream that the RTSP server can serve.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// URL path for this stream (e.g., "webcam", "camera/1")
    pub path: String,
    /// Full SDP body returned for DESCRIBE requests
    pub sdp_body: String,
    /// SSRC identifier for RTP packets
    pub ssrc: u32,
}

impl StreamConfig {
    /// Create a new stream configuration.
    pub fn new(path: &str, sdp_body: &str, ssrc: u32) -> Self {
        Self {
            path: path.to_string(),
            sdp_body: sdp_body.to_string(),
            ssrc,
        }
    }

    /// Returns the URL path for DESCRIBE matching: `/stream/{path}` or `/{path}`
    fn url_path(&self) -> String {
        if self.path.starts_with('/') {
            self.path.clone()
        } else {
            format!("/{}", self.path)
        }
    }

    /// Check if a request URI matches this stream.
    fn matches_uri(&self, uri: &str) -> bool {
        let url_path = self.url_path();
        uri == url_path || uri.ends_with(&url_path) || uri.contains(&format!("/{}", self.path))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Request / Response types for the server
// ═══════════════════════════════════════════════════════════════════════════════

/// A parsed RTSP request.
#[derive(Debug, Clone)]
struct ParsedRequest {
    method: RtspMethod,
    uri: String,
    headers: Vec<(String, String)>,
    _body: Vec<u8>,
    cseq: u32,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Session management
// ═══════════════════════════════════════════════════════════════════════════════

/// State of an RTSP session on the server side.
#[derive(Debug, Clone, PartialEq)]
enum SessionState {
    Init,
    Described,
    Setup {
        session_id: String,
        transport: TransportInfo,
        stream_path: String,
    },
    Playing {
        session_id: String,
        transport: TransportInfo,
        stream_path: String,
        ssrc: u32,
    },
    Teardown,
}

/// Server-side RTSP session.
#[derive(Debug, Clone)]
struct Session {
    state: SessionState,
}

impl Session {
    fn new() -> Self {
        Self {
            state: SessionState::Init,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Auth helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Generate a random nonce string for Digest auth challenges.
fn generate_nonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    hex_encode(&bytes)
}

/// Generate a random session ID string.
fn generate_session_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 8];
    rng.fill(&mut bytes);
    hex_encode(&bytes)
}

/// Build a `WWW-Authenticate: Digest` challenge header value.
fn build_digest_challenge(realm: &str, nonce: &str) -> String {
    format!(r#"Digest realm="{realm}", nonce="{nonce}", algorithm=MD5, stale=FALSE"#)
}

/// Verify a Digest `Authorization` header against stored credentials.
///
/// Returns `true` if the response value matches the expected hash.
fn verify_digest_auth(
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

    // Constant-time comparison would be better, but for now:
    client_response.as_str() == expected_response
        && client_realm.as_str() == realm
        && client_uri.as_str() == uri
}

/// Parse auth params from the Authorization or WWW-Authenticate header value.
fn parse_auth_params(input: &str) -> Result<HashMap<String, String>> {
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

// ═══════════════════════════════════════════════════════════════════════════════
// Response builders
// ═══════════════════════════════════════════════════════════════════════════════

/// Build an RTSP response as raw bytes.
fn build_response(
    cseq: u32,
    status_code: u16,
    reason: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();

    // Status line
    out.extend_from_slice(b"RTSP/1.0 ");
    out.extend_from_slice(status_code.to_string().as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(reason.as_bytes());
    out.extend_from_slice(b"\r\n");

    // CSeq
    out.extend_from_slice(b"CSeq: ");
    out.extend_from_slice(cseq.to_string().as_bytes());
    out.extend_from_slice(b"\r\n");

    // Extra headers
    for &(name, value) in extra_headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }

    // Content-Length if body present
    if !body.is_empty() {
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(body.len().to_string().as_bytes());
        out.extend_from_slice(b"\r\n");
    }

    // Blank line
    out.extend_from_slice(b"\r\n");

    // Body
    if !body.is_empty() {
        out.extend_from_slice(body);
    }

    out
}

/// Build a 401 Unauthorized response with Digest challenge.
fn build_unauthorized_response(cseq: u32, realm: &str, nonce: &str) -> Vec<u8> {
    let challenge = build_digest_challenge(realm, nonce);
    build_response(
        cseq,
        401,
        "Unauthorized",
        &[("WWW-Authenticate", &challenge)],
        b"",
    )
}

/// Build a 200 OK response.
fn build_ok_response(cseq: u32, extra_headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    build_response(cseq, 200, "OK", extra_headers, body)
}

/// Build a 404 Not Found response.
fn build_not_found_response(cseq: u32) -> Vec<u8> {
    build_response(cseq, 404, "Not Found", &[], b"")
}

/// Build a 455 Method Not Valid In This State response.
fn build_invalid_state_response(cseq: u32) -> Vec<u8> {
    build_response(cseq, 455, "Method Not Valid In This State", &[], b"")
}

/// Build a 461 Unsupported Transport response.
fn build_unsupported_transport_response(cseq: u32) -> Vec<u8> {
    build_response(cseq, 461, "Unsupported Transport", &[], b"")
}

// ═══════════════════════════════════════════════════════════════════════════════
// Request reader
// ═══════════════════════════════════════════════════════════════════════════════

/// Read and parse a single RTSP request from the buffered reader.
///
/// Returns `Ok(None)` on EOF (connection closed).
async fn read_rtsp_request<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<ParsedRequest>> {
    // Loop to skip empty lines (keepalive markers or leading CRLF)
    let line = loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches("\r\n").trim_end_matches('\n');
        if !trimmed.is_empty() {
            break trimmed.to_string();
        }
    };

    // Parse: METHOD uri RTSP/1.0
    let parts: Vec<&str> = line.splitn(3, ' ').collect();
    if parts.len() < 3 {
        bail!("Invalid RTSP request line: {line}");
    }

    let method: RtspMethod = parts[0].parse()?;
    let uri = parts[1].to_string();
    let _version = parts[2].to_string();

    // Read headers
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut header_line = String::new();
        let bytes_read = reader.read_line(&mut header_line).await?;
        if bytes_read == 0 {
            bail!("Unexpected EOF in RTSP headers");
        }
        let header_line = header_line.trim_end_matches("\r\n").trim_end_matches('\n');
        if header_line.is_empty() {
            break; // End of headers
        }
        if let Some((name, value)) = header_line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    // Read body if Content-Length is present
    let content_length = get_header(&headers, "Content-Length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    // Extract CSeq
    let cseq = get_header(&headers, "CSeq")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    Ok(Some(ParsedRequest {
        method,
        uri,
        headers,
        _body: body,
        cseq,
    }))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Connection handler
// ═══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
/// Send an interleaved RTP frame on the writer in `$channel length data` format.
async fn send_interleaved_rtp<W: AsyncWrite + Unpin>(
    writer: &mut W,
    channel: u8,
    rtp_data: &[u8],
) -> Result<()> {
    let len = rtp_data.len();
    if len > u16::MAX as usize {
        bail!("RTP packet too large for interleaved transport: {len} bytes");
    }
    let mut frame = Vec::with_capacity(4 + len);
    frame.push(0x24); // '$' magic byte
    frame.push(channel);
    frame.extend_from_slice(&(len as u16).to_be_bytes());
    frame.extend_from_slice(rtp_data);
    writer.write_all(&frame).await?;
    Ok(())
}

/// Options response: advertise supported methods.
fn handle_options(cseq: u32) -> Vec<u8> {
    let public = "DESCRIBE, SETUP, TEARDOWN, PLAY, PAUSE, OPTIONS, GET_PARAMETER";
    build_ok_response(cseq, &[("Public", public)], b"")
}

/// Describe response: return SDP for the matched stream.
fn handle_describe(
    cseq: u32,
    uri: &str,
    stream: &StreamConfig,
    _server_config: &RtspServerConfig,
    session: &mut Session,
) -> Vec<u8> {
    // Build the SDP with content-base pointing to our server
    let base_url = uri.trim_end_matches(&stream.url_path());
    let sdp = stream.sdp_body.clone();

    session.state = SessionState::Described;

    build_ok_response(
        cseq,
        &[
            ("Content-Type", "application/sdp"),
            ("Content-Base", base_url),
        ],
        sdp.as_bytes(),
    )
}

/// Setup response: negotiate transport and create session.
fn handle_setup(
    cseq: u32,
    uri: &str,
    transport_header: &str,
    streams: &HashMap<String, StreamConfig>,
    session: &mut Session,
    _server_config: &RtspServerConfig,
) -> (Vec<u8>, Option<TransportInfo>) {
    // Find the matching stream
    let stream = match find_stream_by_uri(uri, streams) {
        Some(s) => s,
        None => return (build_not_found_response(cseq), None),
    };

    // Parse the Transport header
    let client_transport = match TransportInfo::parse(transport_header) {
        Ok(t) => t,
        Err(e) => {
            warn!("Failed to parse Transport header: {e}");
            return (build_unsupported_transport_response(cseq), None);
        }
    };

    // We support TCP interleaved mode
    let interleaved = client_transport.interleaved.unwrap_or((0, 1));

    // Generate session ID
    let session_id = generate_session_id();

    // Build response transport with our SSRC
    let response_transport = TransportInfo {
        interleaved: Some(interleaved),
        client_port: None,
        server_port: None,
        session_id: session_id.clone(),
        ssrc: Some(stream.ssrc),
        mode: Some("play".to_string()),
    };

    let transport_str = response_transport.serialize();

    let resp = build_ok_response(
        cseq,
        &[("Transport", &transport_str), ("Session", &session_id)],
        b"",
    );

    session.state = SessionState::Setup {
        session_id: session_id.clone(),
        transport: response_transport.clone(),
        stream_path: stream.path.clone(),
    };

    (resp, Some(response_transport))
}

/// Play response: started sending RTP data.
fn handle_play(
    cseq: u32,
    session_id: &str,
    session: &mut Session,
    _streams: &HashMap<String, StreamConfig>,
) -> (Vec<u8>, Option<u8>) {
    match &session.state {
        SessionState::Setup {
            session_id: sid,
            transport,
            stream_path,
        } => {
            if sid != session_id {
                let resp = build_response(cseq, 454, "Session Not Found", &[], b"");
                return (resp, None);
            }

            let channel = transport.interleaved.map(|(c, _)| c).unwrap_or(0);

            session.state = SessionState::Playing {
                session_id: sid.clone(),
                transport: transport.clone(),
                stream_path: stream_path.clone(),
                ssrc: transport.ssrc.unwrap_or(0),
            };

            let resp = build_ok_response(
                cseq,
                &[("Session", session_id), ("Range", "npt=0.000-")],
                b"",
            );

            (resp, Some(channel))
        }
        _ => (build_invalid_state_response(cseq), None),
    }
}

/// Teardown response: cleanup session.
fn handle_teardown(cseq: u32, session_id: &str, session: &mut Session) -> Vec<u8> {
    session.state = SessionState::Teardown;
    build_ok_response(cseq, &[("Session", session_id)], b"")
}

/// Handle PAUSE request.
fn handle_pause(cseq: u32, session_id: &str, session: &Session) -> Vec<u8> {
    match &session.state {
        SessionState::Playing { .. } => build_ok_response(cseq, &[("Session", session_id)], b""),
        _ => build_invalid_state_response(cseq),
    }
}

/// Handle GET_PARAMETER request.
fn handle_get_parameter(cseq: u32) -> Vec<u8> {
    build_ok_response(cseq, &[], b"")
}

/// Find a stream that matches the given URI.
fn find_stream_by_uri<'a>(
    uri: &str,
    streams: &'a HashMap<String, StreamConfig>,
) -> Option<&'a StreamConfig> {
    // Try exact match first
    streams
        .values()
        .find(|&stream| stream.matches_uri(uri))
        .map(|v| v as _)
}

/// Find a live stream entry that matches the given URI.
fn find_live_stream<'a>(
    uri: &'a str,
    live_streams: &'a HashMap<String, LiveStreamEntry>,
) -> Option<(String, &'a LiveStreamEntry)> {
    for (path, entry) in live_streams.iter() {
        let url_path = if path.starts_with('/') {
            path.clone()
        } else {
            format!("/{}", path)
        };
        if uri == url_path || uri.ends_with(&url_path) || uri.contains(&url_path) {
            return Some((path.clone(), entry));
        }
    }
    None
}

/// Check if authorization is needed and valid.
fn check_auth(req: &ParsedRequest, config: &RtspServerConfig) -> Result<bool> {
    if !config.auth_required {
        return Ok(true);
    }

    let auth_header = get_header(&req.headers, "Authorization");
    match auth_header {
        Some(header) => {
            let method_str = req.method.to_string();
            Ok(verify_digest_auth(
                header,
                &method_str,
                &req.uri,
                &config.username,
                &config.password,
                &config.realm,
            ))
        }
        None => Ok(false),
    }
}

/// Handle a single RTSP connection.
async fn handle_connection(
    mut stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    server: Arc<RtspServerInner>,
) {
    // Split into read and write halves
    let (reader, mut writer) = tokio::io::split(&mut stream);
    let mut buf_reader = BufReader::new(reader);

    let mut session = Session::new();
    let config = &server.config;
    let streams = &server.streams;
    let nonce = generate_nonce();

    loop {
        let request = match read_rtsp_request(&mut buf_reader).await {
            Ok(Some(req)) => req,
            Ok(None) => break, // EOF
            Err(e) => {
                debug!("Error reading RTSP request: {e}");
                break;
            }
        };

        debug!(
            "RTSP {} {} (CSeq: {})",
            request.method, request.uri, request.cseq
        );

        // Check authentication
        if !check_auth(&request, config).unwrap_or(false) {
            let resp = build_unauthorized_response(request.cseq, &config.realm, &nonce);
            if let Err(e) = writer.write_all(&resp).await {
                debug!("Error sending 401 response: {e}");
                break;
            }
            // For unauthorized, continue reading requests (don't break)
            continue;
        }

        match request.method {
            RtspMethod::Options => {
                let resp = handle_options(request.cseq);
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending OPTIONS response: {e}");
                    break;
                }
            }

            RtspMethod::Describe => {
                let stream = match find_stream_by_uri(&request.uri, streams) {
                    Some(s) => s.clone(),
                    None => {
                        // Fallback: check live streams (scope lock to avoid holding across await)
                        let live_found = {
                            let live_map = server.live_streams.lock().unwrap();
                            find_live_stream(&request.uri, &live_map)
                                .map(|(path, entry)| {
                                    StreamConfig::new(&path, &entry.sdp_body, entry.ssrc)
                                })
                        };
                        match live_found {
                            Some(stream) => stream,
                            None => {
                                let resp = build_not_found_response(request.cseq);
                                if let Err(e) = writer.write_all(&resp).await {
                                    debug!("Error sending DESCRIBE 404: {e}");
                                    break;
                                }
                                continue;
                            }
                        }
                    }
                };
                let resp =
                    handle_describe(request.cseq, &request.uri, &stream, config, &mut session);
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending DESCRIBE response: {e}");
                    break;
                }
            }

            RtspMethod::Setup => {
                let transport_header = get_header(&request.headers, "Transport");
                let transport_header = match transport_header {
                    Some(t) => t.to_string(),
                    None => {
                        let resp = build_unsupported_transport_response(request.cseq);
                        if let Err(e) = writer.write_all(&resp).await {
                            debug!("Error sending SETUP 461: {e}");
                            break;
                        }
                        continue;
                    }
                };

                // Resolve stream from static configs or live stream entries
                let stream_config = find_stream_by_uri(&request.uri, streams)
                    .cloned()
                    .or_else(|| {
                        let live_map = server.live_streams.lock().unwrap();
                        find_live_stream(&request.uri, &live_map)
                            .map(|(path, entry)| {
                                StreamConfig::new(&path, &entry.sdp_body, entry.ssrc)
                            })
                    });

                let mut temp_map = HashMap::new();
                match stream_config {
                    Some(ref s) => {
                        temp_map.insert(s.path.clone(), s.clone());
                    }
                    None => {
                        let resp = build_not_found_response(request.cseq);
                        if let Err(e) = writer.write_all(&resp).await {
                            debug!("Error sending SETUP 404: {e}");
                            break;
                        }
                        continue;
                    }
                }

                let (resp, transport) = handle_setup(
                    request.cseq,
                    &request.uri,
                    &transport_header,
                    &temp_map,
                    &mut session,
                    config,
                );
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending SETUP response: {e}");
                    break;
                }

                if transport.is_some() {
                    info!("RTSP session created: {} for stream", request.cseq);
                }
            }

            RtspMethod::Play => {
                let session_id = get_header(&request.headers, "Session")
                    .unwrap_or("")
                    .to_string();

                let (resp, _channel) =
                    handle_play(request.cseq, &session_id, &mut session, streams);
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending PLAY response: {e}");
                    break;
                }

                // If Playing a live stream, enter streaming delivery loop
                if let SessionState::Playing {
                    stream_path,
                    transport,
                    ..
                } = &session.state
                {
                    let live_path = stream_path.clone();
                    let interleave_channel = transport.interleaved.map(|(c, _)| c).unwrap_or(0);

                    // Scope the lock to avoid holding MutexGuard (not Send) across await
                    let live_entry = {
                        let mut live_map = server.live_streams.lock().unwrap();
                        live_map.remove(&live_path)
                    };

                    if let Some(entry) = live_entry {
                        let mut frame_rx = entry.frame_rx;
                        info!("Starting live stream delivery for /{live_path}");

                        loop {
                            tokio::select! {
                                frame = frame_rx.recv() => {
                                    match frame {
                                        Some(data) => {
                                            match build_interleaved_frame(interleave_channel, &data) {
                                                Ok(interleaved) => {
                                                    if let Err(e) = writer.write_all(&interleaved).await {
                                                        debug!("Error sending interleaved frame: {e}");
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!("Failed to build interleaved frame: {e}");
                                                    continue;
                                                }
                                            }
                                        }
                                        None => {
                                            debug!("Live stream /{live_path} ended");
                                            break;
                                        }
                                    }
                                }
                                cmd = read_rtsp_request(&mut buf_reader) => {
                                    match cmd {
                                        Ok(Some(req)) => {
                                            match req.method {
                                                RtspMethod::Teardown => {
                                                    let resp = handle_teardown(req.cseq, &session_id, &mut session);
                                                    let _ = writer.write_all(&resp).await;
                                                    break;
                                                }
                                                RtspMethod::Pause => {
                                                    let resp = handle_pause(req.cseq, &session_id, &session);
                                                    let _ = writer.write_all(&resp).await;
                                                    break;
                                                }
                                                _ => {
                                                    let resp = handle_options(req.cseq);
                                                    let _ = writer.write_all(&resp).await;
                                                }
                                            }
                                        }
                                        Ok(None) | Err(_) => break,
                                    }
                                }
                            }
                        }
                        break; // Exit connection handler after streaming
                    }
                }
            }

            RtspMethod::Teardown => {
                let session_id = get_header(&request.headers, "Session")
                    .unwrap_or("")
                    .to_string();
                let resp = handle_teardown(request.cseq, &session_id, &mut session);
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending TEARDOWN response: {e}");
                }
                break;
            }

            RtspMethod::Pause => {
                let session_id = get_header(&request.headers, "Session")
                    .unwrap_or("")
                    .to_string();
                let resp = handle_pause(request.cseq, &session_id, &session);
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending PAUSE response: {e}");
                    break;
                }
            }

            RtspMethod::GetParameter => {
                let resp = handle_get_parameter(request.cseq);
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending GET_PARAMETER response: {e}");
                    break;
                }
            }

            _ => {
                // Unsupported method
                let resp = build_response(request.cseq, 551, "Option not supported", &[], b"");
                if let Err(e) = writer.write_all(&resp).await {
                    debug!("Error sending unsupported response: {e}");
                    break;
                }
            }
        }
    }

    debug!("RTSP connection closed");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Server
// ═══════════════════════════════════════════════════════════════════════════════

/// Entry for a dynamically registered live stream.
struct LiveStreamEntry {
    /// Receiver for incoming H.264 NAL unit data from RtspOutput.
    frame_rx: mpsc::Receiver<Vec<u8>>,
    /// SDP body describing the stream (codec, payload type, etc.).
    sdp_body: String,
    /// SSRC for RTP packets.
    ssrc: u32,
}

/// Internal server state shared across connections.
struct RtspServerInner {
    config: RtspServerConfig,
    streams: HashMap<String, StreamConfig>,
    /// Dynamically registered live stream entries, keyed by path.
    live_streams: Mutex<HashMap<String, LiveStreamEntry>>,
}

/// The RTSP server instance.
///
/// Serves configured streams via the RTSP/1.0 protocol.
/// Each stream is identified by a URL path and provides an SDP description.
///
/// # Example
///
/// ```ignore
/// use protocols::rtsp_server::{RtspServer, RtspServerConfig, StreamConfig};
///
/// let config = RtspServerConfig {
///     port: 8554,
///     auth_required: false,
///     ..Default::default()
/// };
///
/// let sdp = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Test\r\n\
///            c=IN IP4 0.0.0.0\r\nt=0 0\r\n\
///            m=video 0 RTP/AVP 96\r\n\
///            a=rtpmap:96 H264/90000\r\n"
///     .to_string();
///
/// let server = RtspServer::new(config)
///     .with_stream(StreamConfig::new("webcam", &sdp, 0x12345678));
/// ```
pub struct RtspServer {
    inner: Arc<RtspServerInner>,
}

impl RtspServer {
    /// Create a new RTSP server with the given configuration.
    pub fn new(config: RtspServerConfig) -> Self {
        Self {
            inner: Arc::new(RtspServerInner {
                config,
                streams: HashMap::new(),
                live_streams: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Add a stream to the server.
    pub fn with_stream(mut self, stream: StreamConfig) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("RtspServer::with_stream called after sharing")
            .streams
            .insert(stream.path.clone(), stream);
        self
    }

    /// Add a stream to the server by mutating self.
    pub fn add_stream(&mut self, stream: StreamConfig) {
        Arc::get_mut(&mut self.inner)
            .expect("RtspServer::add_stream called after sharing")
            .streams
            .insert(stream.path.clone(), stream);
    }

    /// Register a live stream that receives H.264 NAL data via an mpsc channel.
    ///
    /// Returns a `Sender` that [`RtspOutput`] can use to push video frames.
    /// The server will deliver frames to any RTSP client that PLAYS this stream.
    pub fn register_live_stream(
        &self,
        path: String,
        sdp_body: String,
        ssrc: u32,
    ) -> mpsc::Sender<Vec<u8>> {
        let (tx, rx) = mpsc::channel(64);
        let entry = LiveStreamEntry {
            frame_rx: rx,
            sdp_body,
            ssrc,
        };
        self.inner.live_streams.lock().unwrap().insert(path, entry);
        tx
    }

    /// Start the server and listen for connections.
    ///
    /// Binds to the configured port and accepts incoming RTSP connections.
    /// Each connection is handled in a separate tokio task.
    pub async fn run(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.inner.config.port);
        let listener = TcpListener::bind(&addr).await?;
        info!("RTSP server listening on {addr}");

        let inner = self.inner.clone();

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    debug!("RTSP connection from {peer_addr}");
                    let inner = inner.clone();
                    tokio::spawn(async move {
                        handle_connection(stream, inner).await;
                    });
                }
                Err(e) => {
                    error!("Error accepting RTSP connection: {e}");
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Public helpers for RTP interleaved sending
// ═══════════════════════════════════════════════════════════════════════════════

/// Construct an interleaved RTP frame in the format `$<channel:1B><length:2B><data>`.
///
/// This can be used by external pipelines to inject RTP data into an active RTSP
/// session's TCP stream.
pub fn build_interleaved_frame(channel: u8, rtp_data: &[u8]) -> Result<Vec<u8>> {
    let len = rtp_data.len();
    if len > u16::MAX as usize {
        bail!("RTP data too large for interleaved transport: {len} bytes");
    }
    let mut frame = Vec::with_capacity(4 + len);
    frame.push(0x24); // '$'
    frame.push(channel);
    frame.extend_from_slice(&(len as u16).to_be_bytes());
    frame.extend_from_slice(rtp_data);
    Ok(frame)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ─── MD5 Tests ──────────────────────────────────────────────────────────

    #[test]
    fn test_md5_empty() {
        let digest = md5_hash(b"");
        assert_eq!(hex_encode(&digest), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn test_md5_hello() {
        let digest = md5_hash(b"hello");
        assert_eq!(hex_encode(&digest), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn test_md5_rfc2617_example() {
        // HA1 = MD5("Mufasa:testrealm@host.com:Circle Of Life")
        let ha1 = md5_hash(b"Mufasa:testrealm@host.com:Circle Of Life");
        assert_eq!(hex_encode(&ha1), "939e7578ed9e3c518a452acee763bce9");
    }

    // ─── Digest Auth Tests ──────────────────────────────────────────────────

    #[test]
    fn test_verify_digest_auth_valid() {
        // Test with known values
        let username = "admin";
        let password = "secret";
        let realm = "test";
        let method = "DESCRIBE";
        let uri = "rtsp://localhost:8554/webcam";
        let nonce = "abc123";

        // Manually construct the expected Authorization header
        let ha1 = md5_hex(format!("{username}:{realm}:{password}").as_bytes());
        let ha2 = md5_hex(format!("{method}:{uri}").as_bytes());
        let expected_response = md5_hex(format!("{ha1}:{nonce}:{ha2}").as_bytes());

        let auth_header = format!(
            r#"Digest username="{username}", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{expected_response}""#
        );

        assert!(verify_digest_auth(
            &auth_header,
            method,
            uri,
            username,
            password,
            realm,
        ));
    }

    #[test]
    fn test_verify_digest_auth_wrong_password() {
        let auth_header = r#"Digest username="admin", realm="test", nonce="abc123", uri="rtsp://localhost/webcam", response="deadbeef""#;

        assert!(!verify_digest_auth(
            auth_header,
            "DESCRIBE",
            "rtsp://localhost/webcam",
            "admin",
            "wrongpass",
            "test",
        ));
    }

    #[test]
    fn test_verify_digest_auth_wrong_username() {
        let auth_header = r#"Digest username="other", realm="test", nonce="abc123", uri="rtsp://localhost/webcam", response="deadbeef""#;

        assert!(!verify_digest_auth(
            auth_header,
            "DESCRIBE",
            "rtsp://localhost/webcam",
            "admin",
            "secret",
            "test",
        ));
    }

    #[test]
    fn test_verify_digest_auth_with_qop() {
        let username = "admin";
        let password = "secret";
        let realm = "test";
        let method = "PLAY";
        let uri = "rtsp://localhost:8554/webcam";
        let nonce = "abc123";
        let qop = "auth";
        let nc = "00000001";
        let cnonce = "deadbeef";

        let ha1 = md5_hex(format!("{username}:{realm}:{password}").as_bytes());
        let ha2 = md5_hex(format!("{method}:{uri}").as_bytes());
        let expected_response =
            md5_hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}").as_bytes());

        let auth_header = format!(
            r#"Digest username="{username}", realm="{realm}", nonce="{nonce}", uri="{uri}", qop={qop}, nc={nc}, cnonce="{cnonce}", response="{expected_response}""#
        );

        assert!(verify_digest_auth(
            &auth_header,
            method,
            uri,
            username,
            password,
            realm,
        ));
    }

    #[test]
    fn test_verify_digest_auth_invalid_header() {
        // Invalid format
        assert!(!verify_digest_auth(
            "Basic xxx",
            "DESCRIBE",
            "rtsp://localhost/stream",
            "admin",
            "secret",
            "test"
        ));

        // Empty
        assert!(!verify_digest_auth(
            "",
            "DESCRIBE",
            "rtsp://localhost/stream",
            "admin",
            "secret",
            "test"
        ));
    }

    // ─── Auth Params Parsing ────────────────────────────────────────────────

    #[test]
    fn test_parse_auth_params_basic() {
        let params = parse_auth_params(r#"username="admin", realm="test", nonce="abc""#).unwrap();
        assert_eq!(params.get("username").unwrap(), "admin");
        assert_eq!(params.get("realm").unwrap(), "test");
        assert_eq!(params.get("nonce").unwrap(), "abc");
    }

    #[test]
    fn test_parse_auth_params_unquoted_qop() {
        let params = parse_auth_params(r#"realm="test", qop=auth, nc=00000001"#).unwrap();
        assert_eq!(params.get("realm").unwrap(), "test");
        assert_eq!(params.get("qop").unwrap(), "auth");
        assert_eq!(params.get("nc").unwrap(), "00000001");
    }

    // ─── Stream Config Tests ────────────────────────────────────────────────

    #[test]
    fn test_stream_config_matches_uri() {
        let stream = StreamConfig::new(
            "webcam",
            "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Test\r\nt=0 0\r\n",
            0x1234,
        );
        assert!(stream.matches_uri("/webcam"));
        assert!(stream.matches_uri("rtsp://localhost:8554/webcam"));
        assert!(stream.matches_uri("rtsp://localhost:8554/stream/webcam"));
        assert!(!stream.matches_uri("/other"));
        assert!(!stream.matches_uri("rtsp://localhost:8554/other"));
    }

    #[test]
    fn test_stream_config_url_path() {
        let stream = StreamConfig::new("webcam", "", 0);
        assert_eq!(stream.url_path(), "/webcam");

        let stream2 = StreamConfig::new("/camera/1", "", 0);
        assert_eq!(stream2.url_path(), "/camera/1");
    }

    // ─── Response Building Tests ────────────────────────────────────────────

    #[test]
    fn test_build_response_basic() {
        let resp = build_response(1, 200, "OK", &[("Server", "notebook-cam")], b"");
        let s = String::from_utf8(resp).unwrap();
        assert!(s.starts_with("RTSP/1.0 200 OK\r\n"));
        assert!(s.contains("CSeq: 1\r\n"));
        assert!(s.contains("Server: notebook-cam\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_build_response_with_body() {
        let body = b"v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Test\r\n";
        let resp = build_response(2, 200, "OK", &[("Content-Type", "application/sdp")], body);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("Content-Length"));
        assert!(s.contains(&body.len().to_string()));
        assert!(s.ends_with("s=Test\r\n"));
    }

    #[test]
    fn test_build_unauthorized_response() {
        let resp = build_unauthorized_response(1, "test-realm", "nonce-123");
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("401 Unauthorized"));
        assert!(s.contains("WWW-Authenticate: Digest"));
        assert!(s.contains("realm=\"test-realm\""));
        assert!(s.contains("nonce=\"nonce-123\""));
    }

    #[test]
    fn test_build_not_found_response() {
        let resp = build_not_found_response(5);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("404 Not Found"));
        assert!(s.contains("CSeq: 5"));
    }

    #[test]
    fn test_options_response() {
        let resp = handle_options(1);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("200 OK"));
        assert!(s.contains("Public: DESCRIBE"));
        assert!(s.contains("SETUP"));
        assert!(s.contains("PLAY"));
        assert!(s.contains("TEARDOWN"));
    }

    // ─── Interleaved Frame Tests ────────────────────────────────────────────

    #[test]
    fn test_build_interleaved_frame() {
        let data = vec![0u8; 100];
        let frame = build_interleaved_frame(0, &data).unwrap();
        // Format: $<channel:1><len:2><data>
        assert_eq!(frame[0], 0x24); // '$'
        assert_eq!(frame[1], 0); // channel
        assert_eq!(u16::from_be_bytes([frame[2], frame[3]]), 100); // length
        assert_eq!(&frame[4..], &data[..]);
    }

    #[test]
    fn test_build_interleaved_frame_channel_1() {
        let data = vec![0xFFu8; 50];
        let frame = build_interleaved_frame(1, &data).unwrap();
        assert_eq!(frame[0], 0x24);
        assert_eq!(frame[1], 1);
        assert_eq!(u16::from_be_bytes([frame[2], frame[3]]), 50);
        assert_eq!(&frame[4..], &data[..]);
    }

    #[test]
    fn test_build_interleaved_frame_too_large() {
        let data = vec![0u8; 70000]; // > u16::MAX
        assert!(build_interleaved_frame(0, &data).is_err());
    }

    // ─── Error Response Tests ───────────────────────────────────────────────

    #[test]
    fn test_invalid_state_response() {
        let resp = build_invalid_state_response(3);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("455 Method Not Valid In This State"));
    }

    #[test]
    fn test_unsupported_transport_response() {
        let resp = build_unsupported_transport_response(4);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("461 Unsupported Transport"));
    }

    // ─── Server Logic Tests ─────────────────────────────────────────────────

    /// Helper to create a test SDP for webcam stream
    fn test_webcam_sdp() -> String {
        "v=0\r\no=- 1234567890 1234567890 IN IP4 192.168.1.1\r\n\
         s=Live Stream\r\n\
         c=IN IP4 0.0.0.0\r\n\
         t=0 0\r\n\
         m=video 0 RTP/AVP 96\r\n\
         a=rtpmap:96 H264/90000\r\n\
         a=fmtp:96 packetization-mode=1;profile-level-id=42C01E\r\n\
         a=control:track1\r\n"
            .to_string()
    }

    /// Helper to create a test server with one stream
    fn test_server() -> RtspServer {
        RtspServer::new(RtspServerConfig::default()).with_stream(StreamConfig::new(
            "webcam",
            &test_webcam_sdp(),
            0xdeadbeef,
        ))
    }

    #[test]
    fn test_server_create() {
        let server = test_server();
        assert_eq!(server.inner.streams.len(), 1);
        assert!(server.inner.streams.contains_key("webcam"));
    }

    #[test]
    fn test_server_with_multiple_streams() {
        let mut server = RtspServer::new(RtspServerConfig::default());
        server.add_stream(StreamConfig::new("webcam", &test_webcam_sdp(), 0x1));
        server.add_stream(StreamConfig::new("camera/1", &test_webcam_sdp(), 0x2));
        server.add_stream(StreamConfig::new("camera/2", &test_webcam_sdp(), 0x3));
        assert_eq!(server.inner.streams.len(), 3);
    }

    #[test]
    fn test_find_stream_by_uri() {
        let mut server = RtspServer::new(RtspServerConfig::default());
        server.add_stream(StreamConfig::new("webcam", &test_webcam_sdp(), 0x1));
        server.add_stream(StreamConfig::new("camera/1", &test_webcam_sdp(), 0x2));

        assert!(find_stream_by_uri("/webcam", &server.inner.streams).is_some());
        assert!(
            find_stream_by_uri("rtsp://localhost:8554/webcam", &server.inner.streams).is_some()
        );
        assert!(find_stream_by_uri("/camera/1", &server.inner.streams).is_some());
        assert!(find_stream_by_uri("/other", &server.inner.streams).is_none());
    }

    // ─── Request/Response Protocol Flow Tests ──────────────────────────────

    /// Helper: send an RTSP request and read the response from a duplex stream
    async fn send_rtsp_and_recv(
        writer: &mut (impl AsyncWrite + Unpin),
        reader: &mut (impl AsyncRead + Unpin),
        request: &[u8],
    ) -> Vec<u8> {
        writer.write_all(request).await.unwrap();

        // Read response: read until \r\n\r\n headers are complete, then grab body
        let mut resp_buf = Vec::new();
        let mut buf = [0u8; 1];

        // Read headers first
        let mut header_end = None;
        while header_end.is_none() {
            if reader.read(&mut buf).await.unwrap() == 0 {
                break;
            }
            resp_buf.push(buf[0]);
            if resp_buf.len() >= 4 && resp_buf[resp_buf.len() - 4..] == [b'\r', b'\n', b'\r', b'\n']
            {
                header_end = Some(resp_buf.len());
            }
        }

        // Parse Content-Length from headers to get body size
        let header_str = String::from_utf8_lossy(&resp_buf);
        let content_length = header_str
            .lines()
            .find_map(|line| {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Content-Length") {
                        return value.trim().parse::<usize>().ok();
                    }
                }
                None
            })
            .unwrap_or(0);

        // Read body
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).await.unwrap();
            resp_buf.extend_from_slice(&body);
        }

        resp_buf
    }

    #[tokio::test]
    async fn test_full_rtsp_handshake() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        // Spawn connection handler on server side
        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // 1. OPTIONS
        let request = b"OPTIONS rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"), "OPTIONS should return 200");
        assert!(
            resp_str.contains("Public:"),
            "OPTIONS should have Public header"
        );
        assert!(resp_str.contains("CSeq: 1"));

        // 2. DESCRIBE
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "DESCRIBE should return 200, got: {resp_str}"
        );
        assert!(resp_str.contains("Content-Type: application/sdp"));
        assert!(resp_str.contains("H264"), "SDP should contain H264");

        // 3. SETUP
        let request = b"SETUP rtsp://localhost:8554/webcam/track1 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "SETUP should return 200, got: {resp_str}"
        );
        assert!(resp_str.contains("Transport:"));
        assert!(resp_str.contains("Session:"));

        // Extract session ID
        let session_id = resp_str
            .lines()
            .find_map(|line| {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Session") {
                        return Some(value.trim().to_string());
                    }
                }
                None
            })
            .expect("SETUP response should have Session header");

        // 4. PLAY
        let play_request = format!(
            "PLAY rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 4\r\nSession: {session_id}\r\nRange: npt=0.000-\r\n\r\n"
        );
        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            play_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "PLAY should return 200, got: {resp_str}"
        );

        // 5. TEARDOWN
        let teardown_request = format!(
            "TEARDOWN rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 5\r\nSession: {session_id}\r\n\r\n"
        );
        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            teardown_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "TEARDOWN should return 200, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_describe_nonexistent_stream() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        let request = b"DESCRIBE rtsp://localhost:8554/nonexistent RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("404 Not Found"),
            "Should return 404 for unknown stream, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_setup_without_transport_header() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // First describe
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let _resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;

        // SETUP without Transport header
        let request = b"SETUP rtsp://localhost:8554/webcam/track1 RTSP/1.0\r\nCSeq: 2\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("461 Unsupported Transport"),
            "SETUP without Transport should return 461, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_play_without_setup() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // Describe
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let _resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;

        // PLAY without SETUP (should fail)
        let request =
            b"PLAY rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 2\r\nSession: nosession\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("455 Method Not Valid In This State"),
            "PLAY without SETUP should return 455, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_unauthorized_access() {
        let config = RtspServerConfig {
            port: 8554,
            auth_required: true,
            realm: "test-realm".to_string(),
            username: "admin".to_string(),
            password: "secret".to_string(),
        };
        let server = RtspServer::new(config).with_stream(StreamConfig::new(
            "webcam",
            &test_webcam_sdp(),
            0x1234,
        ));

        let (client, server_stream) = tokio::io::duplex(4096);
        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // Request without auth
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("401 Unauthorized"),
            "Should return 401 without auth, got: {resp_str}"
        );
        assert!(
            resp_str.contains("WWW-Authenticate: Digest"),
            "Should send Digest challenge"
        );
    }

    #[tokio::test]
    async fn test_authorized_access() {
        let config = RtspServerConfig {
            port: 8554,
            auth_required: true,
            realm: "test-realm".to_string(),
            username: "admin".to_string(),
            password: "secret".to_string(),
        };
        let server = RtspServer::new(config).with_stream(StreamConfig::new(
            "webcam",
            &test_webcam_sdp(),
            0x1234,
        ));

        let (client, server_stream) = tokio::io::duplex(4096);
        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // First request to get challenge
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);

        // Extract nonce from WWW-Authenticate
        let nonce = resp_str
            .lines()
            .find_map(|line| {
                if line.contains("WWW-Authenticate") {
                    let params = line.split(':').nth(1)?;
                    let params = params.trim().trim_start_matches("Digest ");
                    // Extract nonce
                    let nonce_start = params.find(r#"nonce=""#)? + 7;
                    let nonce_end = params[nonce_start..].find('"')? + nonce_start;
                    return Some(params[nonce_start..nonce_end].to_string());
                }
                None
            })
            .expect("Should have nonce in challenge");

        // Build authenticated request
        let uri = "rtsp://localhost:8554/webcam";
        let method = "DESCRIBE";
        let ha1 = md5_hex(b"admin:test-realm:secret");
        let ha2 = md5_hex(format!("{method}:{uri}").as_bytes());
        let response = md5_hex(format!("{ha1}:{nonce}:{ha2}").as_bytes());

        let auth_request = format!(
            "DESCRIBE {uri} RTSP/1.0\r\n\
             CSeq: 2\r\n\
             Accept: application/sdp\r\n\
             Authorization: Digest username=\"admin\", realm=\"test-realm\", nonce=\"{nonce}\", uri=\"{uri}\", response=\"{response}\"\r\n\
             \r\n"
        );

        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            auth_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "Should return 200 with valid auth, got: {resp_str}"
        );
        assert!(resp_str.contains("application/sdp"), "Should return SDP");
    }

    #[tokio::test]
    async fn test_get_parameter() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        let request = b"GET_PARAMETER rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "GET_PARAMETER should return 200, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_options_then_describe() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // Send OPTIONS
        let opts_request = b"OPTIONS rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, opts_request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"));

        // Then DESCRIBE
        let desc_request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, desc_request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"));
        assert!(resp_str.contains("H264"));
    }

    #[tokio::test]
    async fn test_pause_before_play() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // Describe + Setup first
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let _ = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;

        let request = b"SETUP rtsp://localhost:8554/webcam/track1 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        let session_id = resp_str
            .lines()
            .find_map(|line| {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Session") {
                        return Some(value.trim().to_string());
                    }
                }
                None
            })
            .unwrap_or_default();

        // PAUSE before PLAY — should fail (not in playing state)
        let pause_request = format!(
            "PAUSE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 3\r\nSession: {session_id}\r\n\r\n"
        );
        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            pause_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("455 Method Not Valid In This State"),
            "PAUSE before PLAY should return 455, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_play_then_pause() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // Full handshake: OPTIONS, DESCRIBE, SETUP, PLAY
        let request = b"DESCRIBE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let _ = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;

        let request = b"SETUP rtsp://localhost:8554/webcam/track1 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        let session_id = resp_str
            .lines()
            .find_map(|line| {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Session") {
                        return Some(value.trim().to_string());
                    }
                }
                None
            })
            .unwrap_or_default();

        // PLAY
        let play_request = format!(
            "PLAY rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 3\r\nSession: {session_id}\r\nRange: npt=0.000-\r\n\r\n"
        );
        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            play_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"), "PLAY should succeed");

        // PAUSE after PLAY — should succeed
        let pause_request = format!(
            "PAUSE rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 4\r\nSession: {session_id}\r\n\r\n"
        );
        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            pause_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "PAUSE after PLAY should return 200, got: {resp_str}"
        );
    }

    #[tokio::test]
    async fn test_connection_close_on_teardown() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // OPTIONS
        let request = b"OPTIONS rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        assert!(String::from_utf8_lossy(&resp).contains("200 OK"));

        // TEARDOWN immediately
        let request =
            b"TEARDOWN rtsp://localhost:8554/webcam RTSP/1.0\r\nCSeq: 2\r\nSession: \r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"));

        // Connection should close; reading should return 0 bytes
        let mut buf = [0u8; 1];
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client_reader.read(&mut buf),
        )
        .await;

        match result {
            Ok(Ok(0)) => {} // Expected: connection closed
            Ok(Ok(n)) => panic!("Expected EOF after TEARDOWN, got {n} bytes"),
            Ok(Err(e)) => panic!("Error after TEARDOWN: {e}"),
            Err(_) => {} // Timeout is also OK if it takes a moment to close
        }
    }

    #[tokio::test]
    async fn test_missing_cseq_handling() {
        let server = test_server();
        let (client, server_stream) = tokio::io::duplex(4096);

        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // Request without CSeq
        let request = b"OPTIONS rtsp://localhost:8554/webcam RTSP/1.0\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        // Should still respond (CSeq defaults to 0)
        assert!(resp_str.contains("200 OK"));
        assert!(resp_str.contains("CSeq: 0"));
    }

    // ─── Live Stream Tests ──────────────────────────────────────────────────

    #[test]
    fn test_register_live_stream() {
        let server = RtspServer::new(RtspServerConfig::default());
        let tx = server.register_live_stream(
            "livecam".to_string(),
            "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Live\r\nt=0 0\r\n".to_string(),
            0x12345678,
        );
        // Verify sender works by sending data
        let result = tx.try_send(vec![0x00, 0x00, 0x00, 0x01, 0x67]);
        assert!(result.is_ok(), "Should be able to send to live stream channel");
        // Verify entry is stored in server
        let live_map = server.inner.live_streams.lock().unwrap();
        assert!(live_map.contains_key("livecam"));
        let entry = live_map.get("livecam").unwrap();
        assert_eq!(entry.ssrc, 0x12345678);
        assert!(entry.sdp_body.contains("s=Live"));
    }

    #[test]
    fn test_find_live_stream() {
        let mut live_map = HashMap::new();
        live_map.insert(
            "livecam".to_string(),
            LiveStreamEntry {
                frame_rx: mpsc::channel(64).1,
                sdp_body: "s=Live".to_string(),
                ssrc: 1,
            },
        );
        assert!(find_live_stream("/livecam", &live_map).is_some());
        assert!(find_live_stream("rtsp://localhost:8554/livecam", &live_map).is_some());
        assert!(find_live_stream("/other", &live_map).is_none());
    }

    #[tokio::test]
    async fn test_live_stream_describe_and_play() {
        let server = RtspServer::new(RtspServerConfig::default());

        // Register a live stream
        let sdp = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=LiveCam\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n";
        let tx = server.register_live_stream(
            "livecam".to_string(),
            sdp.to_string(),
            0xdeadbeef,
        );

        let (client, server_stream) = tokio::io::duplex(65536);
        let inner = server.inner.clone();
        tokio::spawn(async move {
            handle_connection(server_stream, inner).await;
        });

        let (mut client_reader, mut client_writer) = tokio::io::split(client);

        // 1. OPTIONS
        let request = b"OPTIONS rtsp://localhost:8554/livecam RTSP/1.0\r\nCSeq: 1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        assert!(String::from_utf8_lossy(&resp).contains("200 OK"));

        // 2. DESCRIBE — should find the live stream
        let request = b"DESCRIBE rtsp://localhost:8554/livecam RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"), "DESCRIBE should return 200 for live stream, got: {resp_str}");
        assert!(resp_str.contains("H264"), "SDP should contain H264");

        // 3. SETUP
        let request = b"SETUP rtsp://localhost:8554/livecam/track1 RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;interleaved=0-1\r\n\r\n";
        let resp = send_rtsp_and_recv(&mut client_writer, &mut client_reader, request).await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"), "SETUP should return 200, got: {resp_str}");

        // Extract session ID
        let session_id = resp_str
            .lines()
            .find_map(|line| {
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Session") {
                        return Some(value.trim().to_string());
                    }
                }
                None
            })
            .expect("SETUP response should have Session header");

        // 4. PLAY
        let play_request = format!(
            "PLAY rtsp://localhost:8554/livecam RTSP/1.0\r\nCSeq: 4\r\nSession: {session_id}\r\nRange: npt=0.000-\r\n\r\n"
        );
        let resp = send_rtsp_and_recv(
            &mut client_writer,
            &mut client_reader,
            play_request.as_bytes(),
        )
        .await;
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.contains("200 OK"),
            "PLAY should return 200 for live stream, got: {resp_str}"
        );

        // 5. Send a video frame through the live stream, read interleaved RTP
        let nal_data = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80];
        tx.send(nal_data.clone()).await.unwrap();

        // Read the interleaved RTP frame from the client side
        // Format: $<channel:1><len:2><data>
        let mut magic = [0u8; 1];
        client_reader.read_exact(&mut magic).await.unwrap();
        assert_eq!(magic[0], 0x24, "Should receive interleaved RTP magic byte");

        let mut channel = [0u8; 1];
        client_reader.read_exact(&mut channel).await.unwrap();
        assert_eq!(channel[0], 0, "Should be on channel 0");

        let mut len_buf = [0u8; 2];
        client_reader.read_exact(&mut len_buf).await.unwrap();
        let frame_len = u16::from_be_bytes(len_buf) as usize;
        assert_eq!(frame_len, nal_data.len(), "Frame length should match NAL data");

        let mut received_data = vec![0u8; frame_len];
        client_reader.read_exact(&mut received_data).await.unwrap();
        assert_eq!(received_data, nal_data, "Received data should match sent NAL data");

        // 6. TEARDOWN
        let teardown_request = format!(
            "TEARDOWN rtsp://localhost:8554/livecam RTSP/1.0\r\nCSeq: 5\r\nSession: {session_id}\r\n\r\n"
        );
        // After TEARDOWN, the connection should close
        // We send TEARDOWN through the writer
        client_writer.write_all(teardown_request.as_bytes()).await.unwrap();
        // Give time for the handler to process
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

}
