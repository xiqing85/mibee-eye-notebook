use super::*;
use std::net::SocketAddr;

use crate::rtp::H264_PAYLOAD_TYPE;

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
        ssrc: None,
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
        None,
    );
    assert!(auth.contains("username=\"34020000002000000001\""));
    assert!(auth.contains("realm=\"3402000000\""));
    assert!(auth.contains("algorithm=SHA-256"));
    assert!(auth.starts_with("Digest "));
}

// ─── SipDeviceClient Tests ─────────────────────────────────────────────

#[test]
fn test_sip_device_client_register() {
    let addr: SocketAddr = "192.168.1.200:5060".parse().unwrap();
    let client = SipDeviceClient::new(
        "34020000001320000001",
        addr,
        "192.168.1.100",
        5060,
        "3402000000",
        "testpass",
        3600,
    );

    let reg = client.build_register();
    let serialized = reg.serialize();
    assert!(serialized.contains("REGISTER sip:3402000000@3402000000 SIP/2.0"));
    assert!(serialized.contains("Expires: 3600"));
    assert!(serialized.contains("Content-Length: 0"));
    // Should NOT contain Authorization header (unauthenticated)
    assert!(!serialized.contains("Authorization"));
}

#[test]
fn test_parse_401_challenge() {
    let response = "SIP/2.0 401 Unauthorized\r\n\
                    Via: SIP/2.0/UDP 192.168.1.100:5060;branch=z9hG4bK1\r\n\
                    From: <sip:34020000001320000001@3402000000>;tag=1\r\n\
                    To: <sip:34020000001320000001@3402000000>;tag=abc\r\n\
                    Call-ID: test-call\r\n\
                    CSeq: 1 REGISTER\r\n\
                    WWW-Authenticate: Digest realm=\"3402000000\", nonce=\"challenge123\", algorithm=SHA-256\r\n\
                    Content-Length: 0\r\n\
                    \r\n";

    let msg = SipMessage::parse(response).unwrap();
    let challenge = parse_401_challenge(&msg).unwrap();
    assert_eq!(challenge.realm, "3402000000");
    assert_eq!(challenge.nonce, "challenge123");
    assert_eq!(challenge.algorithm, Some("SHA-256".to_string()));
}

#[test]
fn test_build_register_with_auth() {
    let addr: SocketAddr = "192.168.1.200:5060".parse().unwrap();
    let client = SipDeviceClient::new(
        "34020000001320000001",
        addr,
        "192.168.1.100",
        5060,
        "3402000000",
        "password123",
        3600,
    );

    let digest_auth = DigestAuthParams {
        realm: "3402000000".to_string(),
        nonce: "challenge123".to_string(),
        username: String::new(),
        uri: String::new(),
        response: String::new(),
        algorithm: Some("SHA-256".to_string()),
        opaque: None,
        qop: None,
        nc: None,
        cnonce: None,
    };

    let reg = client.build_register_with_auth(&digest_auth);
    let serialized = reg.serialize();
    assert!(serialized.contains("REGISTER sip:3402000000@3402000000 SIP/2.0"));
    assert!(serialized.contains("Authorization: Digest"));
    assert!(serialized.contains("realm=\"3402000000\""));
    assert!(serialized.contains("nonce=\"challenge123\""));
    assert!(serialized.contains("algorithm=SHA-256"));
}

// ─── InviteInfo Tests ──────────────────────────────────────────────────

