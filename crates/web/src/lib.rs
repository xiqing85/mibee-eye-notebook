#![cfg_attr(test, deny(warnings))]

pub mod assets;
/// Axum REST API + static SPA
pub mod db;
pub mod routes;
pub mod stream_manager;
pub mod server;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}

