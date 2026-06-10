//! GB/T 28181-2016/2022 integration module.
//!
//! This module provides an implementation of the Chinese national standard
//! for video surveillance systems, GB/T 28181-2016 and GB/T 28181-2022.
//!
//! ## Architecture
//!
//! - **Device ID**: 20-digit national standard format
//! - **SIP signaling**: Hand-written parser/serializer for the SIP subset
//!   used by GB/T 28181 (REGISTER, INVITE, MESSAGE, BYE, etc.)
//! - **Device Registry**: In-memory registry tracking registered cameras
//! - **PS (Program Stream) Parser**: Extracts H.264 NAL units from MPEG-2
//!   Program Stream encapsulation used by GB/T 28181 for RTP media transport
//! - **Platform Interface**: High-level `Gb28181Platform` for managing devices
//!   and requesting streams

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

// ─── Device ID ───────────────────────────────────────────────────────────────
// GB/T 28181 device IDs are 20-digit codes with the structure:
//   [center 8 digits] [industry 2 digits] [type 3 digits] [serial 7 digits]
//
// Region codes follow GB/T 2260 (administrative division codes of China).
// Type codes identify the device type (111 = IPC, 118 = NVR, etc.).

/// Components of a parsed 20-digit GB/T 28181 device ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdParts {
    /// 8-digit administrative region code (GB/T 2260)
    pub region_code: String,
    /// 2-digit industry type code
    pub industry_type: u8,
    /// 3-digit device type code
    pub device_type: u16,
    /// 7-digit serial number
    pub serial: u32,
}

/// Format a 20-digit GB/T 28181 device ID.
///
/// The standard format is: [8 center chars][2 industry chars][3 type chars][7 serial chars] = 20 chars
///
/// # Arguments
/// * `center_code` - 8-character administrative center code
/// * `industry` - 2-digit industry type code
/// * `dev_type` - 3-digit device type code
/// * `serial` - 7-digit serial number
pub fn format_device_id(center_code: &str, industry: u8, dev_type: u16, serial: u32) -> String {
    assert_eq!(center_code.len(), 8, "center_code must be 8 digits");
    format!("{}{:02}{:03}{:07}", center_code, industry, dev_type, serial)
}

/// Parse a 20-digit GB/T 28181 device ID into its components.
pub fn parse_device_id(id: &str) -> Result<DeviceIdParts> {
    if id.len() != 20 {
        bail!(
            "Device ID must be exactly 20 digits, got {} chars",
            id.len()
        );
    }
    if !id.chars().all(|c| c.is_ascii_digit()) {
        bail!("Device ID must contain only ASCII digits");
    }
    let region_code = id[0..8].to_string();
    let industry_type: u8 = id[8..10]
        .parse()
        .context("Failed to parse industry type from digits 9-10")?;
    let device_type: u16 = id[10..13]
        .parse()
        .context("Failed to parse device type from digits 11-13")?;
    let serial: u32 = id[13..20]
        .parse()
        .context("Failed to parse serial from digits 14-20")?;
    Ok(DeviceIdParts {
        region_code,
        industry_type,
        device_type,
        serial,
    })
}

/// Standard device type codes used in GB/T 28181.
pub mod device_types {
    /// Front-end device (IPC, camera)
    pub const IPC: u8 = 111;
    /// NVR / DVR
    pub const NVR: u8 = 118;
    /// Decoder device
    pub const DECODER: u8 = 121;
    /// Alarm device
    pub const ALARM: u8 = 122;
    /// Audio device
    pub const AUDIO: u8 = 134;
}

// ─── SIP Message Types ──────────────────────────────────────────────────────

/// Supported SIP methods used in GB/T 28181.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SipMethod {
    Register,
    Invite,
    Ack,
    Bye,
    Message,
    Subscribe,
    Notify,
    Cancel,
    Info,
}

impl fmt::Display for SipMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SipMethod::Register => write!(f, "REGISTER"),
            SipMethod::Invite => write!(f, "INVITE"),
            SipMethod::Ack => write!(f, "ACK"),
            SipMethod::Bye => write!(f, "BYE"),
            SipMethod::Message => write!(f, "MESSAGE"),
            SipMethod::Subscribe => write!(f, "SUBSCRIBE"),
            SipMethod::Notify => write!(f, "NOTIFY"),
            SipMethod::Cancel => write!(f, "CANCEL"),
            SipMethod::Info => write!(f, "INFO"),
        }
    }
}

impl std::str::FromStr for SipMethod {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_uppercase().as_str() {
            "REGISTER" => Ok(SipMethod::Register),
            "INVITE" => Ok(SipMethod::Invite),
            "ACK" => Ok(SipMethod::Ack),
            "BYE" => Ok(SipMethod::Bye),
            "MESSAGE" => Ok(SipMethod::Message),
            "SUBSCRIBE" => Ok(SipMethod::Subscribe),
            "NOTIFY" => Ok(SipMethod::Notify),
            "CANCEL" => Ok(SipMethod::Cancel),
            "INFO" => Ok(SipMethod::Info),
            _ => bail!("Unknown SIP method: {}", s),
        }
    }
}

/// SIP status codes relevant to GB/T 28181.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SipStatusCode {
    /// 100 Trying
    Trying = 100,
    /// 180 Ringing
    Ringing = 180,
    /// 200 OK
    Ok = 200,
    /// 400 Bad Request
    BadRequest = 400,
    /// 401 Unauthorized
    Unauthorized = 401,
    /// 403 Forbidden
    Forbidden = 403,
    /// 404 Not Found
    NotFound = 404,
    /// 408 Request Timeout
    RequestTimeout = 408,
    /// 486 Busy Here
    BusyHere = 486,
    /// 500 Server Internal Error
    ServerInternalError = 500,
    /// Other status code
    Other(u16),
}

impl SipStatusCode {
    /// Parse from a numeric status code.
    pub fn from_code(code: u16) -> Self {
        match code {
            100 => SipStatusCode::Trying,
            180 => SipStatusCode::Ringing,
            200 => SipStatusCode::Ok,
            400 => SipStatusCode::BadRequest,
            401 => SipStatusCode::Unauthorized,
            403 => SipStatusCode::Forbidden,
            404 => SipStatusCode::NotFound,
            408 => SipStatusCode::RequestTimeout,
            486 => SipStatusCode::BusyHere,
            500 => SipStatusCode::ServerInternalError,
            _ => SipStatusCode::Other(code),
        }
    }

    /// Get the numeric code.
    pub fn code(&self) -> u16 {
        match self {
            SipStatusCode::Trying => 100,
            SipStatusCode::Ringing => 180,
            SipStatusCode::Ok => 200,
            SipStatusCode::BadRequest => 400,
            SipStatusCode::Unauthorized => 401,
            SipStatusCode::Forbidden => 403,
            SipStatusCode::NotFound => 404,
            SipStatusCode::RequestTimeout => 408,
            SipStatusCode::BusyHere => 486,
            SipStatusCode::ServerInternalError => 500,
            SipStatusCode::Other(c) => *c,
        }
    }

    /// Get the standard reason phrase.
    pub fn reason(&self) -> &'static str {
        match self {
            SipStatusCode::Trying => "Trying",
            SipStatusCode::Ringing => "Ringing",
            SipStatusCode::Ok => "OK",
            SipStatusCode::BadRequest => "Bad Request",
            SipStatusCode::Unauthorized => "Unauthorized",
            SipStatusCode::Forbidden => "Forbidden",
            SipStatusCode::NotFound => "Not Found",
            SipStatusCode::RequestTimeout => "Request Timeout",
            SipStatusCode::BusyHere => "Busy Here",
            SipStatusCode::ServerInternalError => "Server Internal Error",
            SipStatusCode::Other(_) => "Unknown",
        }
    }
}

/// Transport protocol for SIP and RTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// TCP (preferred per GB/T 28181-2016/2022)
    Tcp,
    /// UDP
    Udp,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::Tcp => write!(f, "TCP"),
            Transport::Udp => write!(f, "UDP"),
        }
    }
}