#[test]
fn test_parse_invite() {
    let invite = "INVITE sip:34020000001320000001@192.168.1.100 SIP/2.0\r\n\
                  Via: SIP/2.0/UDP 192.168.1.200:5060;branch=z9hG4bK1234\r\n\
                  From: <sip:34020000002000000001@3402000000>;tag=abc123\r\n\
                  To: <sip:34020000001320000001@3402000000>\r\n\
                  Call-ID: invite-call-456\r\n\
                  CSeq: 1 INVITE\r\n\
                  Contact: <sip:34020000002000000001@192.168.1.200:5060>\r\n\
                  Content-Type: application/sdp\r\n\
                  Content-Length: 148\r\n\
                  \r\n\
                  v=0\r\n\
                  o=34020000002000000001 0 0 IN IP4 192.168.1.200\r\n\
                  s=Play\r\n\
                  c=IN IP4 192.168.1.200\r\n\
                  t=0 0\r\n\
                  m=video 10000 RTP/AVP 96\r\n\
                  a=recvonly\r\n\
                  a=rtpmap:96 PS/90000\r\n";

    let msg = SipMessage::parse(invite).unwrap();
    let info = parse_invite(&msg).unwrap();
    assert_eq!(info.call_id, "invite-call-456");
    assert_eq!(info.media_address, "192.168.1.200");
    assert_eq!(info.media_port, 10000);
    assert_eq!(info.payload_type, 96);
}

#[test]
fn test_parse_invite_with_ssrc() {
    let invite = "INVITE sip:34020000001320000001@192.168.1.100 SIP/2.0\r\n\
                  Via: SIP/2.0/UDP 192.168.1.200:5060;branch=z9hG4bK1234\r\n\
                  From: <sip:34020000002000000001@3402000000>;tag=abc123\r\n\
                  To: <sip:34020000001320000001@3402000000>\r\n\
                  Call-ID: invite-call-789\r\n\
                  CSeq: 1 INVITE\r\n\
                  Content-Type: application/sdp\r\n\
                  Content-Length: 175\r\n\
                  \r\n\
                  v=0\r\n\
                  o=34020000002000000001 0 0 IN IP4 192.168.1.200\r\n\
                  s=Play\r\n\
                  c=IN IP4 192.168.1.200\r\n\
                  t=0 0\r\n\
                  y=12345678\r\n\
                  m=video 20000 RTP/AVP 96\r\n\
                  a=recvonly\r\n\
                  a=rtpmap:96 PS/90000\r\n";

    let msg = SipMessage::parse(invite).unwrap();
    let info = parse_invite(&msg).unwrap();
    assert_eq!(info.call_id, "invite-call-789");
    assert_eq!(info.media_address, "192.168.1.200");
    assert_eq!(info.media_port, 20000);
    assert_eq!(
        info.ssrc, 12345678,
        "SSRC should be parsed from y= field (decimal)"
    );
    assert_eq!(info.payload_type, 96);
}

// ─── RtpPusher Tests ──────────────────────────────────────────────────

#[test]
fn test_rtp_pusher_build_packet() {
    let addr: SocketAddr = "192.168.1.200:10000".parse().unwrap();
    let mut pusher = RtpPusher::new(addr, 0x12345678, H264_PAYLOAD_TYPE);

    let nal = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1E];
    let packet = pusher.build_rtp_packet(&nal);

    // Verify RTP header
    assert!(packet.len() >= 12);
    assert_eq!(packet[0] >> 6, 2); // Version = 2
    assert_eq!(packet[1] & 0x7F, H264_PAYLOAD_TYPE); // Payload type

    // Verify SSRC
    let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
    assert_eq!(ssrc, 0x12345678);

    // Verify payload includes the NAL
    assert!(packet.len() > 12);
    assert_eq!(&packet[12..], &nal[..]);
}

#[test]
fn test_rtp_pusher_sequence_increment() {
    let addr: SocketAddr = "192.168.1.200:10000".parse().unwrap();
    let mut pusher = RtpPusher::new(addr, 0x12345678, H264_PAYLOAD_TYPE);

    let nal = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1E];

    // First packet
    let p1 = pusher.build_rtp_packet(&nal);
    let seq1 = u16::from_be_bytes([p1[2], p1[3]]);

    // Second packet should have incremented sequence number
    let p2 = pusher.build_rtp_packet(&nal);
    let seq2 = u16::from_be_bytes([p2[2], p2[3]]);

    assert_eq!(seq2, seq1.wrapping_add(1));
}

#[test]
fn test_rtp_pusher_timestamp_increment() {
    let addr: SocketAddr = "192.168.1.200:10000".parse().unwrap();
    let mut pusher = RtpPusher::new(addr, 0x12345678, H264_PAYLOAD_TYPE);

    assert_eq!(pusher.timestamp, 0);
    pusher.increment_timestamp(3000);
    assert_eq!(pusher.timestamp, 3000);
    pusher.increment_timestamp(3000);
    assert_eq!(pusher.timestamp, 6000);
}

