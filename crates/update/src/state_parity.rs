//! `tests/state-versions.tsv` guards every code-versioned state this
//! crate registers: its `VERSION` names the code that derives the value,
//! the row hashes the source files, trees and locked dependency versions
//! that derivation reads, and a change to any of them fails here until
//! the version is bumped and the row updated in the same commit.
use crate::cleanup::NotOurs;
use patchula_cache::State;

/// Every `CODE_VERSIONED` state the crate registers.
fn states() -> std::collections::BTreeMap<&'static str, u32> {
    [(NotOurs::NAMESPACE, NotOurs::VERSION)]
        .into_iter()
        .collect()
}

#[test]
fn every_code_versioned_state_versions_its_source() {
    patchula_testkit::state::check("update", &states());
}
