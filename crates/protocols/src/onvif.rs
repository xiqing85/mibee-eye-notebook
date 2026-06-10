//! ONVIF device discovery, connection, and PTZ control.
//!
//! Thin wrapper around the [`oxvif`] crate providing:
//!
//! - **WS-Discovery** — find ONVIF cameras on the local network
//! - **Session management** — connect, read device info, enumerate profiles,
//!   resolve RTSP/snapshot URIs
//! - **PTZ control** — absolute/relative/continuous move, presets, home
//!   position, status query
//!
//! We do NOT reimplement the ONVIF SOAP protocol. [`oxvif`] handles
//! WS-Security, Digest auth, and all SOAP binding. This module maps those
//! capabilities into `notebook-cam`-idiomatic APIs.
//!
//! # Usage
//!
//! ```no_run
//! use std::time::Duration;
//! use protocols::onvif;
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Discover cameras on the network
//! let cameras = onvif::discover_devices(Duration::from_secs(5)).await?;
//!
//! // Connect to the first camera found
//! let session = onvif::connect(
//!     "http://192.168.1.100/onvif/device_service",
//!     "admin",
//!     "password",
//! ).await?;
//!
//! // Read device info
//! let info = session.get_device_info().await?;
//! println!("Camera: {} {}", info.manufacturer, info.model);
//!
//! // Get stream URI
//! let profiles = session.get_profiles().await?;
//! let stream_uri = session.get_stream_uri(&profiles[0].token).await?;
//! println!("RTSP: {}", stream_uri.uri);
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use anyhow::{Context, Result};

// Re-export oxvif types that are stable and useful to callers.
pub use oxvif::types::{DeviceInfo, MediaProfile, PtzPreset, PtzStatus, SnapshotUri, StreamUri};

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Discover ONVIF devices on the local network via WS-Discovery (UDP
/// multicast on port 3702).
///
/// `timeout` controls how long to wait for probe responses. Typical values
/// are 3–5 seconds.
///
/// Returns a list of [`DiscoveredDevice`] entries, each containing the
/// device's service URL (XAddrs), scopes, and type information.
pub use oxvif::discovery::DiscoveredDevice;

/// WS-Discovery probe for ONVIF cameras on the local network.
///
/// Sends a Probe message via UDP multicast and collects responses within
/// the given `timeout`.
pub async fn discover_devices(timeout: Duration) -> Result<Vec<DiscoveredDevice>> {
    let devices = oxvif::discovery::probe(timeout).await;
    Ok(devices)
}

// ── Session ───────────────────────────────────────────────────────────────────

/// An authenticated ONVIF session with cached service URLs.
///
/// Constructed via [`connect`]. All methods delegate to
/// [`oxvif::OnvifSession`] which resolves service endpoints from
/// `GetCapabilities` at construction time.
///
/// # Example
///
/// ```no_run
/// use protocols::onvif;
///
/// # async fn example() -> anyhow::Result<()> {
/// let session = onvif::connect(
///     "http://192.168.1.100/onvif/device_service",
///     "admin",
///     "password",
/// ).await?;
///
/// let info = session.get_device_info().await?;
/// println!("Manufacturer: {}", info.manufacturer);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct OnvifSession {
    inner: oxvif::OnvifSession,
}

impl OnvifSession {
    /// Wrap an existing [`oxvif::OnvifSession`].
    ///
    /// Useful in tests where a [`MockTransport`](oxvif::mock::MockTransport)
    /// is injected into the oxvif session before wrapping.
    #[doc(hidden)]
    pub fn from_inner(inner: oxvif::OnvifSession) -> Self {
        Self { inner }
    }

    /// Access the underlying [`oxvif::OnvifSession`] for operations not
    /// covered by this wrapper.
    #[doc(hidden)]
    pub fn inner(&self) -> &oxvif::OnvifSession {
        &self.inner
    }

    // ── Device Service ────────────────────────────────────────────────────────

    /// Retrieve manufacturer, model, firmware version, and serial number.
    pub async fn get_device_info(&self) -> Result<DeviceInfo> {
        self.inner
            .get_device_info()
            .await
            .context("failed to get ONVIF device info")
    }

    // ── Media Service ─────────────────────────────────────────────────────────

    /// List all media profiles (stream configurations).
    ///
    /// Each profile has a `token` and `name`. Use the token to resolve
    /// stream URIs or to control PTZ.
    pub async fn get_profiles(&self) -> Result<Vec<MediaProfile>> {
        self.inner
            .get_profiles()
            .await
            .context("failed to get ONVIF media profiles")
    }

    /// Get the RTSP stream URI for a given media profile.
    ///
    /// The returned [`StreamUri`] includes the URI string and metadata
    /// about when it expires.
    pub async fn get_stream_uri(&self, profile_token: &str) -> Result<StreamUri> {
        self.inner
            .get_stream_uri(profile_token)
            .await
            .context("failed to get ONVIF stream URI")
    }