// ─── BYE Request Tests ────────────────────────────────────────────────

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

// ─── Device Type Constants Tests ───────────────────────────────────────

#[test]
fn test_device_type_constants() {
    assert_eq!(device_types::IPC, 111);
    assert_eq!(device_types::NVR, 118);
    assert_eq!(device_types::DECODER, 121);
    assert_eq!(device_types::ALARM, 122);
    assert_eq!(device_types::AUDIO, 134);
}

// ─── RTP Stream Info Tests ─────────────────────────────────────────────

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

// ─── Transport Display Tests ───────────────────────────────────────────

#[test]
fn test_transport_display() {
    assert_eq!(Transport::Tcp.to_string(), "TCP");
    assert_eq!(Transport::Udp.to_string(), "UDP");
}

// ─── SIP Message Case Insensitive Tests ────────────────────────────────

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

// ─── SDP Missing Required Tests ────────────────────────────────────────

#[test]
fn test_sdp_missing_required() {
    assert!(SdpSession::parse("v=0\r\n").is_err());
    assert!(SdpSession::parse("").is_err());
}

// ─── 401 Challenge Missing Header Tests ────────────────────────────────

#[test]
fn test_parse_401_challenge_missing_header() {
    let response = "SIP/2.0 401 Unauthorized\r\n\
                    Content-Length: 0\r\n\
                    \r\n";
    let msg = SipMessage::parse(response).unwrap();
    assert!(parse_401_challenge(&msg).is_err());
}

// ─── InviteInfo Missing Call-ID ─────────────────────────────────────────

#[test]
fn test_parse_invite_missing_call_id() {
    let invite = "INVITE sip:test@test.com SIP/2.0\r\n\
                  Content-Type: application/sdp\r\n\
                  Content-Length: 50\r\n\
                  \r\n\
                  v=0\r\n\
                  o=- 0 0 IN IP4 127.0.0.1\r\n\
                  s=Test\r\n\
                  t=0 0\r\n";
    let msg = SipMessage::parse(invite).unwrap();
    assert!(parse_invite(&msg).is_err());
}

// ─── SDP y= SSRC Field Tests ───────────────────────────────────────────

#[test]
fn test_sdp_parses_y_ssrc_decimal() {
    let sdp = "v=0\r\n\
                o=- 0 0 IN IP4 192.168.1.1\r\n\
                s=Play\r\n\
                c=IN IP4 192.168.1.100\r\n\
                y=0100000001\r\n\
                t=0 0\r\n\
                m=video 0 RTP/AVP 96\r\n\
                a=sendonly\r\n\
                a=rtpmap:96 PS/90000\r\n";

    let parsed = SdpSession::parse(sdp).expect("Failed to parse SDP");
    assert_eq!(
        parsed.ssrc,
        Some(100000001),
        "SSRC should be parsed from y= field as decimal"
    );
}

#[test]
fn test_parse_invite_uses_y_field_not_a_ssrc() {
    // INVITE with y= field but no a=ssrc:
    let invite_raw = "INVITE sip:34020000201180000001@3402000000 SIP/2.0\r\n\
                         Via: SIP/2.0/UDP 192.168.1.100:5060;branch=z9hG4bK1234;rport\r\n\
                         From: <sip:34020000002000000001@3402000000>;tag=abc123\r\n\
                         To: <sip:34020000201180000001@3402000000>\r\n\
                         Call-ID: test-y-field-001\r\n\
                         CSeq: 1 INVITE\r\n\
                         Contact: <sip:192.168.1.200:5060>\r\n\
                         Content-Type: application/sdp\r\n\
                         Content-Length: 150\r\n\
                         \r\n\
                         v=0\r\n\
                         o=- 0 0 IN IP4 192.168.1.200\r\n\
                         s=Play\r\n\
                         c=IN IP4 192.168.1.200\r\n\
                         y=0100000001\r\n\
                         t=0 0\r\n\
                         m=video 20000 RTP/AVP 96\r\n\
                         a=sendonly\r\n\
                         a=rtpmap:96 PS/90000\r\n";

    let invite_msg = SipMessage::parse(invite_raw).expect("Failed to parse INVITE");
    let info = parse_invite(&invite_msg).expect("Failed to parse INVITE");

    assert_eq!(info.call_id, "test-y-field-001");
    assert_eq!(info.media_address, "192.168.1.200");
    assert_eq!(info.media_port, 20000);
    assert_eq!(
        info.ssrc, 100000001,
        "SSRC should come from y= field (decimal)"
    );
    assert_eq!(info.payload_type, 96);
}

