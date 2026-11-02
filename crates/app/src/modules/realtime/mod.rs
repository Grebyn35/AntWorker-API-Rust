//! The stream of server-sent events ([spec](../../../../../docs/spec/realtime.md)).

mod error;
mod routes;

pub(crate) use routes::{Realtime, routes};
