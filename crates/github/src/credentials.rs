//! Refreshable credentials. Implementations must never log or persist tokens.
use crate::Result;
use std::{future::Future, pin::Pin};

pub type Token<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

pub trait Credentials: Send + Sync {
    /// Stable, non-secret authorization namespace, independent of token rotation.
    fn scope(&self) -> &str;
    fn token(&self) -> Token<'_>;
    /// A token narrowed to `repository` (`owner/name`) for a lockfile
    /// tool. Only a GitHub App installation can mint one, so every other
    /// credential answers `None` and the caller takes `token()`'s.
    fn scoped_token(&self, _repository: &str) -> Option<Token<'_>> {
        None
    }
}