// ─── Digest Auth MD5 Default Tests ─────────────────────────────────────

#[test]
fn test_digest_defaults_to_md5_when_absent() {
    let challenge = parse_digest_auth("Digest realm=\"TestRealm\", nonce=\"abcdef123456\"")
        .expect("Failed to parse challenge");

    assert_eq!(challenge.realm, "TestRealm");
    assert_eq!(challenge.nonce, "abcdef123456");
    assert_eq!(
        challenge.algorithm, None,
        "Algorithm should be None when not specified"
    );

    // When building auth header with no algorithm, should default to MD5
    let username = "testuser";
    let password = "testpass";
    let algorithm = challenge.algorithm.as_deref().unwrap_or("MD5");
    assert_eq!(
        algorithm, "MD5",
        "Should default to MD5 per RFC 2617 §3.2.1"
    );

    // Build auth header should work with MD5 algorithm
    let uri = "sip:34020000201180000001@3402000000";
    let auth_header = build_digest_auth(
        username,
        &challenge.realm,
        password,
        &challenge.nonce,
        uri,
        "REGISTER",
        algorithm,
        None,
    );

    assert!(
        auth_header.contains("algorithm=MD5"),
        "Auth header should specify MD5 algorithm"
    );
    assert!(auth_header.contains(&format!("username=\"{}\"", username)));
    assert!(auth_header.contains(&format!("realm=\"{}\"", challenge.realm)));
    assert!(auth_header.contains(&format!("nonce=\"{}\"", challenge.nonce)));
}

// ─── Digest Auth qop / algorithm Tests ─────────────────────────────────

#[test]
fn test_build_digest_auth_md5_rfc2617_vector() {
    // RFC 2617 §3.5 example (no qop):
    //   response = MD5(MD5(user:realm:pass):nonce:MD5(method:uri))
    let auth = build_digest_auth(
        "Mufasa",
        "testrealm@host.com",
        "Circle Of Life",
        "dcd98b7102dd2f0e8b11d0f600bfb0c093",
        "/dir/index.html",
        "GET",
        "MD5",
        None,
    );
    assert!(auth.contains("response=\"670fd8c2df070c60b045671b8b24ff02\""));
    assert!(auth.contains("algorithm=MD5"));
}

#[test]
fn test_build_digest_auth_qop_auth() {
    let auth = build_digest_auth(
        "Mufasa",
        "testrealm@host.com",
        "Circle Of Life",
        "dcd98b7102dd2f0e8b11d0f600bfb0c093",
        "/dir/index.html",
        "GET",
        "MD5",
        Some("auth"),
    );
    assert!(auth.contains("qop=auth"));
    assert!(auth.contains("nc=00000001"));
    assert!(auth.contains("cnonce=\""));
    let response = auth
        .split("response=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("response field present");
    assert_eq!(response.len(), 32, "MD5 response must be 32 hex chars");
    assert!(response.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_build_digest_auth_sha256_qop() {
    let auth = build_digest_auth(
        "user",
        "realm",
        "pass",
        "nonce",
        "sip:3402000000@3402000000",
        "REGISTER",
        "SHA-256",
        Some("auth"),
    );
    assert!(auth.contains("qop=auth"));
    assert!(auth.contains("nc=00000001"));
    let response = auth
        .split("response=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("response field present");
    assert_eq!(response.len(), 64, "SHA-256 response must be 64 hex chars");
}
