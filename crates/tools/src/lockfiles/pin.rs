//! Moving a locked package to a chosen version without touching its declaration.
//! Bundler and Poetry can only move a package as far as its constraint allows,
//! so the constraint is pinned to the target for one resolution. The manifest
//! is then restored and the lock resolved again, which keeps the locked
//! version and writes the declared constraint back into the lock.
use super::*;
use patchula_manager::{Edit, Scanner};

/// Pin every exact-version job's package to its target in the `manager`
/// manifests under `directory`, answering the guard that puts them back.
/// With no such jobs nothing is written.
pub(super) fn apply(
    root: &Path,
    directory: &str,
    config: &Config,
    manager: Manager,
    jobs: &[&Job],
) -> Result<context::Replaced, String> {
    let targets: BTreeMap<_, _> = jobs
        .iter()
        .filter(|j| j.exact_version)
        .filter_map(|j| Some((j.package()?, (j.current(), j.target()?))))
        .collect();
    let mut pinned = context::Replaced::default();
    if targets.is_empty() {
        return Ok(pinned);
    }
    let files = collect(root)?;
    let extraction = Scanner::new(config)?.extract(&patchula_manager::all_read(&files))?;
    let mut edits = BTreeMap::<&str, Vec<Edit>>::new();
    let mut found = BTreeSet::new();
    for dep in &extraction.dependencies {
        let Some((current, target)) = targets.get(dep.package.as_str()) else {
            continue;
        };
        if dep.manager != manager
            || !under(&dep.file, directory)
            || dep
                .proposing()
                .lock
                .locked(dep.resolved.as_deref())
                .is_none()
            || dep.resolved.as_deref() != *current
        {
            continue;
        }
        let pin = patchula_versioning::Ecosystem::of(dep.scheme, dep.source)
            .new_range(&dep.current, target, patchula_words::VersionPolicy::Exact)
            .map_err(|e| e.to_string())?;
        edits
            .entry(&dep.file)
            .or_default()
            .extend(patchula_manager::edits(dep, &files[&dep.file], &pin, None)?);
        found.insert(dep.package.as_str());
    }
    if let Some(missing) = targets.keys().find(|package| !found.contains(**package)) {
        return Err(format!(
            "cannot pin {missing} to its target: no declaration of it found in {}",
            context::describe_dir(directory)
        ));
    }
    for (path, edits) in edits {
        let original = &files[path];
        let content = patchula_manager::apply(original, edits)?;
        pinned.replace(&root.join(path), original.clone(), &content)?;
    }
    Ok(pinned)
}
