//! ONVIF Device endpoint — serves device metadata for NVR discovery.
//!
//! This machine IS the ONVIF camera. External NVRs discover it via
//! WS-Discovery (UDP 3702) and query device information via SOAP/HTTP.
//!
//! # Architecture
//!
//! - [`WsDiscoveryServer`] listens on UDP port 3702 for WS-Discovery
//!   Probe messages and responds with [`ProbeMatch`] containing device
//!   XAddrs, types, and scopes.
//! - [`SoapDeviceService`] serves SOAP responses for `GetDeviceInformation`,
//!   `GetProfiles`, and `GetStreamUri` — intended to be wired into an
//!   HTTP server (axum, tokio TcpListener, etc.).
//!
//! All SOAP/XML is built with hand-written string templates. No SOAP
//! library or WS-Security is needed — ONVIF discovery is unauthenticated.
//!
//! # Protocol Direction
//!
//! **OUTBOUND**: External NVRs discover THIS machine. This module does
//! NOT implement ONVIF client features (discovery, PTZ, event subscriptions).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use observability::metrics;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};
// ═══════════════════════════════════════════════════════════════════════════
// Device Configuration
// ═══════════════════════════════════════════════════════════════════════════

/// ONVIF device metadata returned in SOAP responses.
#[derive(Debug, Clone)]
pub struct OnvifDeviceConfig {
    /// Manufacturer name (e.g., "notebook-cam")
    pub manufacturer: String,
    /// Model identifier
    pub model: String,
    /// Firmware version string
    pub firmware_version: String,
    /// Device serial number
    pub serial_number: String,
    /// Hardware revision identifier
    pub hardware_id: String,
    /// List of XAddrs (URIs) advertised via WS-Discovery ProbeMatch.
    /// Typically points to the SOAP device service endpoint, e.g.
    /// `http://192.168.1.100:8080/onvif/device_service`.
    pub xaddrs: Vec<String>,
    /// RTSP base URL for GetStreamUri responses.
    /// Set to the local RTSP server address, e.g.
    /// `rtsp://192.168.1.100:8554/webcam`.
    pub rtsp_url: String,
    /// Scopes advertised in WS-Discovery ProbeMatch.
    pub scopes: Vec<String>,
}

