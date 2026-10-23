//! Shared mechanisms for every module of the API. `platform` holds no product rules (ARCH-02),
//! and never depends on the `app` crate (ARCH-01).

#![deny(missing_docs)]

pub mod blocking;
pub mod clock;
pub mod config;
pub mod db;
pub mod errors;
pub mod http;
pub mod lifecycle;
pub mod telemetry;
pub mod types;
