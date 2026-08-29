#![cfg_attr(test, deny(warnings))]

pub mod audio_codec;
/// ONVIF, GB28181, RTSP, RTMP clients
pub mod rtmp;

pub mod h264;
pub mod rtcp;
pub mod rtp;
pub mod rtsp_server;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        // Intentionally empty — verifies the crate compiles and links in test mode
    }
}
