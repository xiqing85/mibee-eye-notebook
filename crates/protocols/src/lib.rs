#![cfg_attr(test, deny(warnings))]

pub mod audio_codec;
/// ONVIF, GB28181, RTSP, RTMP clients
pub mod rtmp;

pub mod gb28181;
pub mod h264;
pub mod onvif;
pub mod rtp;
pub mod rtsp;
pub mod rtsp_server;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
