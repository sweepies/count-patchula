//! Refreshable credentials. Implementations must never log or persist tokens.
use crate::Result;
use std::{future::Future, pin::Pin};

pub type Token<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
/// A repository's numeric id, as a scoped mint names it.
pub type RepositoryId<'a> = Pin<Box<dyn Future<Output = Result<u64>> + Send + 'a>>;

pub trait Credentials: Send + Sync {
    /// Stable, non-secret authorization namespace, independent of token rotation.
    fn scope(&self) -> &str;
    fn token(&self) -> Token<'_>;
    /// A token narrowed to `repository` (`owner/name`) and `permissions`,
    /// for a tool the Count runs. Only a GitHub App installation can mint
    /// one, so every other credential answers `None` and the caller decides
    /// what to do without it.
    fn scoped_token(&self, _repository: &str, _permissions: Permissions) -> Option<Token<'_>> {
        None
    }
    /// Whether `scoped_token` answers `Some`.
    fn mints_scoped_tokens(&self) -> bool {
        false
    }
    /// The numeric id of the repository `scoped_token` mints for under
    /// this name; `None` exactly when `scoped_token` is. A rename keeps the
    /// id, and a new repository under an old name gets another.
    fn repository_id(&self, _repository: &str) -> Option<RepositoryId<'_>> {
        None
    }
}

/// One exact `permissions` object a repository-scoped mint requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permissions {
    /// `{"metadata": "read"}`: the repository's metadata. The token reads
    /// public repositories as any authenticated caller does.
    MetadataRead,
    /// `{"contents": "read", "metadata": "read"}`: also the repository's
    /// code, releases and tags, which a version listing of a tool released
    /// from the repository itself reads.
    ContentsRead,
}
