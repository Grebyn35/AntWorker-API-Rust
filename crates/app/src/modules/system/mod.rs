//! What the running deployment offers, and the seed of a development database
//! ([spec](../../../../../docs/spec/system.md)).

mod error;
mod routes;
mod seed;

pub(crate) use error::SystemError;
pub(crate) use routes::{Capabilities, Features, SignInClient, SignInProvider, routes};
pub(crate) use seed::{Mailbox, PASSWORD, Seed, allowed as seed_allowed, seed};
// part: workspace_setup
pub(crate) use seed::SettingUp;
// end part: workspace_setup