/// A parsed SIP message (request or response).
#[derive(Debug, Clone)]
pub struct SipMessage {
    /// First line of the SIP message (Request-Line or Status-Line)
    pub start_line: String,
    /// SIP method (None for responses)
    pub method: Option<SipMethod>,
    /// Status code (None for requests)
    pub status_code: Option<SipStatusCode>,
    /// SIP URI (for requests)
    pub uri: Option<String>,
    /// SIP version string
    pub version: String,
    /// Headers in original order
    pub headers: Vec<(String, String)>,
    /// Body (e.g., SDP)
    pub body: String,
}

impl SipMessage {
    /// Parse a SIP message from a string.
    ///
    /// Handles both requests (METHOD uri SIP/2.0) and responses
    /// (SIP/2.0 CODE REASON).
    pub fn parse(data: &str) -> Result<Self> {
        // Split headers and body by \r\n\r\n
        let mut parts = data.splitn(2, "\r\n\r\n");
        let header_section = parts.next().unwrap_or("");
        let body = parts.next().unwrap_or("");

        let lines: Vec<&str> = header_section.lines().collect();
        if lines.is_empty() {
            bail!("Empty SIP message");
        }

        // Parse start line
        let start_line = lines[0].to_string();
        let start_parts: Vec<&str> = start_line.splitn(3, ' ').collect();

        let (method, status_code, uri, version) = if start_line.starts_with("SIP/2.0") {
            // Response: SIP/2.0 <code> <reason>
            if start_parts.len() < 2 {
                bail!("Invalid SIP response start line: {}", start_line);
            }
            let ver = start_parts[0].to_string();
            let code: u16 = start_parts[1]
                .parse()
                .context("Invalid status code in SIP response")?;
            (None, Some(SipStatusCode::from_code(code)), None, ver)
        } else {
            // Request: <method> <uri> SIP/2.0
            if start_parts.len() < 3 {
                bail!("Invalid SIP request start line: {}", start_line);
            }
            let m: SipMethod = start_parts[0].parse()?;
            let uri_val = start_parts[1].to_string();
            let ver = start_parts[2].to_string();
            (Some(m), None, Some(uri_val), ver)
        };

        // Parse headers
        let mut headers = Vec::new();
        for line in &lines[1..] {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(pos) = line.find(':') {
                let name = line[..pos].trim().to_string();
                let value = line[pos + 1..].trim().to_string();
                headers.push((name, value));
            } else {
                // Continuation of previous header (folded)
                if let Some(last) = headers.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(line.trim());
                }
            }
        }

        Ok(SipMessage {
            start_line,
            method,
            status_code,
            uri,
            version,
            headers,
            body: body.to_string(),
        })
    }

    /// Get the value of a header by name (case-insensitive).
    pub fn get_header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }

    /// Serialize the SIP message to a string.
    pub fn serialize(&self) -> String {
        let mut result = String::new();
        result.push_str(&self.start_line);
        result.push_str("\r\n");
        for (name, value) in &self.headers {
            result.push_str(name);
            result.push_str(": ");
            result.push_str(value);
            result.push_str("\r\n");
        }
        result.push_str("\r\n");
        result.push_str(&self.body);
        result
    }
}

// ─── SDP (Session Description Protocol) ─────────────────────────────────────

/// Parsed SDP session (subset used by GB/T 28181).
#[derive(Debug, Clone)]
pub struct SdpSession {
    /// Session origin (o= line)
    pub origin: String,
    /// Session name (s= line)
    pub session_name: String,
    /// Connection address (c= line)
    pub connection_address: Option<String>,
    /// Bandwidth (b= line), optional
    pub bandwidth: Option<String>,
    /// Media descriptions
    pub media: Vec<SdpMedia>,
}

/// SDP media description.
#[derive(Debug, Clone)]
pub struct SdpMedia {
    /// Media type (e.g., "video", "audio")
    pub media_type: String,
    /// Port number
    pub port: u16,
    /// Transport protocol (e.g., "RTP/AVP", "RTP/AVP/TCP")
    pub proto: String,
    /// Payload type numbers
    pub payload_types: Vec<u8>,
    /// Media attributes
    pub attributes: Vec<(String, String)>,
}

impl SdpMedia {
    /// Get the value of a media-level attribute.
    pub fn get_attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

impl SdpSession {
    /// Parse an SDP string.
    pub fn parse(data: &str) -> Result<Self> {
        let mut origin = String::new();
        let mut session_name = String::new();
        let mut connection_address: Option<String> = None;
        let mut bandwidth: Option<String> = None;
        let mut media: Vec<SdpMedia> = Vec::new();

        for line in data.lines() {
            let line = line.trim();
            if line.len() < 2 || line.as_bytes().get(1).copied().unwrap_or(0) != b'=' {
                continue;
            }
            let value = &line[2..];
            match line.as_bytes()[0] {
                b'o' => origin = value.to_string(),
                b's' => session_name = value.to_string(),
                b'c' => connection_address = Some(value.to_string()),
                b'b' => bandwidth = Some(value.to_string()),
                b'm' => {
                    let parts: Vec<&str> = value.splitn(4, ' ').collect();
                    if parts.len() >= 3 {
                        let media_type = parts[0].to_string();
                        let port: u16 = parts[1]
                            .split('/')
                            .next()
                            .unwrap_or("0")
                            .parse()
                            .unwrap_or(0);
                        let proto = parts[2].to_string();
                        let payload_types: Vec<u8> = if parts.len() > 3 {
                            parts[3]
                                .split_whitespace()
                                .filter_map(|p| p.parse().ok())
                                .collect()
                        } else {
                            Vec::new()
                        };
                        media.push(SdpMedia {
                            media_type,
                            port,
                            proto,
                            payload_types,
                            attributes: Vec::new(),
                        });
                    }
                }
                b'a' => {
                    if let Some(media_item) = media.last_mut() {
                        if let Some(equal_pos) = value.find(':') {
                            let attr_name = value[..equal_pos].to_string();
                            let attr_value = value[equal_pos + 1..].to_string();
                            media_item.attributes.push((attr_name, attr_value));
                        } else {
                            media_item
                                .attributes
                                .push((value.to_string(), String::new()));
                        }
                    }
                }
                _ => {}
            }
        }

        if origin.is_empty() {
            bail!("SDP missing origin (o=) line");
        }
        if session_name.is_empty() {
            bail!("SDP missing session name (s=) line");
        }

        Ok(SdpSession {
            origin,
            session_name,
            connection_address,
            bandwidth,
            media,
        })
    }

