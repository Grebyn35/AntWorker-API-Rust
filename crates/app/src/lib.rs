//! The API's feature modules and its composition root. Only the composition entry points are
//! public, so every module item is crate-visible (CODE-15).

mod composition;
mod modules;

pub use composition::{
    Config, MIGRATIONS, RemovalRefused, Resources, SeedError, SeedReport, Services, compose,
    openapi, pipeline, remove_second_factor, seed, seed_allowed,
};
// part: workspace_setup
pub use composition::{SetupRefused, setup_workspace};
// end part: workspace_setup
#[cfg(feature = "test-support")]
pub use composition::{permissions, personal_data_columns};
