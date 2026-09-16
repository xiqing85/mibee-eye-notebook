#![cfg_attr(test, deny(warnings))]

pub mod alarm;
pub mod assets;
pub mod config;
pub mod db;
/// Axum REST API + static SPA
pub mod envelope;
pub mod errors;
/// GB28181 voice-talkback receive: G.711 decode → cpal output.
pub mod gb28181_control;
pub mod gb28181_talkback;
pub mod observe;
pub mod protocol_runtime;
pub mod routes;
pub mod server;
pub mod stream_manager;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
