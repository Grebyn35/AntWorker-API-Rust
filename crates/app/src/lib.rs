//! The API's feature modules and its composition root. Only the composition entry points are
//! public, so every module item is crate-visible (CODE-15).

mod composition;
mod modules;

pub use composition::{
    Config, Done, EndingRefused, MIGRATIONS, Operation, Refused, RemovalRefused, Resources,
    SeedError, SeedReport, Services, compose, openapi, pipeline, seed, seed_allowed,
};
// part: files
pub use composition::{RestoreFiles, RestoreRefused};
// end part: files
// part: superadmins
pub use composition::{SuperadminCommand, SuperadminRefused};
// end part: superadmins
// part: workspace_setup
pub use composition::SetupRefused;
// end part: workspace_setup
#[cfg(feature = "test-support")]
pub use composition::{permissions, personal_data_columns};
