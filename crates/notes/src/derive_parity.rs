//! Byte-for-byte parity between a buffered body and one arriving in chunks:
//! every derive the crate registers reads the same document either way, over
//! every body the fixtures serve.
use patchula_cache::State;
use patchula_testkit::derived::{Registered, erase, streams_the_same};

/// Every `Derived` the crate registers.
fn derives() -> Vec<Registered> {
    vec![
        erase(crate::mirror::NOTES_MISE_VERSIONS),
        erase(crate::source::NOTES_SOURCE_NPM),
        erase(crate::source::NOTES_SOURCE_CRATES),
        erase(crate::source::NOTES_SOURCE_PYPI),
        erase(crate::forge::NOTES_GITLAB_RELEASES),
        erase(crate::forge::NOTES_GITEA_RELEASES),
        erase(crate::forge::NOTES_FORGE_TAGS),
        erase(crate::forge::NOTES_FORGE_FILES),
        erase(crate::forge::NOTES_FORGE_CHANGELOG),
        erase(crate::changelog::NOTES_CHANGELOG_DIRECTORY),
        erase(crate::changelog::PARSED_CHANGELOG),
        erase(crate::adapters::NOTES_ADAPTER_LISTING),
        erase(crate::adapters::PARSED_POSTGRES_SGML),
        erase(crate::adapters::PARSED_GO_HISTORY),
    ]
}

/// Every fixture body derives the same bytes streamed as buffered — and
/// agrees to fail — however its chunks are sized.
#[tokio::test]
async fn every_derive_reads_a_streamed_body_the_same() {
    streams_the_same(&derives()).await;
}

/// A derived body is versioned by its compaction code, not the release:
/// `tests/derived-versions.tsv` hashes the files that produce its bytes,
/// and every `Derived::new` in the crate
/// must be registered in `derives()`. A change to one must bump its
/// `Derived`'s version and update the row in the same commit; this failure
/// names the new hash.
#[test]
fn every_derive_versions_its_compaction_source() {
    let registered: std::collections::BTreeMap<&'static str, u32> = derives()
        .into_iter()
        .map(|(name, version, _)| (name, version))
        .collect();
    patchula_testkit::derived::check("notes", &registered);
}

/// The crate's code-versioned states are pinned the same way:
/// `tests/state-versions.tsv` hashes the parsing sources the stored
/// sections are read through, and a change to one must bump the state's
/// `VERSION` and update the row in the same commit.
#[test]
fn every_code_versioned_state_versions_its_source() {
    let registered: std::collections::BTreeMap<&'static str, u32> = [
        (
            crate::ParsedChangelog::NAMESPACE,
            crate::ParsedChangelog::VERSION,
        ),
        (
            crate::adapters::AdapterSections::NAMESPACE,
            crate::adapters::AdapterSections::VERSION,
        ),
        (
            crate::releases::SavedRelease::NAMESPACE,
            crate::releases::SavedRelease::VERSION,
        ),
    ]
    .into_iter()
    .collect();
    patchula_testkit::state::check("notes", &registered);
}
