//! Dependency identity: the key recipes every persisted record — plans,
//! dashboard rows, cache bookkeeping — shares, kept together so they change
//! with the identity, not with any one consumer.
use patchula_manager::{Declaration, hash};
use patchula_report::Dependency;

/// The declaration a report dependency describes.
pub(crate) fn declaration<'a>(file: &'a str, dep: &'a Dependency) -> Declaration<'a> {
    Declaration {
        file,
        manager: dep.manager,
        name: &dep.name,
        package: &dep.package,
        source: dep.metadata.as_ref().map(|m| m.source),
        current: dep.current.value.as_deref(),
        digest: dep.current.digest.as_deref(),
        registry: dep.registry.as_deref(),
    }
}
pub fn dependency_key(file: &str, dep: &Dependency) -> String {
    declaration(file, dep).key()
}
pub fn extracted_key(dep: &patchula_manager::Dependency) -> String {
    dep.declaration().key()
}
/// The key of a retry box: one package from one source, whichever files
/// declare it. A lookup is shared by all of them.
pub fn retry_key(manager: &str, package: &str, source: &str) -> String {
    hash(
        serde_json::to_string(&("retry", manager, package, source))
            .expect("retry identity")
            .as_bytes(),
    )
}
pub fn extracted_retry_key(dep: &patchula_manager::Dependency) -> String {
    retry_key(dep.manager.as_str(), &dep.package, dep.source.as_str())
}
pub fn held_target(dependency: &str, version: &str, kind: &str) -> String {
    hash(
        serde_json::to_string(&(dependency, version, kind))
            .expect("held identity")
            .as_bytes(),
    )
}
pub fn held_key(file: &str, dep: &Dependency, version: &str, kind: &str) -> String {
    held_target(&dependency_key(file, dep), version, kind)
}
