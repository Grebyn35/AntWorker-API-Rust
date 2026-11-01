//! The API's feature modules and its composition root. Only the composition entry points are
//! public, so every module item is crate-visible (CODE-14).

mod composition;
mod modules;

pub use composition::{Config, MIGRATIONS, Services, compose, openapi, pipeline};
