//! SIP device client — manages device registration with a SIP platform.
//!
//! Also includes INVITE parsing (`InviteInfo` / `parse_invite`) and
//! 401 challenge extraction (`parse_401_challenge`).

use std::net::SocketAddr;

use anyhow::{Result, anyhow};
use observability::metrics;

use super::sip::{
    DigestAuthParams, SdpSession, SipMessage, build_bye_request, build_digest_auth,
    build_register_request,
};
use crate::rtp::H264_PAYLOAD_TYPE;

/// A GB/T 28181 SIP device client that registers with a SIP platform.
///
/// Manages the REGISTER dialog with a GB/T 28181 SIP platform, including
/// digest authentication challenge-response.
#[derive(Debug, Clone)]
pub struct SipDeviceClient {
    /// 20-digit device ID
    pub device_id: String,
    /// SIP server (platform) address
    pub sip_server_addr: SocketAddr,
    /// Local IP address advertised in SIP messages
    pub local_ip: String,
    /// Local SIP port
    pub local_port: u16,
    /// SIP domain (usually the platform's domain)
    pub domain: String,
    /// Authentication username (usually same as device_id)
    pub username: String,
    /// Authentication password
    pub password: String,
    /// Current Call-ID for SIP dialogs
    pub call_id: String,
    /// Current CSeq number
    pub cseq: u32,
    /// Registration expiry in seconds
    pub expires: u32,
}

impl SipDeviceClient {
    /// Create a new SIP device client.
    #[tracing::instrument(skip_all, fields(device_id))]
    pub fn new(
        device_id: &str,
        sip_server_addr: SocketAddr,
        local_ip: &str,
        local_port: u16,
        domain: &str,
        password: &str,
        expires: u32,
    ) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            device_id: device_id.to_string(),
            sip_server_addr,
            local_ip: local_ip.to_string(),
            local_port,
            domain: domain.to_string(),
            username: device_id.to_string(),
            password: password.to_string(),
            call_id: format!("{}-{}", device_id, nanos),
            cseq: 1,
            expires,
        }
    }

    /// Build an initial (unauthenticated) SIP REGISTER request.
    #[tracing::instrument(skip_all)]
    pub fn build_register(&self) -> SipMessage {
        metrics::increment_gb28181_register_status("registered");
        build_register_request(
            &self.device_id,
            &self.local_ip,
            &self.domain,
            &self.domain,
            self.expires,
            None,
            &self.call_id,
            self.cseq,
        )
    }

    /// Build a SIP REGISTER request with Digest authentication.
    #[tracing::instrument(skip_all)]
    pub fn build_register_with_auth(&self, auth: &DigestAuthParams) -> SipMessage {
        metrics::increment_gb28181_register_status("registered");
        let uri = format!("sip:{}@{}", self.device_id, self.domain);
        let auth_header = build_digest_auth(
            &self.username,
            &auth.realm,
            &self.password,
            &auth.nonce,
            &uri,
            "REGISTER",
            auth.algorithm.as_deref().unwrap_or("SHA-256"),
        );
        build_register_request(
            &self.device_id,
            &self.local_ip,
            &self.domain,
            &self.domain,
            self.expires,
            Some(&auth_header),
            &self.call_id,
            self.cseq,
        )
    }

    /// Build a SIP BYE request to end a session.
    #[tracing::instrument(skip_all)]
    pub fn build_bye(
        &self,
        remote_id: &str,
        remote_addr: &str,
        call_id: &str,
        cseq: u32,
    ) -> SipMessage {
        build_bye_request(
            &self.device_id,
            &self.local_ip,
            remote_id,
            remote_addr,
            call_id,
            cseq,
        )
    }

    /// Increment the CSeq counter.
    #[tracing::instrument(skip_all)]
    pub fn inc_cseq(&mut self) {
        self.cseq = self.cseq.wrapping_add(1);
    }
}

/// Parse the WWW-Authenticate header from a 401 SIP response to extract
/// the Digest challenge parameters.
#[tracing::instrument(skip_all)]
pub fn parse_401_challenge(msg: &SipMessage) -> Result<DigestAuthParams> {
    let auth_header = msg
        .get_header("WWW-Authenticate")
        .ok_or_else(|| anyhow!("401 response missing WWW-Authenticate header"))?;
    super::sip::parse_digest_auth(auth_header)
}

/// Information extracted from a received SIP INVITE request.
#[derive(Debug, Clone)]
pub struct InviteInfo {
    /// Call-ID from the INVITE
    pub call_id: String,
    /// Media address (IP) extracted from SDP
    pub media_address: String,
    /// Media port from SDP m= line
    pub media_port: u16,
    /// SSRC (0 if not specified in SDP)
    pub ssrc: u32,
    /// RTP payload type
    pub payload_type: u8,
}

/// Parse a SIP INVITE message to extract stream target information.
///
/// The INVITE comes FROM the platform TO this device, containing the
/// platform's receive address and port in the SDP body.
#[tracing::instrument(skip_all)]
pub fn parse_invite(msg: &SipMessage) -> Result<InviteInfo> {
    let call_id = msg
        .get_header("Call-ID")
        .ok_or_else(|| anyhow!("INVITE missing Call-ID header"))?
        .to_string();

    let sdp = SdpSession::parse(&msg.body)?;

    let media = sdp
        .media
        .first()
        .ok_or_else(|| anyhow!("INVITE SDP has no media lines"))?;

    // Extract IP from connection address (format: "IN IP4 x.x.x.x")
    let c_addr = sdp
        .connection_address
        .as_deref()
        .unwrap_or("IN IP4 127.0.0.1");
    let ip = c_addr
        .split_whitespace()
        .last()
        .unwrap_or("127.0.0.1")
        .to_string();

    let payload_type = media
        .payload_types
        .first()
        .copied()
        .unwrap_or(H264_PAYLOAD_TYPE);

    // SSRC may be specified as an SDP attribute
    let ssrc = media
        .get_attr("ssrc")
        .and_then(|s| {
            // Format: "ssrc:12345678" or just the hex value
            let val = s.split_whitespace().next().unwrap_or(s);
            let val = val.strip_prefix("ssrc:").unwrap_or(val);
            u32::from_str_radix(val, 16).ok()
        })
        .unwrap_or(0);

    Ok(InviteInfo {
        call_id,
        media_address: ip,
        media_port: media.port,
        ssrc,
        payload_type,
    })
}
