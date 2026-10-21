//! The API's feature modules and its composition root. Only the composition entry point is
//! public, so every module item is crate-visible (CODE-14).

mod composition;

pub use composition::router;
