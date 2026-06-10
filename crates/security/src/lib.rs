#![cfg_attr(test, deny(warnings))]

//! Auth, TLS, encryption, rate limiting for notebook-cam.

pub mod auth;
pub mod encryption;
pub mod middleware;
pub mod password;
pub mod rate_limit;
pub mod tls;

pub use auth::{
    create_session, generate_token, invalidate_session, is_first_run, reset_password,
    validate_session,
};
pub use encryption::{decrypt_field, encrypt_field, get_encryption_key, hex_decode, hex_encode};
pub use middleware::AuthenticatedUser;
pub use password::{hash_password, verify_password};
pub use rate_limit::check_rate_limit;
pub use tls::{build_tls_config, generate_self_signed_cert};
