//! The job of `auth` (OPS-09), owned by `auth`:
//! - `auth.send_verification_code`, enqueued by a registration with email on and by asking for a
//!   code, for the account's user, issues a code that proves the account's email address, in
//!   place of the one before it, touching `verification_codes` and `audit_entries`, and sends it
//!   to the address at once, so the code is kept nowhere but the email (AUTH-030, SEC-03). A
//!   second run, such as after a replica stopped once the email was sent, issues a new code and
//!   sends it too, which replaces the first: the account receives two emails, and only the
//!   second's code works, which its owner recognizes by the email's text, since each says that a
//!   newer code replaces it (OPS-09, D128).

use std::time::Duration;

use platform::jobs::{Duplicate, Job, JobContext, Jobs};
use serde::{Deserialize, Serialize};

use super::{model::Purpose, service::Auth};

/// The payload of `auth.send_verification_code`: what the code proves.
#[derive(Serialize, Deserialize)]
pub(super) struct SendCode {
    pub(super) purpose: Purpose,
}

impl Job for SendCode {
    const KIND: &'static str = "auth.send_verification_code";
    /// As many as `platform.send_email`'s, so a mail server down for a few hours delays a code
    /// rather than losing it (PLATFORM-021).
    const MOST_ATTEMPTS: u32 = 12;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
}

/// Registers the job of `auth` on `auth`.
pub(crate) fn register(auth: &std::sync::Arc<Auth>, jobs: &mut Jobs) -> Result<(), Duplicate> {
    let auth = auth.clone();
    jobs.queued(move |context: JobContext, job: SendCode| {
        let auth = auth.clone();
        async move { auth.issue_code(&context, job).await }
    })
}
