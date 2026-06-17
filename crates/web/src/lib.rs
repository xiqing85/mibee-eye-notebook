#![cfg_attr(test, deny(warnings))]

pub mod assets;
pub mod config;
pub mod db;
/// Axum REST API + static SPA
pub mod errors;
pub mod routes;
pub mod protocol_runtime;
pub mod server;
pub mod stream_manager;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