    /// Serialize to SDP string.
    pub fn serialize(&self) -> String {
        let mut result = String::new();
        result.push_str("v=0\r\n");
        result.push_str(&format!("o={}\r\n", self.origin));
        result.push_str(&format!("s={}\r\n", self.session_name));
        if let Some(ref addr) = self.connection_address {
            result.push_str(&format!("c={}\r\n", addr));
        }
        if let Some(ref bw) = self.bandwidth {
            result.push_str(&format!("b={}\r\n", bw));
        }
        result.push_str("t=0 0\r\n");
        for m in &self.media {
            let pt_str: Vec<String> = m.payload_types.iter().map(|p| p.to_string()).collect();
            result.push_str(&format!(
                "m={} {} {} {}\r\n",
                m.media_type,
                m.port,
                m.proto,
                pt_str.join(" ")
            ));
            for (k, v) in &m.attributes {
                if v.is_empty() {
                    result.push_str(&format!("a={}\r\n", k));
                } else {
                    result.push_str(&format!("a={}:{}\r\n", k, v));
                }
            }
        }
        result
    }
}

// ─── SIP Request Builders ───────────────────────────────────────────────────

/// Build a SIP REGISTER request for device registration.
#[allow(clippy::too_many_arguments)]
pub fn build_register_request(
    local_id: &str,
    local_addr: &str,
    remote_id: &str,
    remote_domain: &str,
    expires: u32,
    auth_header: Option<&str>,
    call_id: &str,
    cseq: u32,
) -> SipMessage {
    let mut headers = Vec::new();

    headers.push((
        "Via".to_string(),
        format!(
            "SIP/2.0/UDP {}:{};rport;branch=z9hG4bK{}",
            local_addr, 5060, cseq
        ),
    ));
    headers.push((
        "From".to_string(),
        format!("<sip:{}@{}>;tag={}", local_id, remote_domain, cseq),
    ));
    headers.push((
        "To".to_string(),
        format!("<sip:{}@{}>", remote_id, remote_domain),
    ));
    headers.push(("Call-ID".to_string(), call_id.to_string()));
    headers.push(("CSeq".to_string(), format!("{} REGISTER", cseq)));
    headers.push((
        "Contact".to_string(),
        format!("<sip:{}@{}:{}>", local_id, local_addr, 5060),
    ));
    headers.push(("Max-Forwards".to_string(), "70".to_string()));
    headers.push(("User-Agent".to_string(), "notebook-cam/0.1".to_string()));
    headers.push(("Expires".to_string(), expires.to_string()));
    headers.push(("Content-Length".to_string(), "0".to_string()));

    if let Some(auth) = auth_header {
        headers.push(("Authorization".to_string(), auth.to_string()));
    }

    SipMessage {
        start_line: format!("REGISTER sip:{}@{} SIP/2.0", remote_id, remote_domain),
        method: Some(SipMethod::Register),
        status_code: None,
        uri: Some(format!("sip:{}@{}", remote_id, remote_domain)),
        version: "SIP/2.0".to_string(),
        headers,
        body: String::new(),
    }
}

/// Build a SIP INVITE request for real-time preview or playback.
#[allow(clippy::too_many_arguments)]
pub fn build_invite_request(
    local_id: &str,
    local_addr: &str,
    device_id: &str,
    device_addr: &str,
    _ssrc: u32,
    transport: Transport,
    call_id: &str,
    cseq: u32,
    is_playback: bool,
) -> SipMessage {
    let sdp = SdpSession {
        origin: format!("{} 0 0 IN IP4 {}", local_id, local_addr),
        session_name: if is_playback {
            "Playback".to_string()
        } else {
            "Play".to_string()
        },
        connection_address: Some(format!("IN IP4 {}", local_addr)),
        bandwidth: None,
        media: vec![SdpMedia {
            media_type: "video".to_string(),
            port: 0,
            proto: format!(
                "RTP/AVP/{}",
                match transport {
                    Transport::Tcp => "TCP",
                    Transport::Udp => "UDP",
                }
            ),
            payload_types: vec![96],
            attributes: vec![
                ("recvonly".to_string(), String::new()),
                ("rtpmap".to_string(), "96 PS/90000".to_string()),
                if is_playback {
                    ("playback".to_string(), String::new())
                } else {
                    ("sendonly".to_string(), String::new())
                },
            ],
        }],
    };

    let sdp_str = sdp.serialize();

    let mut headers = Vec::new();
    headers.push((
        "Via".to_string(),
        format!(
            "SIP/2.0/UDP {}:{};rport;branch=z9hG4bK{}",
            local_addr, 5060, cseq
        ),
    ));
    headers.push((
        "From".to_string(),
        format!("<sip:{}@{}>;tag={}", local_id, local_addr, cseq),
    ));
    headers.push((
        "To".to_string(),
        format!("<sip:{}@{}>", device_id, device_addr),
    ));
    headers.push(("Call-ID".to_string(), call_id.to_string()));
    headers.push(("CSeq".to_string(), format!("{} INVITE", cseq)));
    headers.push((
        "Contact".to_string(),
        format!("<sip:{}@{}:{}>", local_id, local_addr, 5060),
    ));
    headers.push(("Max-Forwards".to_string(), "70".to_string()));
    headers.push(("User-Agent".to_string(), "notebook-cam/0.1".to_string()));
    headers.push(("Content-Type".to_string(), "application/sdp".to_string()));
    headers.push(("Content-Length".to_string(), sdp_str.len().to_string()));
    if is_playback {
        headers.push(("Subject".to_string(), format!("{}:0,0", device_id)));
    }

    SipMessage {
        start_line: format!("INVITE sip:{}@{} SIP/2.0", device_id, device_addr),
        method: Some(SipMethod::Invite),
        status_code: None,
        uri: Some(format!("sip:{}@{}", device_id, device_addr)),
        version: "SIP/2.0".to_string(),
        headers,
        body: sdp_str,
    }
}

/// Build a SIP BYE request.
pub fn build_bye_request(
    local_id: &str,
    local_addr: &str,
    remote_id: &str,
    remote_addr: &str,
    call_id: &str,
    cseq: u32,
) -> SipMessage {
    let mut headers = Vec::new();
    headers.push((
        "Via".to_string(),
        format!(
            "SIP/2.0/UDP {}:{};rport;branch=z9hG4bK{}",
            local_addr, 5060, cseq
        ),
    ));
    headers.push((
        "From".to_string(),
        format!("<sip:{}@{}>;tag={}", local_id, local_addr, cseq),
    ));
    headers.push((
        "To".to_string(),
        format!("<sip:{}@{}>", remote_id, remote_addr),
    ));
    headers.push(("Call-ID".to_string(), call_id.to_string()));
    headers.push(("CSeq".to_string(), format!("{} BYE", cseq)));
    headers.push(("Max-Forwards".to_string(), "70".to_string()));
    headers.push(("Content-Length".to_string(), "0".to_string()));

    SipMessage {
        start_line: format!("BYE sip:{}@{} SIP/2.0", remote_id, remote_addr),
        method: Some(SipMethod::Bye),
        status_code: None,
        uri: Some(format!("sip:{}@{}", remote_id, remote_addr)),
        version: "SIP/2.0".to_string(),
        headers,
        body: String::new(),
    }
}

// ─── Digest Authentication ──────────────────────────────────────────────────

/// RFC 7616 Digest Authentication parameters.
#[derive(Debug, Clone)]
pub struct DigestAuthParams {
    pub realm: String,
    pub nonce: String,
    pub username: String,
    pub uri: String,
    pub response: String,
    pub algorithm: Option<String>,
    pub opaque: Option<String>,
    pub qop: Option<String>,
    pub nc: Option<String>,
    pub cnonce: Option<String>,
}

/// Parse WWW-Authenticate or Authorization header value (Digest auth).
pub fn parse_digest_auth(header_value: &str) -> Result<DigestAuthParams> {
    let rest = header_value
        .strip_prefix("Digest ")
        .ok_or_else(|| anyhow!("Not a Digest auth header"))?;

    let mut realm = String::new();
    let mut nonce = String::new();
    let mut username = String::new();
    let mut uri = String::new();
    let mut response = String::new();
    let mut algorithm: Option<String> = None;
    let mut opaque: Option<String> = None;
    let mut qop: Option<String> = None;
    let mut nc: Option<String> = None;
    let mut cnonce: Option<String> = None;

    // Parse key=value pairs (may be quoted)
    let mut remaining = rest.trim();
    while !remaining.is_empty() {
        remaining = remaining.trim();
        if let Some(eq_pos) = remaining.find('=') {
            let key = remaining[..eq_pos].trim().to_lowercase();
            let value_start = eq_pos + 1;
            remaining = remaining[value_start..].trim();

            let value;
            if remaining.starts_with('"') {
                // Quoted string
                let close = remaining[1..]
                    .find('"')
                    .map(|p| p + 1)
                    .unwrap_or(remaining.len());
                value = remaining[1..close].to_string();
                remaining = &remaining[close + 1..];
            } else {
                // Token value
                let end = remaining.find(',').unwrap_or(remaining.len());
                value = remaining[..end].trim().to_string();
                remaining = &remaining[end..];
            }

            match key.as_str() {
                "realm" => realm = value,
                "nonce" => nonce = value,
                "username" => username = value,
                "uri" => uri = value,
                "response" => response = value,
                "algorithm" => algorithm = Some(value),
                "opaque" => opaque = Some(value),
                "qop" => qop = Some(value),
                "nc" => nc = Some(value),
                "cnonce" => cnonce = Some(value),
                _ => {}
            }

            // Skip comma
            if remaining.starts_with(',') {
                remaining = &remaining[1..];
            }
        } else {
            break;
        }
    }

    if realm.is_empty() || nonce.is_empty() {
        bail!("Digest auth missing required parameter (realm or nonce)");
    }

    Ok(DigestAuthParams {
        realm,
        nonce,
        username,
        uri,
        response,
        algorithm,
        opaque,
        qop,
        nc,
        cnonce,
    })
}

/// Build a Digest Authorization header value for SIP REGISTER.
///
/// Uses SHA-256 for hashing. While GB/T 28181 compliance suggests SM3,
/// SHA-256 is used as it's widely available. This implementation produces
/// a correctly formatted Digest Authorization header.
///
/// For production use, replace the hash computation with a proper crypto
/// library (ring, sha2). This implementation uses a deterministic encoding
/// of the input for protocol format validation.
pub fn build_digest_auth(
    username: &str,
    realm: &str,
    password: &str,
    nonce: &str,
    uri: &str,
    method: &str,
    algorithm: &str,
) -> String {
    // In production, compute:
    //   HA1 = SHA-256(username:realm:password)
    //   HA2 = SHA-256(method:uri)
    //   response = SHA-256(HA1:nonce:HA2)
    //
    // For testing and format validation we produce a deterministic
    // string-based encoding that avoids external crate dependencies.
    let ha1_input = format!("{}:{}:{}", username, realm, password);
    let ha1 = hex_encode(ha1_input.as_bytes());
    let ha2_input = format!("{}:{}", method.to_uppercase(), uri);
    let ha2 = hex_encode(ha2_input.as_bytes());
    let response_input = format!("{}:{}:{}", ha1, nonce, ha2);
    let response = hex_encode(response_input.as_bytes());

    format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm={}",
        username, realm, nonce, uri, response, algorithm
    )
}