impl Default for OnvifDeviceConfig {
    fn default() -> Self {
        Self {
            manufacturer: "notebook-cam".into(),
            model: "NB-CAM-1".into(),
            firmware_version: "1.0.0".into(),
            serial_number: "NB-000001".into(),
            hardware_id: "1.0".into(),
            xaddrs: vec!["http://localhost:8080/onvif/device_service".into()],
            rtsp_url: "rtsp://localhost:8554/webcam".into(),
            scopes: vec![
                "onvif://www.onvif.org/type/NetworkVideoTransmitter".into(),
                "onvif://www.onvif.org/hardware/NB-CAM-1".into(),
                "onvif://www.onvif.org/location/".into(),
            ],
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SOAP/XML Builders
// ═══════════════════════════════════════════════════════════════════════════

const SOAP_ENVELOPE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope"
    xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing"
    xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery"
    xmlns:dn="http://www.onvif.org/ver10/network/wsdl">"#;

/// Build a WS-Discovery ProbeMatch response XML string.
///
/// `relates_to` is the MessageID from the incoming Probe that this
/// response matches. Pass `None` for tests that don't need the correlation.
pub fn build_probe_match_xml(config: &OnvifDeviceConfig, relates_to: Option<&str>) -> String {
    let uuid = generate_uuid();
    let xaddrs = config.xaddrs.join(" ");
    let scopes = config.scopes.join(" ");
    let types = "dn:NetworkVideoTransmitter";

    let relates_to_tag = match relates_to {
        Some(rel) => format!("    <wsa:RelatesTo>{rel}</wsa:RelatesTo>\n"),
        None => String::new(),
    };

    format!(
        r#"{soap}
 <soap:Header>
  <wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/ProbeMatches</wsa:Action>
  <wsa:MessageID>{uuid}</wsa:MessageID>
{relates_to_tag}  <wsa:To>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:To>
 </soap:Header>
 <soap:Body>
  <wsd:ProbeMatches>
   <wsd:ProbeMatch>
    <wsa:EndpointReference>
     <wsa:Address>{uuid}</wsa:Address>
    </wsa:EndpointReference>
    <wsd:Types>{types}</wsd:Types>
    <wsd:Scopes>{scopes}</wsd:Scopes>
    <wsd:XAddrs>{xaddrs}</wsd:XAddrs>
    <wsd:MetadataVersion>1</wsd:MetadataVersion>
   </wsd:ProbeMatch>
  </wsd:ProbeMatches>
 </soap:Body>
</soap:Envelope>"#,
        soap = SOAP_ENVELOPE,
        uuid = uuid,
        relates_to_tag = relates_to_tag,
        types = types,
        scopes = scopes,
        xaddrs = xaddrs,
    )
}

/// Build a SOAP `GetDeviceInformation` response XML string.
pub fn build_get_device_info_response(config: &OnvifDeviceConfig) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope"
    xmlns:tds="http://www.onvif.org/ver10/device/wsdl">
 <soap:Body>
  <tds:GetDeviceInformationResponse>
   <tds:Manufacturer>{manufacturer}</tds:Manufacturer>
   <tds:Model>{model}</tds:Model>
   <tds:FirmwareVersion>{firmware}</tds:FirmwareVersion>
   <tds:SerialNumber>{serial}</tds:SerialNumber>
   <tds:HardwareId>{hardware}</tds:HardwareId>
  </tds:GetDeviceInformationResponse>
 </soap:Body>
</soap:Envelope>"#,
        manufacturer = config.manufacturer,
        model = config.model,
        firmware = config.firmware_version,
        serial = config.serial_number,
        hardware = config.hardware_id,
    )
}

/// Build a SOAP `GetProfiles` response XML string with a single profile.
pub fn build_get_profiles_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope"
    xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
 <soap:Body>
  <trt:GetProfilesResponse>
   <trt:Profiles token="Profile_1">
    <trt:Name>mainStream</trt:Name>
    <trt:VideoSourceConfiguration token="VideoSource_1">
     <trt:Name>Video Source</trt:Name>
     <trt:SourceToken>Video_1</trt:SourceToken>
    </trt:VideoSourceConfiguration>
    <trt:VideoEncoderConfiguration token="Encoder_1">
     <trt:Name>Main Stream</trt:Name>
     <trt:Resolution>
      <trt:Width>1920</trt:Width>
      <trt:Height>1080</trt:Height>
     </trt:Resolution>
    </trt:VideoEncoderConfiguration>
   </trt:Profiles>
  </trt:GetProfilesResponse>
 </soap:Body>
</soap:Envelope>"#
        .to_string()
}

