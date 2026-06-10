#![cfg_attr(test, deny(warnings))]

/// Stream pipeline: capture -> encode -> distribute
///
/// Central hub connecting media sources to outputs with fan-out,
/// buffer management, and resource control.
pub mod buffer;
pub mod capture_source;
pub mod hub;
pub mod mibee;
pub mod output;
pub mod resource;
pub mod source;