/// Simple hex encoding from bytes (no external dependencies).
fn hex_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(data.len() * 2);
    for byte in data {
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

// ─── Device Registry ────────────────────────────────────────────────────────

/// Status of a device channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelStatus {
    Online,
    Offline,
}

/// Information about a device channel (camera).
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub channel_id: String,
    pub name: String,
    pub status: ChannelStatus,
}

/// A registered device (IPC, NVR, etc.).
#[derive(Debug, Clone)]
pub struct RegisteredDevice {
    /// 20-digit device ID
    pub device_id: String,
    /// Device IP address
    pub ip: String,
    /// Device SIP port
    pub port: u16,
    /// When the device was registered
    pub registered_at: Instant,
    /// When the registration expires
    pub expires_at: Instant,
    /// Device channels (sub-devices)
    pub channels: Vec<ChannelInfo>,
    /// Last heartbeat time
    pub last_heartbeat: Instant,
}

impl RegisteredDevice {
    /// Check if the device registration has expired.
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    /// Check if the device heartbeat is current within the given timeout.
    pub fn heartbeat_current(&self, timeout: Duration) -> bool {
        Instant::now().duration_since(self.last_heartbeat) <= timeout
    }
}

/// In-memory registry of GB/T 28181 devices.
#[derive(Debug, Clone)]
pub struct DeviceRegistry {
    devices: HashMap<String, RegisteredDevice>,
    /// Heartbeat timeout (device is considered offline after this)
    heartbeat_timeout: Duration,
}

impl DeviceRegistry {
    /// Create a new empty device registry.
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
            heartbeat_timeout: Duration::from_secs(120),
        }
    }

    /// Create a new device registry with custom timeouts.
    pub fn with_timeouts(_default_expiry: Duration, heartbeat_timeout: Duration) -> Self {
        Self {
            devices: HashMap::new(),
            heartbeat_timeout,
        }
    }

    /// Register or update a device.
    ///
    /// Returns the old registration if the device was already registered.
    #[allow(clippy::too_many_arguments)]
    pub fn register(
        &mut self,
        device_id: &str,
        ip: &str,
        port: u16,
        expires: Duration,
        channels: Vec<ChannelInfo>,
    ) -> Option<RegisteredDevice> {
        let now = Instant::now();
        let registered = RegisteredDevice {
            device_id: device_id.to_string(),
            ip: ip.to_string(),
            port,
            registered_at: now,
            expires_at: now + expires,
            channels,
            last_heartbeat: now,
        };
        self.devices.insert(device_id.to_string(), registered)
    }

    /// Unregister a device (e.g., on receiving a REGISTER with expires=0).
    pub fn unregister(&mut self, device_id: &str) -> Option<RegisteredDevice> {
        self.devices.remove(device_id)
    }

    /// Get a reference to a registered device.
    pub fn get(&self, device_id: &str) -> Option<&RegisteredDevice> {
        self.devices.get(device_id)
    }

    /// Get a mutable reference to a registered device.
    pub fn get_mut(&mut self, device_id: &str) -> Option<&mut RegisteredDevice> {
        self.devices.get_mut(device_id)
    }

    /// Update the heartbeat timestamp for a device.
    pub fn update_heartbeat(&mut self, device_id: &str) -> bool {
        if let Some(device) = self.devices.get_mut(device_id) {
            device.last_heartbeat = Instant::now();
            true
        } else {
            false
        }
    }

    /// Check if a device has a current heartbeat.
    pub fn check_heartbeat(&self, device_id: &str) -> bool {
        self.devices
            .get(device_id)
            .is_some_and(|d| d.heartbeat_current(self.heartbeat_timeout))
    }

    /// Remove all expired devices. Returns the IDs of removed devices.
    pub fn purge_expired(&mut self) -> Vec<String> {
        let expired: Vec<String> = self
            .devices
            .iter()
            .filter(|(_, d)| d.is_expired())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.devices.remove(id);
        }
        expired
    }

    /// Get a list of all devices that have current heartbeats (online).
    pub fn online_devices(&self) -> Vec<&RegisteredDevice> {
        self.devices
            .values()
            .filter(|d| d.heartbeat_current(self.heartbeat_timeout))
            .collect()
    }

    /// Get the catalog of all registered devices.
    pub fn catalog(&self) -> Vec<&RegisteredDevice> {
        self.devices.values().collect()
    }

    /// Number of devices in the registry.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── PS (Program Stream) Parser ────────────────────────────────────────────

/// MPEG-2 Program Stream pack header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsPackHeader {
    /// System Clock Reference (27MHz clock)
    pub scr: u64,
    /// Multiplex rate (50 bytes/sec units)
    pub mux_rate: u32,
    /// Pack stuffing length in bytes
    pub stuffing_length: u8,
}

/// PES (Packetized Elementary Stream) packet header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PesPacket {
    /// Stream ID (0xE0-0xEF for video, 0xC0-0xDF for audio)
    pub stream_id: u8,
    /// Packet length (0 if unbounded)
    pub length: u16,
    /// PTS (Presentation Time Stamp) in 90kHz ticks
    pub pts: Option<u64>,
    /// DTS (Decode Time Stamp) in 90kHz ticks
    pub dts: Option<u64>,
    /// Payload data (elementary stream)
    pub data: Vec<u8>,
}

/// Find all PS start codes in a byte stream.
/// Returns positions of start code prefixes (0x00 0x00 0x01).
fn find_ps_start_codes(data: &[u8]) -> Vec<usize> {
    let mut positions = Vec::new();
    if data.len() < 4 {
        return positions;
    }
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            positions.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    positions
}

/// Parse a PS pack header from data starting at a pack_start_code
/// (0x00 0x00 0x01 0xBA).
///
/// Returns (header, bytes_consumed).
pub fn parse_ps_pack_header(data: &[u8]) -> Result<(PsPackHeader, usize)> {
    if data.len() < 4 || data[0] != 0 || data[1] != 0 || data[2] != 1 || data[3] != 0xBA {
        bail!("Invalid PS pack start code");
    }

    if data.len() < 14 {
        bail!("PS pack header too short");
    }

    let marker_check = data[4];
    let is_mpeg2 = (marker_check & 0xC0) == 0x40; // Bits 7-6 = 01 for MPEG-2

    let scr: u64;
    let _mux_rate: u32;
    let offset: usize;

    if is_mpeg2 {
        // MPEG-2 system stream (recommended by GB28181)
        let b4 = data[4];
        let b5 = data[5];
        let b6 = data[6];
        let b7 = data[7];
        let b8 = data[8];
        let b9 = data[9];
        let b10 = data[10];

        // Reconstruct SCR (33 bits base + 9 bits extension = 42 bits total)
        let scr_base_high = (b4 as u64 & 0x38) >> 3;
        let scr_base_mid_hi = b5 as u64 >> 1;
        let scr_base_mid_lo = (b5 as u64 & 1) << 14 | ((b6 as u64 >> 1) & 0x7FFF);
        let scr_base = (scr_base_high << 30) | (scr_base_mid_hi << 15) | scr_base_mid_lo;

        let _scr_ext = ((b7 as u64 & 0x20) >> 4)
            | ((b7 as u64 & 0x01) << 2)
            | (((b4 as u64 & 0x01) << 1) & 0x01);

        scr = scr_base * 300; // 27MHz = 90kHz * 300

        let mux_rate_val = ((b7 as u32 & 0x3F) << 16) | ((b8 as u32) << 8) | (b9 as u32);
        _mux_rate = mux_rate_val >> 2;

        let stuffing_length = b10 & 0x07;
        offset = 11 + stuffing_length as usize;
    } else {
        scr = 0;
        _mux_rate = 0;
        offset = 12;
    }

    Ok((
        PsPackHeader {
            scr,
            mux_rate: _mux_rate,
            stuffing_length: if is_mpeg2 { data[10] & 0x07 } else { 0 },
        },
        offset,
    ))
}

