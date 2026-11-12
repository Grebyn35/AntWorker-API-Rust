//! What the running deployment offers, and the seed of a development database
//! ([spec](../../../../../docs/spec/system.md)).

mod error;
mod routes;
mod seed;

pub(crate) use error::SystemError;
pub(crate) use routes::{Capabilities, Features, SignInClient, SignInProvider, routes};
pub(crate) use seed::{Mailbox, PASSWORD, allowed as seed_allowed, seed};
