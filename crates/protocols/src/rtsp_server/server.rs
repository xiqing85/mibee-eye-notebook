//! RTSP method handlers and connection lifecycle.
//!
//! Provides:
//! - [`RtspMethod`] enum and parsing
//! - RTSP request parsing ([`read_rtsp_request`], [`ParsedRequest`])
//! - Session state machine ([`SessionState`], [`Session`])
//! - Response builders for all RTSP status codes
//! - Method handlers: OPTIONS, DESCRIBE, SETUP, PLAY, PAUSE, TEARDOWN, GET_PARAMETER
//! - [`handle_connection`] — the per-TCP-connection orchestrator

use anyhow::{Result, bail};
use observability::metrics;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use super::auth::{build_digest_challenge, generate_nonce, generate_session_id, verify_digest_auth};
use super::framing::{
    TransportInfo, build_interleaved_frame, get_header, parse_rtp_header_for_tracking,
    SequenceTracker,
};
use super::{LiveStreamEntry, RtspServer, RtspServerConfig, RtspServerInner, StreamConfig};

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
// Request / Response types
// ═══════════════════════════════════════════════════════════════════════════════

/// A parsed RTSP request.
#[derive(Debug, Clone)]
pub(super) struct ParsedRequest {
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
pub(super) enum SessionState {
    Init,
    Described {
        stream_path: String,
    },
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
pub(super) struct Session {
    state: SessionState,
}

impl Session {
    pub(super) fn new() -> Self {
        Self {
            state: SessionState::Init,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Response builders
// ═══════════════════════════════════════════════════════════════════════════════

/// Build an RTSP response as raw bytes.
pub(super) fn build_response(
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
pub(super) fn build_unauthorized_response(cseq: u32, realm: &str, nonce: &str) -> Vec<u8> {
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
pub(super) fn build_ok_response(cseq: u32, extra_headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    build_response(cseq, 200, "OK", extra_headers, body)
}

/// Build a 404 Not Found response.
pub(super) fn build_not_found_response(cseq: u32) -> Vec<u8> {
    build_response(cseq, 404, "Not Found", &[], b"")
}

/// Build a 455 Method Not Valid In This State response.
pub(super) fn build_invalid_state_response(cseq: u32) -> Vec<u8> {
    build_response(cseq, 455, "Method Not Valid In This State", &[], b"")
}

/// Build a 461 Unsupported Transport response.
pub(super) fn build_unsupported_transport_response(cseq: u32) -> Vec<u8> {
    build_response(cseq, 461, "Unsupported Transport", &[], b"")
}

// ═══════════════════════════════════════════════════════════════════════════════
// Request reader
// ═══════════════════════════════════════════════════════════════════════════════

/// Read and parse a single RTSP request from the buffered reader.
///
/// Returns `Ok(None)` on EOF (connection closed).
pub(super) async fn read_rtsp_request<R: AsyncRead + Unpin>(
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
// Method handlers
// ═══════════════════════════════════════════════════════════════════════════════

/// Options response: advertise supported methods.
#[tracing::instrument(skip_all)]
pub(super) fn handle_options(cseq: u32) -> Vec<u8> {
    let public = "DESCRIBE, SETUP, TEARDOWN, PLAY, PAUSE, OPTIONS, GET_PARAMETER";
    build_ok_response(cseq, &[("Public", public)], b"")
}

/// Describe response: return SDP for the matched stream.
#[tracing::instrument(skip_all)]
pub(super) fn handle_describe(
    cseq: u32,
    uri: &str,
    stream: &StreamConfig,
    _server_config: &RtspServerConfig,
    session: &mut Session,
) -> Vec<u8> {
    // Build the SDP with content-base pointing to our server
    let base_url = uri.trim_end_matches(&stream.url_path());
    let sdp = stream.sdp_body.clone();

    session.state = SessionState::Described {
        stream_path: stream.path.clone(),
    };

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
#[tracing::instrument(skip_all)]
pub(super) fn handle_setup(
    cseq: u32,
    uri: &str,
    transport_header: &str,
    streams: &HashMap<String, StreamConfig>,
    session: &mut Session,
    _server_config: &RtspServerConfig,
) -> (Vec<u8>, Option<TransportInfo>) {
    // Find the matching stream; fall back to the single stream in the
    // map when the SETUP URI has no path (some RTSP clients send SETUP
    // to the base URL when the SDP has no a=control attribute).
    let stream = match find_stream_by_uri(uri, streams)
        .or_else(|| streams.values().next().filter(|_| streams.len() == 1))
    {
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
#[tracing::instrument(skip_all)]
pub(super) fn handle_play(
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
#[tracing::instrument(skip_all)]
pub(super) fn handle_teardown(cseq: u32, session_id: &str, session: &mut Session) -> Vec<u8> {
    session.state = SessionState::Teardown;
    build_ok_response(cseq, &[("Session", session_id)], b"")
}

/// Handle PAUSE request.
#[tracing::instrument(skip_all)]
pub(super) fn handle_pause(cseq: u32, session_id: &str, session: &Session) -> Vec<u8> {
    match &session.state {
        SessionState::Playing { .. } => build_ok_response(cseq, &[("Session", session_id)], b""),
        _ => build_invalid_state_response(cseq),
    }
}

/// Handle GET_PARAMETER request.
#[tracing::instrument(skip_all)]
pub(super) fn handle_get_parameter(cseq: u32) -> Vec<u8> {
    build_ok_response(cseq, &[], b"")
}

/// Find a stream that matches the given URI.
pub(super) fn find_stream_by_uri<'a>(
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
pub(super) fn find_live_stream<'a>(
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
pub(super) fn check_auth(req: &ParsedRequest, config: &RtspServerConfig) -> Result<bool> {
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

// ═══════════════════════════════════════════════════════════════════════════════
// Connection handler
// ═══════════════════════════════════════════════════════════════════════════════

/// Handle a single RTSP connection.
#[tracing::instrument(skip_all)]
pub(super) async fn handle_connection(
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
                            let live_map = server.live_streams.lock();
                            find_live_stream(&request.uri, &live_map).map(|(path, entry)| {
                                // Build SDP dynamically — includes sprop-parameter-sets
                                // when SPS/PPS are available from the capture pipeline.
                                let sdp = RtspServer::build_live_sdp(entry);
                                StreamConfig::new(&path, &sdp, entry.ssrc)
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
                        let live_map = server.live_streams.lock();
                        find_live_stream(&request.uri, &live_map).map(|(path, entry)| {
                            StreamConfig::new(&path, &entry.sdp_body, entry.ssrc)
                        })
                    })
                    .or_else(|| {
                        // Fallback: use the stream path saved during DESCRIBE
                        if let SessionState::Described { stream_path } = &session.state {
                            let path = stream_path.clone();
                            let live_map = server.live_streams.lock();
                            live_map
                                .get(&path)
                                .map(|entry| StreamConfig::new(&path, &entry.sdp_body, entry.ssrc))
                        } else {
                            None
                        }
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
                    let rtcp_channel = transport.interleaved.map(|(_, c)| c).unwrap_or(1);

                    // Scope the lock to avoid holding MutexGuard (not Send) across await
                    // Subscribe to the broadcast channel (supports multiple concurrent clients)
                    let live_result = {
                        let live_map = server.live_streams.lock();
                        live_map
                            .get(&live_path)
                            .map(|entry| (entry.frame_tx.subscribe(), entry.ssrc))
                    };

                    info!(
                        "delivery lookup: live_path={}, found={}",
                        live_path,
                        live_result.is_some()
                    );
                    if let Some((mut frame_rx, ssrc)) = live_result {
                        metrics::increment_rtsp_sessions("active");
                        info!("Starting live stream delivery for /{live_path} (ssrc={ssrc})");
                        let mut seq_tracker = SequenceTracker::new();
                        let mut last_activity = Instant::now();
                        let mut packet_count: u32 = 0;
                        let mut octet_count: u32 = 0;

                        loop {
                            tokio::select! {
                                frame = frame_rx.recv() => {
                                    match frame {
                                        Ok(data) => {
                                            last_activity = Instant::now();
                                            // Check for RTP sequence number gaps
                                            if let Some(info) = parse_rtp_header_for_tracking(&data) {
                                                let expected = seq_tracker.expected();
                                                if let Some(gap) = seq_tracker.check(info.sequence_number) {
                                                    warn!(
                                                        ssrc = info.ssrc,
                                                        expected = expected,
                                                        got = info.sequence_number,
                                                        gap = gap,
                                                        "RTP sequence gap detected"
                                                    );
                                                }
                                            }
                                            // Track packet/octet counts for RTCP SR
                                            packet_count = packet_count.wrapping_add(1);
                                            let payload_size = if data.len() >= 12 { data.len() - 12 } else { 0 };
                                            octet_count = octet_count.wrapping_add(payload_size as u32);

                                            match build_interleaved_frame(interleave_channel, &data) {
                                                Ok(interleaved) => {
                                                    if writer.write_all(&interleaved).await.is_err() {
                                                        break; // client disconnected
                                                    }
                                                    metrics::increment_rtsp_bytes_sent(interleaved.len() as u64);
                                                }
                                                Err(e) => {
                                                    warn!("Failed to build interleaved frame: {e}");
                                                    continue;
                                                }
                                            }
                                        }
                                        Err(broadcast::error::RecvError::Closed) => {
                                            debug!("Live stream /{live_path} ended");
                                            break;
                                        }
                                        Err(broadcast::error::RecvError::Lagged(n)) => {
                                            warn!("RTSP client lagged by {n} packets on /{live_path}");
                                            continue;
                                        }
                                    }
                                }
                                cmd = read_rtsp_request(&mut buf_reader) => {
                                    match cmd {
                                        Ok(Some(req)) => {
                                            last_activity = Instant::now();
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
                                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                                    // Check idle timeout (60s threshold)
                                    if last_activity.elapsed() > Duration::from_secs(60) {
                                        warn!("RTSP session timeout: {session_id}, idle for 60s");
                                        let resp = handle_teardown(0, &session_id, &mut session);
                                        let _ = writer.write_all(&resp).await;
                                        break;
                                    }
                                    // Send RTCP Sender Report every 5 seconds
                                    let now = std::time::SystemTime::now();
                                    let ntp_ts = crate::rtcp::system_time_to_ntp(now);
                                    let rtp_ts = crate::rtcp::ntp_to_rtp(ntp_ts, 90000);
                                    let sr = crate::rtcp::build_sender_report(ssrc, ntp_ts, rtp_ts, packet_count, octet_count);
                                    match build_interleaved_frame(rtcp_channel, &sr) {
                                        Ok(frame) => {
                                            if let Err(e) = writer.write_all(&frame).await {
                                                debug!("Error sending RTCP SR: {e}");
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Error building RTCP SR frame: {e}");
                                        }
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
    metrics::increment_rtsp_sessions("closed");
    debug!("RTSP connection closed");
}
