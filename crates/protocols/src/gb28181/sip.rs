//! SIP message types, parser/serializer, SDP, request builders, and Digest auth.
//!
//! This module provides a hand-written parser/serializer for the SIP subset
//! used by GB/T 28181 (REGISTER, INVITE, MESSAGE, BYE, etc.), along with
//! RFC 7616 Digest authentication for SIP REGISTER.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

// ─── SIP Method & Status Code ───────────────────────────────────────────────

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

// ─── SIP Message ────────────────────────────────────────────────────────────

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
    #[tracing::instrument(skip_all)]
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
    #[tracing::instrument(skip_all)]
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
    #[tracing::instrument(skip_all)]
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
    #[tracing::instrument(skip_all)]
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
#[tracing::instrument(skip_all)]
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
    headers.push(("User-Agent".to_string(), "mibee-rec/0.1".to_string()));
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

/// Build a SIP BYE request.
#[tracing::instrument(skip_all)]
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

/// Build a SIP 200 OK response to an INVITE request.
///
/// # Arguments
/// * `invite` - The received INVITE message to respond to
/// * `local_id` - This device's ID (20-digit)
/// * `local_sdp` - SDP body describing the media being sent (video stream)
/// * `local_tag` - Tag to add to To header for dialog identification
/// * `cseq` - CSeq number from INVITE
#[tracing::instrument(skip_all)]
pub fn build_invite_response(
    invite: &SipMessage,
    local_id: &str,
    local_sdp: &str,
    local_tag: u32,
    cseq: u32,
) -> SipMessage {
    let mut headers = Vec::new();

    // Copy Via from INVITE (add rport, received)
    if let Some(via) = invite.get_header("Via") {
        headers.push(("Via".to_string(), via.to_string()));
    }

    // From header from INVITE
    if let Some(from) = invite.get_header("From") {
        headers.push(("From".to_string(), from.to_string()));
    }

    // To header with our tag added
    if let Some(to) = invite.get_header("To") {
        // Parse and add tag
        if to.contains("tag=") {
            headers.push(("To".to_string(), to.to_string()));
        } else {
            headers.push(("To".to_string(), format!("{};tag={}", to, local_tag)));
        }
    }

    // Call-ID from INVITE
    if let Some(call_id) = invite.get_header("Call-ID") {
        headers.push(("Call-ID".to_string(), call_id.to_string()));
    }

    // CSeq from INVITE (method stays as INVITE in response)
    headers.push(("CSeq".to_string(), format!("{} INVITE", cseq)));
    headers.push((
        "Contact".to_string(),
        format!("<sip:{}@{}:5060>", local_id, local_id),
    ));
    headers.push(("Content-Type".to_string(), "application/sdp".to_string()));
    headers.push(("Content-Length".to_string(), local_sdp.len().to_string()));

    // Extract URI from INVITE Request-Line
    let request_uri = invite.uri.as_deref().unwrap_or("sip:unknown");

    SipMessage {
        start_line: "SIP/2.0 200 OK".to_string(),
        method: None,
        status_code: Some(SipStatusCode::Ok),
        uri: Some(request_uri.to_string()),
        version: "SIP/2.0".to_string(),
        headers,
        body: local_sdp.to_string(),
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
#[tracing::instrument(skip_all)]
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

/// Build a Digest Authorization header value for SIP 401 challenge responses.
///
/// Computes SHA-256 digest per RFC 7616:
///   HA1 = SHA-256(username:realm:password)
///   HA2 = SHA-256(method:uri)
///   response = SHA-256(HA1:nonce:HA2)
#[tracing::instrument(skip_all)]
pub fn build_digest_auth(
    username: &str,
    realm: &str,
    password: &str,
    nonce: &str,
    uri: &str,
    method: &str,
    algorithm: &str,
) -> String {
    let ha1 = {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:{}:{}", username, realm, password).as_bytes());
        hex::encode(hasher.finalize())
    };
    let ha2 = {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:{}", method.to_uppercase(), uri).as_bytes());
        hex::encode(hasher.finalize())
    };
    let response = {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:{}:{}", ha1, nonce, ha2).as_bytes());
        hex::encode(hasher.finalize())
    };

    format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm={}",
        username, realm, nonce, uri, response, algorithm
    )
}
