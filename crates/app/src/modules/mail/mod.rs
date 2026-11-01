//! The emails the API sends, from background jobs ([spec](../../../../../docs/spec/mail.md)).

mod error;
mod message;
mod service;
mod settings;
mod transport;

pub(crate) use error::MailError;
pub(crate) use message::{Invitation, Message};
pub(crate) use service::Mail;
pub(crate) use settings::Settings;
