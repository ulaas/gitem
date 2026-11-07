//! gitem-core: everything that is not UI.
//!
//! All repository access goes through the `git` executable (see [`runner`]);
//! there is no libgit2. Read queries live in [`repo`], write operations are
//! described as data in [`ops`] so the UI can log, confirm and replay them.

#[cfg(test)]
gitem_testenv::isolate!();

pub mod agents;
pub mod ai;
pub mod autorebase;
pub mod cred;
pub mod diff;
pub mod editors;
pub mod forge;
pub mod graph;
pub mod mcp;
pub mod merge;
pub mod model;
pub mod ops;
pub mod parse;
pub mod patches;
pub mod rebase;
pub mod repo;
pub mod runner;
pub mod settings;
pub mod treemap;
pub mod trust;
pub mod undo;
pub mod util;

pub use model::*;
pub use repo::Repo;
pub use runner::{GitExe, GitOutput};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Git(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}
pub type Result<T> = std::result::Result<T, Error>;
