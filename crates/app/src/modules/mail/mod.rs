//! The emails the API sends, from background jobs ([spec](../../../../../docs/spec/mail.md)).
//! The transport they go through is `platform`'s `mail` area.

mod error;
mod jobs;
mod model;
mod service;

pub(crate) use error::MailError;
pub(crate) use model::{Invitation, Message};
pub(crate) use service::Mail;