    /// Get the HTTP snapshot (JPEG) URI for a given media profile.
    pub async fn get_snapshot_uri(&self, profile_token: &str) -> Result<SnapshotUri> {
        self.inner
            .get_snapshot_uri(profile_token)
            .await
            .context("failed to get ONVIF snapshot URI")
    }

    // ── PTZ Control ───────────────────────────────────────────────────────────

    /// Move the camera to an absolute pan/tilt/zoom position.
    ///
    /// Pan/tilt/zoom values are normalized to the range supported by the
    /// device (typically -1.0 to 1.0 or 0.0 to 1.0, depending on the
    /// PTZ node configuration).
    pub async fn ptz_absolute_move(
        &self,
        profile_token: &str,
        pan: f32,
        tilt: f32,
        zoom: f32,
    ) -> Result<()> {
        self.inner
            .ptz_absolute_move(profile_token, pan, tilt, zoom)
            .await
            .context("PTZ absolute move failed")
    }

    /// Move the camera by a relative offset from the current position.
    pub async fn ptz_relative_move(
        &self,
        profile_token: &str,
        pan: f32,
        tilt: f32,
        zoom: f32,
    ) -> Result<()> {
        self.inner
            .ptz_relative_move(profile_token, pan, tilt, zoom)
            .await
            .context("PTZ relative move failed")
    }

    /// Start continuous pan/tilt/zoom movement.
    ///
    /// Values control the direction and speed. Set a value to `0.0` to
    /// stop movement on that axis. Call [`ptz_stop`](Self::ptz_stop) to
    /// halt all movement.
    pub async fn ptz_continuous_move(
        &self,
        profile_token: &str,
        pan: f32,
        tilt: f32,
        zoom: f32,
    ) -> Result<()> {
        self.inner
            .ptz_continuous_move(profile_token, pan, tilt, zoom)
            .await
            .context("PTZ continuous move failed")
    }

    /// Stop all ongoing PTZ movement.
    pub async fn ptz_stop(&self, profile_token: &str) -> Result<()> {
        self.inner
            .ptz_stop(profile_token)
            .await
            .context("PTZ stop failed")
    }

    /// List all saved PTZ presets for the given profile.
    pub async fn ptz_get_presets(&self, profile_token: &str) -> Result<Vec<PtzPreset>> {
        self.inner
            .ptz_get_presets(profile_token)
            .await
            .context("failed to get PTZ presets")
    }

    /// Move the camera to a saved preset position.
    pub async fn ptz_goto_preset(&self, profile_token: &str, preset_token: &str) -> Result<()> {
        self.inner
            .ptz_goto_preset(profile_token, preset_token)
            .await
            .context("PTZ goto preset failed")
    }

    /// Save the current camera position as a named preset.
    ///
    /// Returns the token of the newly created preset.
    pub async fn ptz_set_preset(
        &self,
        profile_token: &str,
        preset_name: Option<&str>,
        preset_token: Option<&str>,
    ) -> Result<String> {
        self.inner
            .ptz_set_preset(profile_token, preset_name, preset_token)
            .await
            .context("failed to set PTZ preset")
    }

    /// Delete a saved PTZ preset.
    pub async fn ptz_remove_preset(&self, profile_token: &str, preset_token: &str) -> Result<()> {
        self.inner
            .ptz_remove_preset(profile_token, preset_token)
            .await
            .context("failed to remove PTZ preset")
    }

    /// Query the current PTZ position and movement state.
    pub async fn ptz_get_status(&self, profile_token: &str) -> Result<PtzStatus> {
        self.inner
            .ptz_get_status(profile_token)
            .await
            .context("failed to get PTZ status")
    }

    /// Move the camera to its configured home position.
    ///
    /// `speed` is optional (0.0–1.0); `None` uses the device default.
    pub async fn ptz_goto_home_position(
        &self,
        profile_token: &str,
        speed: Option<f32>,
    ) -> Result<()> {
        self.inner
            .ptz_goto_home_position(profile_token, speed)
            .await
            .context("PTZ goto home position failed")
    }

    /// Set the current PTZ position as the home position.
    pub async fn ptz_set_home_position(&self, profile_token: &str) -> Result<()> {
        self.inner
            .ptz_set_home_position(profile_token)
            .await
            .context("failed to set PTZ home position")
    }
}

// ── Connection ────────────────────────────────────────────────────────────────

