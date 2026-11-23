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
mod targets;

pub use lookup::{Bootstrap, Lookup, PACKAGE, Query, Release, bootstrap};
pub use policy::{bracketed, check, options, split};
pub use registry::{backends, github_sources, listed_github_repository};
pub use sandbox::{sandbox_env, without_plugins};

/// Refuses a tool argument the Count does not hand mise: a name or options
/// `check` refuses, or options that point mise at a cloud instance-metadata
/// endpoint. Every tool argument reaches mise through this, whether mise
/// lists it or locks it.
pub async fn admit(tool: &str) -> Result<(), Error> {
    check(tool)?;
    targets::check(tool).await
}

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
