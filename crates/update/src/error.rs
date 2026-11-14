//! The update crate's errors: typed sources for the work it asks GitHub and
//! the cache to do, the expected races a rerun resolves, and the refusals the
//! Count words himself. Rendered for the report only through `Display` —
//! `patchula_cache::Error::Database` deliberately keeps SQLite's text out of
//! diagnostics, so nothing here is ever formatted `{:?}` or source-chained.
use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Github(#[from] patchula_github::Error),
    #[error(transparent)]
    Cache(#[from] patchula_cache::Error),
    /// The repository changed under the run; nothing was written against the
    /// stale view.
    #[error("{0}")]
    Raced(&'static str),
    /// The Count will not do this as things stand, in his own words.
    #[error("{0}")]
    Refused(Cow<'static, str>),
}
/// The pure planner's closed error set: it names no GitHub or cache error,
/// and every refusal it can raise is a variant here, its current text kept
/// verbatim.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("prs.branch produces an invalid Git branch name")]
    BranchName,
    #[error("branch name collision after manager disambiguation; set explicit groups")]
    BranchCollision,
    #[error("missing extracted file {}", .0)]
    MissingExtractedFile(String),
    #[error("missing extraction commit")]
    MissingExtractionCommit,
    #[error("{} is reserved for the batch PR; rename the update group", .0)]
    BatchReserved(String),
    /// The evaluator's refusal, in lookup's own words.
    #[error("{0}")]
    Lookup(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_is_frozen() {
        let cases: [(Error, &str); 4] = [
            (
                Error::Github(patchula_github::Error::Status(418)),
                "GitHub returned HTTP 418",
            ),
            (
                Error::Cache(patchula_cache::Error::Io(std::io::Error::other("x"))),
                "cache I/O failed",
            ),
            (
                Error::Raced("default branch changed since extraction; rerun"),
                "default branch changed since extraction; rerun",
            ),
            (Error::Refused("not like this".into()), "not like this"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn plan_error_text_is_frozen() {
        let cases: [(PlanError, &str); 6] = [
            (
                PlanError::BranchName,
                "prs.branch produces an invalid Git branch name",
            ),
            (
                PlanError::BranchCollision,
                "branch name collision after manager disambiguation; set explicit groups",
            ),
            (
                PlanError::MissingExtractedFile("a/Cargo.toml".into()),
                "missing extracted file a/Cargo.toml",
            ),
            (
                PlanError::MissingExtractionCommit,
                "missing extraction commit",
            ),
            (
                PlanError::BatchReserved("patchula/batch".into()),
                "patchula/batch is reserved for the batch PR; rename the update group",
            ),
            (PlanError::Lookup("lookup said no".into()), "lookup said no"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }
}