/// Build a SOAP `GetStreamUri` response XML string.
pub fn build_get_stream_uri_response(rtsp_url: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope"
    xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
 <soap:Body>
  <trt:GetStreamUriResponse>
   <trt:MediaUri>
    <trt:Uri>{url}</trt:Uri>
    <trt:InvalidAfterConnect>false</trt:InvalidAfterConnect>
    <trt:InvalidAfterReboot>false</trt:InvalidAfterReboot>
    <trt:Timeout>PT30S</trt:Timeout>
   </trt:MediaUri>
  </trt:GetStreamUriResponse>
 </soap:Body>
</soap:Envelope>"#,
        url = rtsp_url,
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Probe Handling
// ═══════════════════════════════════════════════════════════════════════════

/// Extract the `wsa:MessageID` from a WS-Discovery Probe XML string.
pub fn extract_message_id(xml: &str) -> Option<String> {
    // Look for <wsa:MessageID> or <MessageID> tag
    for tag in &["<wsa:MessageID>", "<MessageID>"] {
        if let Some(start) = xml.find(tag) {
            let after = start + tag.len();
            let close_tags = &["</wsa:MessageID>", "</MessageID>"];
            for close in close_tags {
                if let Some(end) = xml[after..].find(close) {
                    return Some(xml[after..after + end].trim().to_string());
                }
            }
        }
    }
    None
}

/// Handle a WS-Discovery Probe message and return a ProbeMatch response.
///
/// Returns `None` if the message does not appear to be a Probe.
pub fn handle_probe_message(body: &str, config: &OnvifDeviceConfig) -> Option<String> {
    // Simple check: look for Probe action marker
    if !body.contains("Probe") && !body.contains("wsdiscovery") {
        return None;
    }
    metrics::increment_onvif_discovery_requests();
    let message_id = extract_message_id(body);
    Some(build_probe_match_xml(config, message_id.as_deref()))
}

// ═══════════════════════════════════════════════════════════════════════════
// WS-Discovery Server
// ═══════════════════════════════════════════════════════════════════════════

/// WS-Discovery server listening on UDP port 3702.
///
/// Responds to WS-Discovery Probe messages with ProbeMatch containing
/// device metadata so that external NVRs can discover this host.
///
/// # Example
///
/// ```ignore
/// use crate::onvif::{WsDiscoveryServer, OnvifDeviceConfig};
///
/// # async fn example() -> anyhow::Result<()> {
/// let config = OnvifDeviceConfig::default();
/// let server = WsDiscoveryServer::bind(config, "0.0.0.0:3702").await?;
/// tokio::spawn(async move {
///     server.run().await.ok();
/// });
/// # Ok(())
/// # }
/// ```
pub struct WsDiscoveryServer {
    config: Arc<OnvifDeviceConfig>,
    socket: Arc<UdpSocket>,
}

impl WsDiscoveryServer {
    /// Create and bind a WS-Discovery server to the given address.
    ///
    /// The standard ONVIF port is 3702. Use `0.0.0.0:3702` to bind on all
    /// interfaces, or `127.0.0.1:3702` for local-only testing.
    pub async fn bind(config: OnvifDeviceConfig, addr: &str) -> Result<Self> {
        let socket = UdpSocket::bind(addr)
            .await
            .with_context(|| format!("failed to bind WS-Discovery to {addr}"))?;
        info!("WS-Discovery server listening on {addr}");
        Ok(Self {
            config: Arc::new(config),
            socket: Arc::new(socket),
        })
    }

    /// Return the local socket address.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.socket
            .local_addr()
            .context("failed to get WS-Discovery local address")
    }

    /// Run the WS-Discovery event loop.
    ///
    /// Listens for UDP datagrams, matches Probe messages, and sends
    /// ProbeMatch responses. Runs indefinitely; drop the future to stop.
    pub async fn run(&self) -> Result<()> {
        let mut buf = vec![0u8; 65535];
        loop {
            let (len, src) = self.socket.recv_from(&mut buf).await?;
            let data = &buf[..len];
            debug!("WS-Discovery received {} bytes from {}", len, src);

            let body = match std::str::from_utf8(data) {
                Ok(s) => s,
                Err(_) => {
                    debug!("Ignoring non-UTF8 WS-Discovery message from {src}");
                    continue;
                }
            };

            if let Some(response) = handle_probe_message(body, &self.config) {
                if let Err(e) = self.socket.send_to(response.as_bytes(), src).await {
                    warn!("Failed to send ProbeMatch to {src}: {e}");
                } else {
                    debug!("Sent ProbeMatch to {src}");
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SOAP Device Service
// ═══════════════════════════════════════════════════════════════════════════

/// SOAP device service that responds to ONVIF queries.
///
/// This is a **stateless** XML builder — it parses the SOAP action from
/// the request body and returns the appropriate response XML. Intended
/// to be wired into an HTTP endpoint (axum, tokio TcpListener, etc.).
///
/// # How to integrate
///
/// ```ignore
/// POST /onvif/device_service HTTP/1.1
/// Content-Type: application/soap+xml
///
/// <?xml ...>
///   <soap:Body>
///     <tds:GetDeviceInformation/>
///   </soap:Body>
/// </soap:Envelope>
/// ```
///
/// Respond with `Content-Type: application/soap+xml` and the XML from
/// [`SoapDeviceService::handle_request`].
pub struct SoapDeviceService {
    config: OnvifDeviceConfig,
}

impl SoapDeviceService {
    /// Create a new SOAP device service with the given device config.
    pub fn new(config: OnvifDeviceConfig) -> Self {
        Self { config }
    }

    /// Parse a SOAP request body and return the appropriate response XML.
    ///
    /// Recognized actions:
    /// - `GetDeviceInformation`
    /// - `GetProfiles`
    /// - `GetStreamUri`
    ///
    /// Returns `None` if the action is not recognized (caller should
    /// respond with a SOAP fault).
    pub fn handle_request(&self, body: &str) -> Option<String> {
        if body.contains("GetDeviceInformation") {
            Some(build_get_device_info_response(&self.config))
        } else if body.contains("GetProfiles") {
            Some(build_get_profiles_response())
        } else if body.contains("GetStreamUri") {
            Some(build_get_stream_uri_response(&self.config.rtsp_url))
        } else {
            None
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Generate a random UUID v4 string in the format `uuid:xxxxxxxx-...`.
fn generate_uuid() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.r#gen();
    format!(
        "uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn test_config() -> OnvifDeviceConfig {
        OnvifDeviceConfig {
            manufacturer: "TestMaker".into(),
            model: "TestCam-4K".into(),
            firmware_version: "2.1.0".into(),
            serial_number: "SN-00042".into(),
            hardware_id: "rev-b".into(),
            xaddrs: vec!["http://10.0.0.1:8080/onvif/device_service".into()],
            rtsp_url: "rtsp://10.0.0.1:8554/teststream".into(),
            scopes: vec![
                "onvif://www.onvif.org/type/NetworkVideoTransmitter".into(),
                "onvif://www.onvif.org/hardware/TestCam-4K".into(),
            ],
        }
    }

    // ── Probe XML Parsing ────────────────────────────────────────────────

    #[test]
    fn test_extract_message_id_present() {
        let xml = r#"<?xml version="1.0"?>
<soap:Envelope>
 <soap:Header>
  <wsa:MessageID>uuid:a1b2c3d4-e5f6-7890-abcd-ef1234567890</wsa:MessageID>
 </soap:Header>
</soap:Envelope>"#;
        let id = extract_message_id(xml);
        assert_eq!(
            id.as_deref(),
            Some("uuid:a1b2c3d4-e5f6-7890-abcd-ef1234567890")
        );
    }

    #[test]
    fn test_extract_message_id_absent() {
        let xml = r#"<soap:Envelope><soap:Body/></soap:Envelope>"#;
        assert!(extract_message_id(xml).is_none());
    }

    #[test]
    fn test_handle_probe_message_recognized() {
        let xml = r#"<?xml version="1.0"?>
<soap:Envelope>
 <soap:Header>
  <wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</wsa:Action>
  <wsa:MessageID>uuid:abc-123</wsa:MessageID>
 </soap:Header>
 <soap:Body>
  <wsd:Probe>
   <wsd:Types>dn:NetworkVideoTransmitter</wsd:Types>
  </wsd:Probe>
 </soap:Body>
</soap:Envelope>"#;
        let config = test_config();
        let response = handle_probe_message(xml, &config);
        assert!(response.is_some(), "should recognize Probe message");
        let resp = response.unwrap();
        assert!(
            resp.contains("ProbeMatches"),
            "response should be ProbeMatch"
        );
        assert!(resp.contains("uuid:abc-123"), "should include RelatesTo");
        assert!(
            resp.contains("http://10.0.0.1:8080/onvif/device_service"),
            "should include XAddrs"
        );
    }

    #[test]
    fn test_handle_probe_message_non_probe() {
        let xml = r#"<hello>world</hello>"#;
        let config = test_config();
        assert!(handle_probe_message(xml, &config).is_none());
    }

    // ── ProbeMatch XML Generation ────────────────────────────────────────

    #[test]
    fn test_build_probe_match_response() {
        let config = test_config();
        let xml = build_probe_match_xml(&config, None);

        // Verify structure
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("ProbeMatches"));
        assert!(xml.contains("ProbeMatch"));

        // Verify device metadata
        assert!(xml.contains("http://10.0.0.1:8080/onvif/device_service"));
        assert!(xml.contains("NetworkVideoTransmitter"));
        assert!(xml.contains("TestCam-4K"));
        assert!(xml.contains("MetadataVersion"));
    }

    #[test]
    fn test_build_probe_match_with_relates_to() {
        let config = test_config();
        let xml = build_probe_match_xml(&config, Some("uuid:incoming-msg-42"));

        assert!(xml.contains("uuid:incoming-msg-42"));
        assert!(xml.contains("wsa:RelatesTo"));
    }

    // ── GetDeviceInformation ─────────────────────────────────────────────

    #[test]
    fn test_build_get_device_info_response() {
        let config = test_config();
        let xml = build_get_device_info_response(&config);

        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("GetDeviceInformationResponse"));
        assert!(xml.contains("<tds:Manufacturer>TestMaker</tds:Manufacturer>"));
        assert!(xml.contains("<tds:Model>TestCam-4K</tds:Model>"));
        assert!(xml.contains("<tds:FirmwareVersion>2.1.0</tds:FirmwareVersion>"));
        assert!(xml.contains("<tds:SerialNumber>SN-00042</tds:SerialNumber>"));
        assert!(xml.contains("<tds:HardwareId>rev-b</tds:HardwareId>"));
    }

    // ── GetProfiles ──────────────────────────────────────────────────────

    #[test]
    fn test_build_get_profiles_response() {
        let xml = build_get_profiles_response();

        assert!(xml.contains("GetProfilesResponse"));
        assert!(xml.contains("Profile_1"));
        assert!(xml.contains("mainStream"));
        assert!(xml.contains("1920"));
        assert!(xml.contains("1080"));
        assert!(xml.contains("VideoSourceConfiguration"));
        assert!(xml.contains("VideoEncoderConfiguration"));
    }

    // ── GetStreamUri ─────────────────────────────────────────────────────

    #[test]
    fn test_build_get_stream_uri_response() {
        let xml = build_get_stream_uri_response("rtsp://10.0.0.1:8554/teststream");

        assert!(xml.contains("GetStreamUriResponse"));
        assert!(xml.contains("<trt:Uri>rtsp://10.0.0.1:8554/teststream</trt:Uri>"));
        assert!(xml.contains("<trt:InvalidAfterConnect>false</trt:InvalidAfterConnect>"));
        assert!(xml.contains("<trt:Timeout>PT30S</trt:Timeout>"));
    }

    // ── SOAP Device Service ──────────────────────────────────────────────

    #[test]
    fn test_soap_device_service_get_device_info() {
        let service = SoapDeviceService::new(test_config());
        let body = r#"<soap:Body><tds:GetDeviceInformation/></soap:Body>"#;
        let response = service.handle_request(body);
        assert!(response.is_some());
        assert!(response.unwrap().contains("TestMaker"));
    }

    #[test]
    fn test_soap_device_service_get_profiles() {
        let service = SoapDeviceService::new(test_config());
        let body = r#"<soap:Body><trt:GetProfiles/></soap:Body>"#;
        let response = service.handle_request(body);
        assert!(response.is_some());
        assert!(response.unwrap().contains("Profile_1"));
    }

    #[test]
    fn test_soap_device_service_get_stream_uri() {
        let service = SoapDeviceService::new(test_config());
        let body = r#"<soap:Body><trt:GetStreamUri/></soap:Body>"#;
        let response = service.handle_request(body);
        assert!(response.is_some());
        let xml = response.unwrap();
        assert!(xml.contains("rtsp://10.0.0.1:8554/teststream"));
    }

    #[test]
    fn test_soap_device_service_unknown_action() {
        let service = SoapDeviceService::new(test_config());
        let body = r#"<soap:Body><tds:RebootSystem/></soap:Body>"#;
        assert!(service.handle_request(body).is_none());
    }

    // ── WS-Discovery Server ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_ws_discovery_server_bind() {
        let config = test_config();
        let server = WsDiscoveryServer::bind(config, "127.0.0.1:0")
            .await
            .expect("should bind to random port");
        let addr = server.local_addr().unwrap();
        assert!(addr.port() > 0, "should be bound to a port");
    }

    #[tokio::test]
    async fn test_ws_discovery_server_send_probe() {
        let config = test_config();
        let server = WsDiscoveryServer::bind(config, "127.0.0.1:0")
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();

        // Spawn the server's event loop in the background
        let handle = tokio::spawn(async move {
            server.run().await.ok();
        });

        // Brief pause to ensure server is ready
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Create a client socket
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // Send a Probe message to the server
        let probe_xml = concat!(
            "<?xml version=\"1.0\"?>\n",
            "<soap:Envelope>\n",
            " <soap:Header>\n",
            "  <wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</wsa:Action>\n",
            "  <wsa:MessageID>uuid:test-probe-1</wsa:MessageID>\n",
            " </soap:Header>\n",
            " <soap:Body>\n",
            "  <wsd:Probe>\n",
            "   <wsd:Types>dn:NetworkVideoTransmitter</wsd:Types>\n",
            "  </wsd:Probe>\n",
            " </soap:Body>\n",
            "</soap:Envelope>\n",
        );

        client.send_to(probe_xml.as_bytes(), addr).await.unwrap();

        // Read the response with a timeout
        let mut buf = vec![0u8; 65535];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.recv_from(&mut buf),
        )
        .await;

        // Stop the server loop
        handle.abort();

        let (len, _src) = result
            .expect("should receive response within timeout")
            .expect("recv should succeed");
        let response = String::from_utf8_lossy(&buf[..len]);

        assert!(response.contains("ProbeMatches"), "should get ProbeMatch");
        assert!(
            response.contains("http://10.0.0.1:8080/onvif/device_service"),
            "should contain XAddrs"
        );
    }

    // ── UUID generation ──────────────────────────────────────────────────

    #[test]
    fn test_generate_uuid_format() {
        let uuid = generate_uuid();
        assert!(uuid.starts_with("uuid:"), "should start with uuid:");
        // uuid:xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        let body = &uuid[5..];
        assert_eq!(body.len(), 36, "UUID body should be 36 chars");
        assert_eq!(
            body.chars().filter(|&c| c == '-').count(),
            4,
            "should have 4 hyphens"
        );
    }

    // ── OnvifDeviceConfig defaults ───────────────────────────────────────

    #[test]
    fn test_default_config() {
        let config = OnvifDeviceConfig::default();
        assert_eq!(config.manufacturer, "notebook-cam");
        assert_eq!(config.model, "NB-CAM-1");
        assert_eq!(config.firmware_version, "1.0.0");
        assert_eq!(config.serial_number, "NB-000001");
        assert!(!config.xaddrs.is_empty());
        assert_eq!(config.rtsp_url, "rtsp://localhost:8554/webcam");
        assert!(!config.scopes.is_empty());
    }
}
