//! The pinned `mise` runner and its registry snapshot, used by catalog
//! lookup, lockfile updates and the manager. Tool versions come from
//! `mise ls-remote --json` run in a sandbox; the snapshot only maps tool
//! names to registry backends and the GitHub repositories the
//! mise-versions mirror lists.
#![forbid(unsafe_code)]
mod lookup;
mod policy;
mod registry;
mod sandbox;

pub use lookup::{Bootstrap, Lookup, PACKAGE, Query, Release, bootstrap};
pub use policy::check;
pub use registry::{backends, listed_github_repository};
pub use sandbox::sandbox_env;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Refused(String),
    #[error("mise failed: {0}")]
    Failed(patchula_process::Failure),
    #[error("mise gave an unreadable listing for {0}")]
    Unreadable(String),
    #[error("mise lists no versions of `{0}`")]
    Empty(String),
}
