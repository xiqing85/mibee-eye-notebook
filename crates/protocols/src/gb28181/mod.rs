//! GB/T 28181-2016/2022 Device module.
//!
//! This module provides an implementation of the Chinese national standard
//! for video surveillance systems, GB/T 28181-2016 and GB/T 28181-2022,
//! operating as a **device** that registers with a SIP platform.
//!
//! ## Architecture
//!
//! - **Device ID**: 20-digit national standard format ([`device_id`])
//! - **SIP signaling**: Hand-written parser/serializer for the SIP subset
//!   used by GB/T 28181 (REGISTER, INVITE, MESSAGE, BYE, etc.) ([`sip`])
//! - **Digest Auth**: RFC 7616 Digest authentication for SIP REGISTER ([`sip`])
//! - **PS (Program Stream) Parser**: Extracts H.264 NAL units from MPEG-2
//!   Program Stream encapsulation used by GB/T 28181 for RTP media transport ([`ps`])
//! - **SipDeviceClient**: Manages device registration with a SIP platform ([`client`])
//! - **RtpPusher**: Constructs and sends RTP packets to a destination ([`rtp_pusher`])

pub mod client;
pub mod device_id;
pub mod manscdp;
pub mod ps;
pub mod rtp_pusher;
pub mod sip;

pub use client::{InviteInfo, SipDeviceClient, parse_401_challenge, parse_invite};
pub use device_id::device_types;
pub use device_id::{DeviceIdParts, format_device_id, parse_device_id};
pub use manscdp::{ChannelItem, DeviceItem, DeviceList, Notify, Query, Response};
pub use ps::{
    PesPacket, PsPackHeader, parse_pes_packet, parse_ps_pack_header, parse_ps_to_h264,
    parse_ps_to_nal_units,
};
pub use rtp_pusher::{RtpPusher, RtpStreamInfo};
pub use sip::{
    DigestAuthParams, SdpMedia, SdpSession, SipMessage, SipMethod, SipStatusCode, Transport,
    build_bye_request, build_digest_auth, build_invite_response, build_register_request,
    parse_digest_auth,
};

#[cfg(test)]
mod tests;
