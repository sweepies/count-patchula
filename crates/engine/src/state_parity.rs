//! `tests/state-versions.tsv` guards every code-versioned state this
//! crate registers: its `VERSION` names the code that derives the value,
//! the row hashes the source files and trees that derivation reads, and a
//! change to any of them fails here until the version is bumped and the
//! row updated in the same commit. The locked versions of the row's listed
//! crates key the cache entry instead, so a bump of one needs neither.
use super::*;

/// Every `CODE_VERSIONED` state the crate registers.
fn states() -> std::collections::BTreeMap<&'static str, u32> {
    [
        (RepoConfig::NAMESPACE, RepoConfig::VERSION),
        (CachedExtraction::NAMESPACE, CachedExtraction::VERSION),
        (FileExtraction::NAMESPACE, FileExtraction::VERSION),
        (EvaluatedConfig::NAMESPACE, EvaluatedConfig::VERSION),
        (catalogs::Saved::NAMESPACE, catalogs::Saved::VERSION),
        (catalogs::Controls::NAMESPACE, catalogs::Controls::VERSION),
    ]
    .into_iter()
    .collect()
}

#[test]
fn every_code_versioned_state_versions_its_source() {
    patchula_testkit::state::check("engine", &states());
}
