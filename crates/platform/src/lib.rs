//! Shared mechanisms for every module of the API. `platform` holds no product rules (ARCH-02),
//! and never depends on the `app` crate (ARCH-01).

#![deny(missing_docs)]

pub mod archive;
pub mod authn;
pub mod authz;
pub mod blocking;
pub mod clock;
pub mod config;
pub mod db;
pub mod encryption;
pub mod errors;
pub mod events;
pub mod hashing;
pub mod http;
pub mod idempotency;
pub mod jobs;
pub mod lifecycle;
pub mod lists;
pub mod mail;
pub mod personal_data;
pub mod random;
pub mod ranges;
pub mod rate_limit;
// part: files
pub mod scanner;
pub mod signed_urls;
pub mod storage;
// end part: files
pub mod telemetry;
mod tls;
pub mod types;
