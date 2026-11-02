//! What the running deployment offers, and the seed of a development database
//! ([spec](../../../../../docs/spec/system.md)).

mod routes;
mod seed;

pub(crate) use routes::{Features, routes};
pub(crate) use seed::{PASSWORD, SeedError, allowed as seed_allowed, seed};