/// Connect to an ONVIF device and return an authenticated session.
///
/// `device_url` should point to the ONVIF device service endpoint, e.g.
/// `http://192.168.1.100/onvif/device_service`.
///
/// Automatically performs:
/// 1. WS-Security `UsernameToken` authentication
/// 2. Clock sync via `GetSystemDateAndTime` to avoid timestamp drift
/// 3. `GetCapabilities` to cache all service URLs
pub async fn connect(device_url: &str, username: &str, password: &str) -> Result<OnvifSession> {
    let inner = oxvif::OnvifSession::builder(device_url)
        .with_credentials(username, password)
        .with_clock_sync()
        .build()
        .await
        .context("failed to connect to ONVIF device")?;
    Ok(OnvifSession { inner })
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use oxvif::mock::MockTransport;
    use std::sync::Arc;

    /// Build an OnvifSession backed by a MockTransport for testing.
    async fn mock_session(transport: MockTransport) -> OnvifSession {
        let inner = oxvif::OnvifSession::builder("http://mock")
            .with_credentials("admin", "admin")
            .with_transport(Arc::new(transport))
            .build()
            .await
            .expect("mock session build should succeed");
        OnvifSession { inner }
    }

    // ── Device Info ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_device_info() {
        let t = MockTransport::new();
        let session = mock_session(t).await;

        let info = session.get_device_info().await.unwrap();
        assert_eq!(info.manufacturer, "oxvif-mock");
        assert_eq!(info.model, "MockCam-1080p");
        assert_eq!(info.firmware_version, "1.0.0");
        assert_eq!(info.serial_number, "MOCK-0001");
        assert_eq!(info.hardware_id, "1.0");
    }

    // ── Media Profiles ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_profiles() {
        let t = MockTransport::new();
        let session = mock_session(t).await;

        let profiles = session.get_profiles().await.unwrap();
        assert!(!profiles.is_empty(), "should have at least one profile");

        let main = profiles.iter().find(|p| p.name == "mainStream");
        assert!(main.is_some(), "should have a mainStream profile");
        assert_eq!(main.unwrap().token, "Profile_1");

        let sub = profiles.iter().find(|p| p.name == "subStream");
        assert!(sub.is_some(), "should have a subStream profile");
        assert_eq!(sub.unwrap().token, "Profile_2");
    }

    #[tokio::test]
    async fn test_get_profiles_returns_multiple() {
        let t = MockTransport::new();
        let session = mock_session(t).await;

        let profiles = session.get_profiles().await.unwrap();
        assert!(profiles.len() >= 2, "mock should seed at least 2 profiles");
    }

    // ── Stream URIs ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_stream_uri() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let uri = session.get_stream_uri(&profiles[0].token).await.unwrap();
        assert!(!uri.uri.is_empty(), "stream URI should not be empty");
        assert!(
            uri.uri.starts_with("rtsp://"),
            "stream URI should be RTSP: {}",
            uri.uri
        );
    }

    #[tokio::test]
    async fn test_get_stream_uri_all_profiles() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        for profile in &profiles {
            let uri = session.get_stream_uri(&profile.token).await.unwrap();
            assert!(
                uri.uri.starts_with("rtsp://"),
                "profile '{}' URI should be RTSP: {}",
                profile.name,
                uri.uri
            );
        }
    }

    // ── Snapshot URIs ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_snapshot_uri() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let uri = session.get_snapshot_uri(&profiles[0].token).await.unwrap();
        assert!(!uri.uri.is_empty(), "snapshot URI should not be empty");
    }

    // ── PTZ Control ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_ptz_get_presets() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let presets = session.ptz_get_presets(&profiles[0].token).await.unwrap();
        assert!(!presets.is_empty(), "mock should seed PTZ presets");

        let home = presets.iter().find(|p| p.name == "Home");
        assert!(home.is_some(), "should have a 'Home' preset");
    }

    #[tokio::test]
    async fn test_ptz_absolute_move() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        session
            .ptz_absolute_move(&profiles[0].token, 0.5, 0.3, 0.0)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_relative_move() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        session
            .ptz_relative_move(&profiles[0].token, 0.1, -0.05, 0.0)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_continuous_move() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        session
            .ptz_continuous_move(&profiles[0].token, 0.2, 0.0, 0.0)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_stop() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        // Start then stop
        session
            .ptz_continuous_move(&profiles[0].token, 0.5, 0.0, 0.0)
            .await
            .unwrap();
        session.ptz_stop(&profiles[0].token).await.unwrap();
    }

    #[tokio::test]
    async fn test_ptz_goto_preset() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        session
            .ptz_goto_preset(&profiles[0].token, "Preset_1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_set_preset() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let token = session
            .ptz_set_preset(&profiles[0].token, Some("MyView"), None)
            .await
            .unwrap();
        assert!(!token.is_empty(), "new preset token should not be empty");
    }

    #[tokio::test]
    async fn test_ptz_remove_preset() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        // Set then remove
        let token = session
            .ptz_set_preset(&profiles[0].token, Some("Temp"), None)
            .await
            .unwrap();
        session
            .ptz_remove_preset(&profiles[0].token, &token)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_get_status() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let status = session.ptz_get_status(&profiles[0].token).await.unwrap();
        // Mock returns defaults (all 0.0)
        assert_eq!(status.pan, Some(0.0));
        assert_eq!(status.tilt, Some(0.0));
        assert_eq!(status.zoom, Some(0.0));
    }

    #[tokio::test]
    async fn test_ptz_goto_home_position() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        // Without speed
        session
            .ptz_goto_home_position(&profiles[0].token, None)
            .await
            .unwrap();

        // With speed
        session
            .ptz_goto_home_position(&profiles[0].token, Some(0.5))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_set_home_position() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        session
            .ptz_set_home_position(&profiles[0].token)
            .await
            .unwrap();
    }

    // ── PTZ on sub-stream profile ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_ptz_on_sub_stream() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let sub = profiles.iter().find(|p| p.name == "subStream").unwrap();
        session
            .ptz_absolute_move(&sub.token, 0.0, 0.0, 0.0)
            .await
            .unwrap();
        let presets = session.ptz_get_presets(&sub.token).await.unwrap();
        assert!(!presets.is_empty());
    }

    // ── Error injection ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_injected_fault_returns_error() {
        let t = MockTransport::new();
        t.inject_fault("GetProfiles", "ter:NotAuthorized", "simulated denial");
        let session = mock_session(t).await;

        let result = session.get_profiles().await;
        assert!(result.is_err(), "injected fault should cause error");
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("NotAuthorized") || msg.contains("simulated"),
            "error message should mention the fault: {msg}"
        );
    }

    #[tokio::test]
    async fn test_injected_fault_then_healthy() {
        let t = MockTransport::new();
        t.inject_fault(
            "GetDeviceInformation",
            "ter:NotAuthorized",
            "first-call deny",
        );
        let session = mock_session(t).await;

        // First call fails
        let result1 = session.get_device_info().await;
        assert!(
            result1.is_err(),
            "first call should fail with injected fault"
        );

        // Second call should succeed (fault is single-shot)
        let info = session.get_device_info().await.unwrap();
        assert_eq!(info.manufacturer, "oxvif-mock");
    }

    // ── Full workflow ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_full_workflow() {
        // Simulate a realistic workflow: connect → info → profiles
        // → stream URI → PTZ preset → goto preset → status → stop
        let t = MockTransport::new();
        let session = mock_session(t).await;

        // Device info
        let info = session.get_device_info().await.unwrap();
        assert_eq!(info.manufacturer, "oxvif-mock");

        // Profiles
        let profiles = session.get_profiles().await.unwrap();
        assert!(!profiles.is_empty());
        let main = &profiles[0];

        // Stream URI
        let uri = session.get_stream_uri(&main.token).await.unwrap();
        assert!(uri.uri.starts_with("rtsp://"));

        // PTZ presets
        let presets = session.ptz_get_presets(&main.token).await.unwrap();
        assert!(!presets.is_empty());

        // Goto home preset
        session
            .ptz_goto_preset(&main.token, "Preset_1")
            .await
            .unwrap();

        // Status
        let status = session.ptz_get_status(&main.token).await.unwrap();
        assert_eq!(status.pan, Some(0.0));

        // Absolute move
        session
            .ptz_absolute_move(&main.token, 0.5, 0.0, 0.0)
            .await
            .unwrap();
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_ptz_move_with_zero_values() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        // Moving to (0,0,0) should be valid
        session
            .ptz_absolute_move(&profiles[0].token, 0.0, 0.0, 0.0)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ptz_continuous_move_single_axis() {
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        // Zoom only
        session
            .ptz_continuous_move(&profiles[0].token, 0.0, 0.0, 0.5)
            .await
            .unwrap();

        // Tilt only
        session
            .ptz_continuous_move(&profiles[0].token, 0.0, -0.3, 0.0)
            .await
            .unwrap();

        session.ptz_stop(&profiles[0].token).await.unwrap();
    }

    #[tokio::test]
    async fn test_preset_roundtrip() {
        // Set → Get should include the new preset
        let t = MockTransport::new();
        let session = mock_session(t).await;
        let profiles = session.get_profiles().await.unwrap();

        let before = session.ptz_get_presets(&profiles[0].token).await.unwrap();
        let before_count = before.len();

        let new_token = session
            .ptz_set_preset(&profiles[0].token, Some("NewView"), None)
            .await
            .unwrap();

        let after = session.ptz_get_presets(&profiles[0].token).await.unwrap();
        assert_eq!(
            after.len(),
            before_count + 1,
            "preset count should increase by 1"
        );
        assert!(
            after.iter().any(|p| p.token == new_token),
            "new preset token should appear in presets"
        );
    }
}