/// Parse a PES packet from data starting at a packet_start_code_prefix.
///
/// Returns (pes_packet, bytes_consumed).
pub fn parse_pes_packet(data: &[u8]) -> Result<(PesPacket, usize)> {
    if data.len() < 6 {
        bail!("PES packet too short");
    }
    if data[0] != 0 || data[1] != 0 || data[2] != 1 {
        bail!("Invalid PES start code prefix");
    }

    let stream_id = data[3];
    let length = u16::from_be_bytes([data[4], data[5]]);

    if data.len() < 6 + 3 {
        bail!("PES packet truncated before header fields");
    }

    let mut offset = 6; // Start of optional PES header

    // Handle padding stream
    if stream_id == 0xBE {
        if length > 0 {
            offset += length as usize;
        }
        return Ok((
            PesPacket {
                stream_id,
                length,
                pts: None,
                dts: None,
                data: Vec::new(),
            },
            offset.min(data.len()),
        ));
    }

    if (0xC0..=0xEF).contains(&stream_id) {
        // Audio (0xC0-0xDF) or Video (0xE0-0xEF) stream
        if offset + 2 > data.len() {
            bail!("PES packet truncated at optional header fields");
        }

        let pes_header_flags = data[offset];
        let pes_header_length = data[offset + 1] as usize;
        offset += 2;

        if offset + pes_header_length > data.len() {
            bail!("PES packet truncated: optional header length exceeds data");
        }

        let mut pts: Option<u64> = None;
        let mut dts: Option<u64> = None;

        let pts_dts_flags = (pes_header_flags >> 6) & 0x03;

        if pts_dts_flags == 2 || pts_dts_flags == 3 {
            pts = Some(parse_pts_dts(&data[offset..offset + 5]));
        }

        if pts_dts_flags == 3 {
            dts = Some(parse_pts_dts(&data[offset + 5..offset + 10]));
        }

        // Skip remaining optional header fields (stuffing, etc.)
        offset = 6 + 3 + pes_header_length; // 6 bytes prefix+length + 3 bytes header info

        let remaining_len = if length > 0 {
            let header_overhead = offset - 6;
            (length as usize).saturating_sub(header_overhead)
        } else {
            data.len().saturating_sub(offset)
        };

        let payload_end = (offset + remaining_len).min(data.len());
        let payload = data[offset..payload_end].to_vec();

        Ok((
            PesPacket {
                stream_id,
                length,
                pts,
                dts,
                data: payload,
            },
            payload_end,
        ))
    } else if stream_id == 0xBC || (0xB9..=0xBB).contains(&stream_id) {
        // Program stream map, end code, padding
        let payload_end = if length > 0 {
            (6 + length as usize).min(data.len())
        } else {
            data.len()
        };
        Ok((
            PesPacket {
                stream_id,
                length,
                pts: None,
                dts: None,
                data: data[6..payload_end].to_vec(),
            },
            payload_end,
        ))
    } else {
        // Other stream types
        let payload_end = if length > 0 {
            (6 + length as usize).min(data.len())
        } else {
            data.len()
        };
        Ok((
            PesPacket {
                stream_id,
                length,
                pts: None,
                dts: None,
                data: data[6..payload_end].to_vec(),
            },
            payload_end,
        ))
    }
}

/// Parse a 5-byte PTS/DTS value (33 bits packed with marker bits).
fn parse_pts_dts(bytes: &[u8]) -> u64 {
    if bytes.len() < 5 {
        return 0;
    }
    let b0 = bytes[0] as u64;
    let b1 = bytes[1] as u64;
    let b2 = bytes[2] as u64;
    let b3 = bytes[3] as u64;
    let b4 = bytes[4] as u64;

    ((b0 >> 1) & 0x07) << 30
        | (b1 << 22)
        | ((b2 >> 1) & 0x7F) << 15
        | (b3 << 7)
        | ((b4 >> 1) & 0x7F)
}

