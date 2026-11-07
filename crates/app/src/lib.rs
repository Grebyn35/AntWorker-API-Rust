//! The API's feature modules and its composition root. Only the composition entry points are
//! public, so every module item is crate-visible (CODE-15).

mod composition;
mod modules;

pub use composition::{
    Config, FilesRestored, MIGRATIONS, Resources, RestoreFiles, SeedError, SeedReport, Services,
    compose, openapi, permissions, pipeline, restore_files, seed, seed_allowed,
};
