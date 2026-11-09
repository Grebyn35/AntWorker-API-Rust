//! The API's feature modules and its composition root. Only the composition entry points are
//! public, so every module item is crate-visible (CODE-15).

mod composition;
mod modules;

pub use composition::{
    Config, MIGRATIONS, Resources, SeedError, SeedReport, Services, compose, openapi, pipeline,
    seed, seed_allowed,
};
#[cfg(feature = "test-support")]
pub use composition::{permissions, personal_data_columns};