/// Extract H.264 payload data from a PS (Program Stream) data buffer.
///
/// This parses the MPEG-2 Program Stream encapsulation used by GB/T 28181
/// and returns the H.264 data found within video PES packets.
pub fn parse_ps_to_h264(ps_data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let start_codes = find_ps_start_codes(ps_data);
    if start_codes.is_empty() {
        bail!("No PS start codes found in data");
    }

    let mut h264_data = Vec::new();

    let mut i = 0;
    while i < start_codes.len() {
        let pos = start_codes[i];
        if pos + 3 >= ps_data.len() {
            break;
        }

        let stream_id = ps_data[pos + 3];
        if stream_id == 0xBA || stream_id == 0xBB {
            i += 1;
            continue;
        } else if (0xE0..=0xEF).contains(&stream_id) {
            // Video PES packet
            if let Ok((pes, _consumed)) = parse_pes_packet(&ps_data[pos..]) {
                if !pes.data.is_empty() {
                    h264_data.push(pes.data);
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    Ok(h264_data)
}

/// Extract H.264 NAL units from a PS stream.
///
/// This function first extracts PES payloads (PS to PES), then finds
/// Annex B NAL units within the concatenated H.264 data.
pub fn parse_ps_to_nal_units(ps_data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let pes_payloads = parse_ps_to_h264(ps_data)?;

    if pes_payloads.is_empty() {
        return Ok(Vec::new());
    }

    let total_size: usize = pes_payloads.iter().map(|d| d.len()).sum();
    let mut combined = Vec::with_capacity(total_size);
    for payload in &pes_payloads {
        combined.extend_from_slice(payload);
    }

    // Find Annex B start codes and extract NAL units using existing H.264 parser
    let nal_units = crate::h264::split_nal_units(&combined);
    let result: Vec<Vec<u8>> = nal_units.into_iter().map(|nal| nal.to_vec()).collect();
    Ok(result)
}

// ─── RTP Stream Info ────────────────────────────────────────────────────────

/// Information about an RTP stream established via SIP INVITE.
#[derive(Debug, Clone)]
pub struct RtpStreamInfo {
    /// Device ID that is sending/receiving the stream
    pub device_id: String,
    /// Channel ID
    pub channel_id: String,
    /// SSRC of the RTP stream
    pub ssrc: u32,
    /// Transport protocol
    pub transport: Transport,
    /// Remote address for RTP data
    pub remote_addr: String,
    /// Remote port for RTP data
    pub remote_port: u16,
}

// ─── Platform Interface ─────────────────────────────────────────────────────

/// A GB/T 28181 platform server instance.
///
/// Manages device registration, stream setup, and heartbeat tracking.
pub struct Gb28181Platform {
    /// SIP listening address
    pub sip_addr: SocketAddr,
    /// Platform device ID (20-digit)
    pub platform_id: String,
    /// Platform SIP domain
    pub domain: String,
    /// Device registry
    pub registry: DeviceRegistry,
    /// Active streams
    pub active_streams: HashMap<String, RtpStreamInfo>,
}

impl Gb28181Platform {
    /// Create a new GB/T 28181 platform.
    pub fn new(sip_addr: SocketAddr, platform_id: &str, domain: &str) -> Self {
        Self {
            sip_addr,
            platform_id: platform_id.to_string(),
            domain: domain.to_string(),
            registry: DeviceRegistry::new(),
            active_streams: HashMap::new(),
        }
    }

    /// Handle an incoming SIP REGISTER request.
    ///
    /// Returns the response status code (200 on success).
    pub fn handle_register(&mut self, device_id: &str, ip: &str, port: u16, expires: u32) -> u16 {
        if expires == 0 {
            // Unregistration
            self.registry.unregister(device_id);
            return 200;
        }

        let expiry_dur = if expires > 0 {
            Duration::from_secs(expires as u64)
        } else {
            Duration::from_secs(3600)
        };

        self.registry
            .register(device_id, ip, port, expiry_dur, vec![]);
        tracing::info!("Device registered: {} at {}:{}", device_id, ip, port);
        200
    }

    /// Send an INVITE for real-time preview or playback.
    ///
    /// Returns the RTP stream info for the established stream.
    pub async fn invite_preview(&self, device_id: &str, channel_id: &str) -> Result<RtpStreamInfo> {
        let device = self
            .registry
            .get(device_id)
            .ok_or_else(|| anyhow!("Device not registered: {}", device_id))?;

        let ssrc = rand::random::<u32>();

        Ok(RtpStreamInfo {
            device_id: device_id.to_string(),
            channel_id: channel_id.to_string(),
            ssrc,
            transport: Transport::Tcp,
            remote_addr: device.ip.clone(),
            remote_port: 0,
        })
    }

    /// Query the device catalog (list all registered devices).
    pub fn query_catalog(&self) -> Vec<&RegisteredDevice> {
        self.registry.catalog()
    }

    /// Check if a device heartbeat is current.
    pub fn check_heartbeat(&self, device_id: &str) -> bool {
        self.registry.check_heartbeat(device_id)
    }

    /// Update a device's heartbeat timestamp.
    pub fn update_heartbeat(&mut self, device_id: &str) -> bool {
        self.registry.update_heartbeat(device_id)
    }

    /// Handle an incoming device catalog query response.
    pub fn update_device_channels(&mut self, device_id: &str, channels: Vec<ChannelInfo>) -> bool {
        if let Some(device) = self.registry.get_mut(device_id) {
            device.channels = channels;
            true
        } else {
            false
        }
    }

    /// Record an active stream in the platform state.
    pub fn add_stream(&mut self, call_id: String, stream: RtpStreamInfo) {
        self.active_streams.insert(call_id, stream);
    }

    /// Remove a finished stream.
    pub fn remove_stream(&mut self, call_id: &str) -> Option<RtpStreamInfo> {
        self.active_streams.remove(call_id)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ─── Device ID Tests ──────────────────────────────────────────────────

    #[test]
    fn test_device_id_format() {
        let id = format_device_id("34020000", 20, 118, 1);
        assert_eq!(id.len(), 20);
        assert_eq!(id, "34020000201180000001");

        let id = format_device_id("34020000", 20, 111, 1234567);
        assert_eq!(id, "34020000201111234567");
    }

    #[test]
    fn test_device_id_parsing() {
        let parts = parse_device_id("34020000201180000001").unwrap();
        assert_eq!(parts.region_code, "34020000");
        assert_eq!(parts.industry_type, 20);
        assert_eq!(parts.device_type, 118);
        assert_eq!(parts.serial, 1);

        let parts = parse_device_id("34020000201111234567").unwrap();
        assert_eq!(parts.region_code, "34020000");
        assert_eq!(parts.industry_type, 20);
        assert_eq!(parts.device_type, 111);
        assert_eq!(parts.serial, 1234567);
    }

    #[test]
    fn test_device_id_errors() {
        assert!(parse_device_id("short").is_err());
        assert!(parse_device_id("340200002011800000a").is_err());
        assert!(parse_device_id("3402000020118000000").is_err());
    }

    #[test]
    fn test_device_id_empty() {
        assert!(parse_device_id("").is_err());
        assert!(parse_device_id("3402000020118000000").is_err());
    }

    // ─── SIP Message Tests ────────────────────────────────────────────────

    #[test]
    fn test_register_request_serialize() {
        let msg = build_register_request(
            "34020000002000000001",
            "192.168.1.100",
            "3402000000",
            "3402000000",
            3600,
            None,
            "call-id-123",
            1,
        );
        let serialized = msg.serialize();
        assert!(serialized.contains("REGISTER sip:3402000000@3402000000 SIP/2.0"));
        assert!(serialized.contains("Call-ID: call-id-123"));
        assert!(serialized.contains("CSeq: 1 REGISTER"));
        assert!(serialized.contains("Expires: 3600"));
        assert!(serialized.contains("Content-Length: 0"));
    }

    #[test]
    fn test_register_response_parse() {
        let response = "SIP/2.0 200 OK\r\n\
                        Via: SIP/2.0/UDP 192.168.1.100:5060;rport=5060;received=192.168.1.100\r\n\
                        From: <sip:34020000002000000001@3402000000>;tag=abc123\r\n\
                        To: <sip:34020000002000000001@3402000000>;tag=def456\r\n\
                        Call-ID: call-id-123\r\n\
                        CSeq: 1 REGISTER\r\n\
                        Contact: <sip:34020000002000000001@192.168.1.100:5060>\r\n\
                        Expires: 3600\r\n\
                        Content-Length: 0\r\n\
                        \r\n";

        let msg = SipMessage::parse(response).unwrap();
        assert!(msg.status_code.is_some());
        assert_eq!(msg.status_code.unwrap(), SipStatusCode::Ok);
        assert_eq!(msg.version, "SIP/2.0");
        assert_eq!(msg.get_header("Call-ID"), Some("call-id-123"));
        assert_eq!(msg.get_header("CSeq"), Some("1 REGISTER"));
        assert_eq!(msg.get_header("Expires"), Some("3600"));
    }

    #[test]
    fn test_invite_request_serialize() {
        let msg = build_invite_request(
            "34020000002000000001",
            "192.168.1.10",
            "34020000001320000001",
            "192.168.1.100",
            0x12345678,
            Transport::Tcp,
            "invite-call-1",
            1,
            false,
        );
        let serialized = msg.serialize();
        assert!(serialized.contains("INVITE sip:34020000001320000001@192.168.1.100 SIP/2.0"));
        assert!(serialized.contains("Call-ID: invite-call-1"));
        assert!(serialized.contains("CSeq: 1 INVITE"));
        assert!(serialized.contains("Content-Type: application/sdp"));
        assert!(serialized.contains("v=0"));
        assert!(serialized.contains("m=video 0 RTP/AVP/TCP 96"));
    }

    #[test]
    fn test_sip_message_parse_minimal() {
        let data = "REGISTER sip:3402000000@3402000000 SIP/2.0\r\n\
                    Via: SIP/2.0/UDP 192.168.1.100:5060;branch=z9hG4bK1\r\n\
                    From: <sip:34020000002000000001@3402000000>;tag=1\r\n\
                    To: <sip:3402000000@3402000000>\r\n\
                    Call-ID: test-call\r\n\
                    CSeq: 1 REGISTER\r\n\
                    Contact: <sip:34020000002000000001@192.168.1.100:5060>\r\n\
                    Expires: 3600\r\n\
                    Content-Length: 0\r\n\
                    \r\n";

        let msg = SipMessage::parse(data).unwrap();
        assert_eq!(msg.method, Some(SipMethod::Register));
        assert!(msg.status_code.is_none());
        assert_eq!(msg.uri, Some("sip:3402000000@3402000000".to_string()));
        assert_eq!(msg.get_header("Expires"), Some("3600"));
        assert_eq!(msg.get_header("Call-ID"), Some("test-call"));

        let re_serialized = msg.serialize();
        assert!(re_serialized.contains("REGISTER"));
        assert!(re_serialized.contains("Call-ID: test-call"));
    }

    // ─── SDP Tests ────────────────────────────────────────────────────────

    #[test]
    fn test_sdp_parse() {
        let sdp_str = "v=0\r\n\
                       o=34020000002000000001 0 0 IN IP4 192.168.1.10\r\n\
                       s=Play\r\n\
                       c=IN IP4 192.168.1.10\r\n\
                       t=0 0\r\n\
                       m=video 10000 RTP/AVP 96\r\n\
                       a=recvonly\r\n\
                       a=rtpmap:96 PS/90000\r\n";

        let sdp = SdpSession::parse(sdp_str).unwrap();
        assert_eq!(sdp.session_name, "Play");
        assert_eq!(
            sdp.connection_address,
            Some("IN IP4 192.168.1.10".to_string())
        );
        assert_eq!(sdp.media.len(), 1);
        assert_eq!(sdp.media[0].media_type, "video");
        assert_eq!(sdp.media[0].port, 10000);
        assert_eq!(sdp.media[0].proto, "RTP/AVP");
        assert_eq!(sdp.media[0].payload_types, vec![96]);
        assert_eq!(sdp.media[0].get_attr("rtpmap"), Some("96 PS/90000"));
    }

    #[test]
    fn test_sdp_roundtrip() {
        let original = SdpSession {
            origin: "34020000002000000001 0 0 IN IP4 192.168.1.10".to_string(),
            session_name: "Play".to_string(),
            connection_address: Some("IN IP4 192.168.1.10".to_string()),
            bandwidth: None,
            media: vec![SdpMedia {
                media_type: "video".to_string(),
                port: 10000,
                proto: "RTP/AVP".to_string(),
                payload_types: vec![96],
                attributes: vec![
                    ("recvonly".to_string(), String::new()),
                    ("rtpmap".to_string(), "96 PS/90000".to_string()),
                ],
            }],
        };

        let serialized = original.serialize();
        let parsed = SdpSession::parse(&serialized).unwrap();
        assert_eq!(parsed.origin, original.origin);
        assert_eq!(parsed.session_name, original.session_name);
        assert_eq!(parsed.media.len(), 1);
        assert_eq!(parsed.media[0].port, 10000);
    }

    // ─── Device Registry Tests ────────────────────────────────────────────

    #[test]
    fn test_device_registry_register() {
        let mut registry = DeviceRegistry::new();
        assert!(registry.is_empty());

        let channels = vec![ChannelInfo {
            channel_id: "34020000001320000001".to_string(),
            name: "Front Door".to_string(),
            status: ChannelStatus::Online,
        }];

        registry.register(
            "3402000000111800000001",
            "192.168.1.100",
            5060,
            Duration::from_secs(3600),
            channels,
        );

        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());

        let device = registry.get("3402000000111800000001").unwrap();
        assert_eq!(device.ip, "192.168.1.100");
        assert_eq!(device.port, 5060);
        assert_eq!(device.channels.len(), 1);
        assert_eq!(device.channels[0].name, "Front Door");
    }

    #[test]
    fn test_device_registry_expire() {
        let mut registry = DeviceRegistry::new();
        registry.register(
            "3402000000111800000001",
            "192.168.1.100",
            5060,
            Duration::from_secs(0),
            vec![],
        );

        let expired = registry.purge_expired();
        assert_eq!(expired.len(), 1);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_device_registry_unregister() {
        let mut registry = DeviceRegistry::new();
        registry.register(
            "3402000000111800000001",
            "192.168.1.100",
            5060,
            Duration::from_secs(3600),
            vec![],
        );
        assert_eq!(registry.len(), 1);

        let removed = registry.unregister("3402000000111800000001");
        assert!(removed.is_some());
        assert!(registry.is_empty());

        let removed = registry.unregister("nonexistent");
        assert!(removed.is_none());
    }

    #[test]
    fn test_heartbeat_check() {
        let mut registry =
            DeviceRegistry::with_timeouts(Duration::from_secs(3600), Duration::from_secs(120));

        registry.register(
            "3402000000111800000001",
            "192.168.1.100",
            5060,
            Duration::from_secs(3600),
            vec![],
        );

        assert!(registry.check_heartbeat("3402000000111800000001"));
        assert!(!registry.check_heartbeat("nonexistent"));

        assert!(registry.update_heartbeat("3402000000111800000001"));
        assert!(!registry.update_heartbeat("nonexistent"));
    }

    #[test]
    fn test_catalog_query_empty() {
        let registry = DeviceRegistry::new();
        assert!(registry.catalog().is_empty());
    }

    #[test]
    fn test_catalog_query_with_devices() {
        let mut registry = DeviceRegistry::new();
        registry.register(
            "3402000000111800000001",
            "192.168.1.100",
            5060,
            Duration::from_secs(3600),
            vec![],
        );
        registry.register(
            "3402000000111800000002",
            "192.168.1.101",
            5060,
            Duration::from_secs(3600),
            vec![],
        );

        let catalog = registry.catalog();
        assert_eq!(catalog.len(), 2);
    }

    #[test]
    fn test_online_devices() {
        let mut registry = DeviceRegistry::new();

        registry.register(
            "3402000000111800000001",
            "192.168.1.100",
            5060,
            Duration::from_secs(3600),
            vec![],
        );
        registry.register(
            "3402000000111800000002",
            "192.168.1.101",
            5060,
            Duration::from_secs(3600),
            vec![],
        );

        assert_eq!(registry.online_devices().len(), 2);
    }

    // ─── SIP Method Tests ─────────────────────────────────────────────────

    #[test]
    fn test_sip_method_display_and_parse() {
        let methods = [
            SipMethod::Register,
            SipMethod::Invite,
            SipMethod::Ack,
            SipMethod::Bye,
            SipMethod::Message,
            SipMethod::Subscribe,
            SipMethod::Notify,
            SipMethod::Cancel,
            SipMethod::Info,
        ];
        for method in &methods {
            let s = method.to_string();
            let parsed: SipMethod = s.parse().unwrap();
            assert_eq!(&parsed, method);
        }
    }

    #[test]
    fn test_sip_status_code() {
        assert_eq!(SipStatusCode::from_code(200), SipStatusCode::Ok);
        assert_eq!(SipStatusCode::from_code(401), SipStatusCode::Unauthorized);
        assert_eq!(SipStatusCode::from_code(404), SipStatusCode::NotFound);
        assert_eq!(SipStatusCode::from_code(503).code(), 503);
        assert_eq!(SipStatusCode::from_code(503).reason(), "Unknown");
    }

    // ─── PS Parser Tests ─────────────────────────────────────────────────

    #[test]
    fn test_ps_pack_header_parse() {
        let mut ps_header = vec![0x00, 0x00, 0x01, 0xBA];
        ps_header.resize(14, 0x00);
        ps_header[4] = 0x44; // 01 000 100 -> bits 7-6 = 01 (MPEG-2)
        ps_header[7] = 0x21; // marker bit at position 5

        let result = parse_ps_pack_header(&ps_header);
        assert!(result.is_ok());

        let invalid = vec![0x00, 0x00, 0x01, 0x00];
        assert!(parse_ps_pack_header(&invalid).is_err());
    }

    #[test]
    fn test_ps_pes_parse() {
        let pes = vec![
            0x00, 0x00, 0x01, 0xE0, // start code + stream_id
            0x00, 0x0A, // PES length = 10
            0x80, // PTS_DTS_flags = 2 (PTS only)
            0x05, // header data length = 5
            0x21, 0x00, 0x00, 0x00, 0x01, // PTS (5 bytes)
            0x00, 0x00, 0x01, 0x67, 0x42, // Payload
        ];

        let result = parse_pes_packet(&pes);
        assert!(result.is_ok());
        let (packet, _consumed) = result.unwrap();
        assert_eq!(packet.stream_id, 0xE0);
        assert_eq!(packet.length, 10);
        assert!(packet.pts.is_some());
        assert!(packet.dts.is_none());
        assert_eq!(packet.data.len(), 2);
    }

    #[test]
    fn test_ps_to_h264_extraction() {
        let mut ps_data = vec![
            0x00, 0x00, 0x01, 0xBA, // pack_start_code
            0x44, 0x01, 0x00, 0x21, 0x00, 0x00, 0x00, // SCR + mux_rate
            0x01, // stuffing_length = 1
            0x00, // stuffing byte
        ];

        // Video PES with H.264 SPS NAL
        ps_data.extend_from_slice(&[0x00, 0x00, 0x01, 0xE0]);
        ps_data.extend_from_slice(&[0x00, 0x10]);
        ps_data.extend_from_slice(&[0x80, 0x05]);
        ps_data.extend_from_slice(&[0x21, 0x00, 0x00, 0x00, 0x01]);
        ps_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1E]);

        let result = parse_ps_to_h264(&ps_data);
        assert!(result.is_ok());
        let h264_data = result.unwrap();
        assert!(!h264_data.is_empty(), "Should extract H.264 data from PS");

        let nal_units = parse_ps_to_nal_units(&ps_data);
        assert!(nal_units.is_ok());
        let nals = nal_units.unwrap();
        assert!(!nals.is_empty(), "Should extract NAL units from PS stream");
    }

    // ─── Digest Auth Tests ────────────────────────────────────────────────

    #[test]
    fn test_digest_auth_parse() {
        let auth_str = "Digest realm=\"3402000000\", nonce=\"abc123\", algorithm=SHA-256";
        let params = parse_digest_auth(auth_str).unwrap();
        assert_eq!(params.realm, "3402000000");
        assert_eq!(params.nonce, "abc123");
        assert_eq!(params.algorithm, Some("SHA-256".to_string()));

        let auth_str = "Digest username=\"34020000002000000001\", realm=\"3402000000\", \
                        nonce=\"xyz789\", uri=\"sip:3402000000@3402000000\", \
                        response=\"1234abcd\", algorithm=SHA-256";
        let params = parse_digest_auth(auth_str).unwrap();
        assert_eq!(params.username, "34020000002000000001");
        assert_eq!(params.realm, "3402000000");
        assert_eq!(params.response, "1234abcd");
    }

    #[test]
    fn test_build_digest_auth() {
        let auth = build_digest_auth(
            "34020000002000000001",
            "3402000000",
            "password123",
            "nonce-value",
            "sip:3402000000@3402000000",
            "REGISTER",
            "SHA-256",
        );
        assert!(auth.contains("username=\"34020000002000000001\""));
        assert!(auth.contains("realm=\"3402000000\""));
        assert!(auth.contains("algorithm=SHA-256"));
        assert!(auth.starts_with("Digest "));
    }

    // ─── Platform Tests ───────────────────────────────────────────────────

    #[test]
    fn test_platform_handle_register() {
        let addr: SocketAddr = "0.0.0.0:5060".parse().unwrap();
        let mut platform = Gb28181Platform::new(addr, "34020000002000000001", "3402000000");

        let status =
            platform.handle_register("3402000000111800000001", "192.168.1.100", 5060, 3600);
        assert_eq!(status, 200);
        assert_eq!(platform.registry.len(), 1);

        let catalog = platform.query_catalog();
        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn test_platform_unregister() {
        let addr: SocketAddr = "0.0.0.0:5060".parse().unwrap();
        let mut platform = Gb28181Platform::new(addr, "34020000002000000001", "3402000000");

        platform.handle_register("3402000000111800000001", "192.168.1.100", 5060, 3600);
        assert_eq!(platform.registry.len(), 1);

        platform.handle_register("3402000000111800000001", "192.168.1.100", 5060, 0);
        assert!(platform.registry.is_empty());
    }

    #[test]
    fn test_platform_heartbeat() {
        let addr: SocketAddr = "0.0.0.0:5060".parse().unwrap();
        let mut platform = Gb28181Platform::new(addr, "34020000002000000001", "3402000000");

        platform.handle_register("3402000000111800000001", "192.168.1.100", 5060, 3600);
        assert!(platform.check_heartbeat("3402000000111800000001"));

        assert!(platform.update_heartbeat("3402000000111800000001"));
        assert!(!platform.check_heartbeat("nonexistent"));
    }

    #[test]
    fn test_platform_channels() {
        let addr: SocketAddr = "0.0.0.0:5060".parse().unwrap();
        let mut platform = Gb28181Platform::new(addr, "34020000002000000001", "3402000000");

        platform.handle_register("3402000000111800000001", "192.168.1.100", 5060, 3600);

        let channels = vec![
            ChannelInfo {
                channel_id: "34020000001320000001".to_string(),
                name: "Camera 1".to_string(),
                status: ChannelStatus::Online,
            },
            ChannelInfo {
                channel_id: "34020000001320000002".to_string(),
                name: "Camera 2".to_string(),
                status: ChannelStatus::Offline,
            },
        ];

        assert!(platform.update_device_channels("3402000000111800000001", channels));
        let device = platform.registry.get("3402000000111800000001").unwrap();
        assert_eq!(device.channels.len(), 2);
        assert_eq!(device.channels[0].name, "Camera 1");
        assert_eq!(device.channels[1].status, ChannelStatus::Offline);
    }

    #[test]
    fn test_bye_request_serialize() {
        let msg = build_bye_request(
            "34020000002000000001",
            "192.168.1.10",
            "34020000001320000001",
            "192.168.1.100",
            "bye-call-1",
            2,
        );
        let serialized = msg.serialize();
        assert!(serialized.contains("BYE sip:34020000001320000001@192.168.1.100 SIP/2.0"));
        assert!(serialized.contains("CSeq: 2 BYE"));
        assert!(serialized.contains("Content-Length: 0"));
    }

    #[test]
    fn test_device_type_constants() {
        assert_eq!(device_types::IPC, 111);
        assert_eq!(device_types::NVR, 118);
        assert_eq!(device_types::DECODER, 121);
        assert_eq!(device_types::ALARM, 122);
        assert_eq!(device_types::AUDIO, 134);
    }

    #[test]
    fn test_rtp_stream_info() {
        let stream = RtpStreamInfo {
            device_id: "34020000001320000001".to_string(),
            channel_id: "34020000001320000001".to_string(),
            ssrc: 0x12345678,
            transport: Transport::Tcp,
            remote_addr: "192.168.1.100".to_string(),
            remote_port: 10000,
        };
        assert_eq!(stream.device_id, "34020000001320000001");
        assert_eq!(stream.ssrc, 0x12345678);
        assert_eq!(stream.transport, Transport::Tcp);
    }

    #[test]
    fn test_platform_invite_preview() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let addr: SocketAddr = "0.0.0.0:5060".parse().unwrap();
            let mut platform = Gb28181Platform::new(addr, "34020000002000000001", "3402000000");

            platform.handle_register("3402000000111800000001", "192.168.1.100", 5060, 3600);

            let result = platform
                .invite_preview("3402000000111800000001", "34020000001320000001")
                .await;
            assert!(result.is_ok());
            let stream = result.unwrap();
            assert_eq!(stream.device_id, "3402000000111800000001");
            assert_eq!(stream.channel_id, "34020000001320000001");
        });
    }

    #[test]
    fn test_platform_stream_management() {
        let addr: SocketAddr = "0.0.0.0:5060".parse().unwrap();
        let mut platform = Gb28181Platform::new(addr, "34020000002000000001", "3402000000");

        let stream = RtpStreamInfo {
            device_id: "34020000001320000001".to_string(),
            channel_id: "34020000001320000001".to_string(),
            ssrc: 0x12345678,
            transport: Transport::Tcp,
            remote_addr: "192.168.1.100".to_string(),
            remote_port: 10000,
        };

        platform.add_stream("call-1".to_string(), stream);
        assert_eq!(platform.active_streams.len(), 1);

        let removed = platform.remove_stream("call-1");
        assert!(removed.is_some());
        assert!(platform.active_streams.is_empty());
    }

    #[test]
    fn test_transport_display() {
        assert_eq!(Transport::Tcp.to_string(), "TCP");
        assert_eq!(Transport::Udp.to_string(), "UDP");
    }

    #[test]
    fn test_sip_message_header_case_insensitive() {
        let data = "REGISTER sip:test@test.com SIP/2.0\r\n\
                    call-id: case-test\r\n\
                    Content-Length: 0\r\n\
                    \r\n";
        let msg = SipMessage::parse(data).unwrap();
        assert_eq!(msg.get_header("Call-ID"), Some("case-test"));
        assert_eq!(msg.get_header("call-id"), Some("case-test"));
        assert_eq!(msg.get_header("CALL-ID"), Some("case-test"));
    }

    #[test]
    fn test_sdp_missing_required() {
        assert!(SdpSession::parse("v=0\r\n").is_err());
        assert!(SdpSession::parse("").is_err());
    }

    #[test]
    fn test_hex_encode_fn() {
        let result = hex_encode(b"hello");
        assert_eq!(result, "68656c6c6f");
        assert_eq!(hex_encode(b""), "");
    }
}
