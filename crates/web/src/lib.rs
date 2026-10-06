#![cfg_attr(test, deny(warnings))]

pub mod alarm;
pub mod assets;
pub mod cloud;
pub mod config;
/// Conversation records — the human-readable dialogue turn log (SPEC v1 §3.4).
pub mod conversations;
/// Conversation model-call chain tracing (SPEC v1 §3.3).
pub mod convtrace;
pub mod db;
/// Axum REST API + static SPA
pub mod envelope;
pub mod errors;
/// GB28181 voice-talkback receive: G.711 decode → cpal output.
pub mod gb28181_control;
pub mod gb28181_talkback;
pub mod grounding;
pub mod observe;
pub mod onvif_alarm;
pub mod protocol_runtime;
pub mod routes;
pub mod server;
pub mod stream_manager;
pub mod zones;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
