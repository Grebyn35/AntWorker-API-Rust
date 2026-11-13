//! A user's identities, the ways their account signs in, as they read them (AUTH-085).

use platform::types::Timestamp;
use serde::Serialize;
use utoipa::ToSchema;

/// The ways an account signs in: its password, and its accounts at providers (AUTH-085).
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Identities {
    /// The account's password identity, or null for an account without a password.
    pub(crate) password: Option<PasswordIdentity>,
    /// The account's provider identities, the earliest linked first.
    pub(crate) providers: Vec<ProviderIdentity>,
}

/// An account's password, as one of its identities. It holds no hash.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct PasswordIdentity {
    /// The identity's ID, which removing it names (AUTH-088).
    pub(crate) id: uuid::Uuid,
    /// When the account got a password.
    pub(crate) created_at: Timestamp,
}

/// An account at a provider through which its user signs in, as one of their identities. It
/// holds no provider's ID for the user.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ProviderIdentity {
    /// The identity's ID, which removing it names (AUTH-088).
    pub(crate) id: uuid::Uuid,
    /// The provider, by its key, as `sign_in_providers` lists it while the deployment
    /// configures it (AUTH-081).
    pub(crate) provider: String,
    /// When it was linked, or when its sign-in registered the account.
    pub(crate) created_at: Timestamp,
}
