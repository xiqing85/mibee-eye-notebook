#![cfg_attr(test, deny(warnings))]

pub mod assets;
/// Axum REST API + static SPA
pub mod db;
pub mod routes;
pub mod server;
pub mod stream_manager;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
