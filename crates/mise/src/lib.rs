//! Looks up tool versions by running the pinned `mise ls-remote --json` in a
//! sandbox, so every backend mise supports works without the Count reading
//! mise's registry or backend rules.
#![forbid(unsafe_code)]
mod lookup;
mod policy;
mod registry;
mod sandbox;

pub use lookup::{Lookup, PACKAGE, Query, Release};
pub use policy::check;
pub use registry::listed_github_repository;
pub use sandbox::sandbox_env;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Refused(String),
    #[error("mise failed: {0}")]
    Failed(String),
    #[error("mise gave an unreadable listing for {0}")]
    Unreadable(String),
    #[error("mise lists no versions of `{0}`")]
    Empty(String),
    #[error(
        "mise lists no release versions of `{tool}`, only `{sample}`: it installs from a Git ref"
    )]
    NoReleases { tool: String, sample: String },
}
