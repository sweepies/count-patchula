//! Moving a locked package to a chosen version without touching its declaration.
//! Bundler and Poetry can only move a package as far as its constraint allows,
//! so the constraint is pinned to the target for one resolution. The manifest
//! is then restored and the lock resolved again, which keeps the locked
//! version and writes the declared constraint back into the lock.
use super::*;
use patchula_manager::{Edit, Scanner};
use patchula_versioning::Versioning;

/// Manifests as they were before a transient pin.
pub(super) struct Pinned {
    root: PathBuf,
    originals: BTreeMap<String, String>,
}

impl Pinned {
    /// Pin every lock-only job's package to its target in the `manager`
    /// manifests under `directory`. With no such jobs nothing is written.
    pub(super) fn apply(
        root: &Path,
        directory: &str,
        config: &Config,
        manager: Manager,
        jobs: &[&Job],
    ) -> Result<Self, String> {
        let targets: BTreeMap<_, _> = jobs
            .iter()
            .filter(|j| j.lock_only)
            .filter_map(|j| {
                Some((
                    j.package.as_deref()?,
                    (j.current.as_deref(), j.target.as_deref()?),
                ))
            })
            .collect();
        let mut pinned = Self::none(root);
        if targets.is_empty() {
            return Ok(pinned);
        }
        let files = collect(root)?;
        let extraction = Scanner::new(config)?.extract(&files)?;
        let mut edits = BTreeMap::<&str, Vec<Edit>>::new();
        let mut found = BTreeSet::new();
        for dep in &extraction.dependencies {
            let Some((current, target)) = targets.get(dep.package.as_str()) else {
                continue;
            };
            if dep.manager != manager
                || !under(&dep.file, directory)
                || dep.locked().is_none()
                || dep.resolved.as_deref() != *current
            {
                continue;
            }
            let pin = patchula_lookup::scheme(dep)?
                .new_range(&dep.current, target, patchula_config::VersionPolicy::Exact)
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
                if directory.is_empty() {
                    "the repository root"
                } else {
                    directory
                }
            ));
        }
        for (path, edits) in edits {
            let original = &files[path];
            write(root, path, &patchula_manager::apply(original, edits)?)?;
            pinned.originals.insert(path.into(), original.clone());
        }
        Ok(pinned)
    }
    pub(super) fn none(root: &Path) -> Self {
        Self {
            root: root.into(),
            originals: BTreeMap::new(),
        }
    }
    pub(super) fn is_empty(&self) -> bool {
        self.originals.is_empty()
    }
    /// Put the manifests back as they were.
    pub(super) fn restore(&self) -> Result<(), String> {
        self.originals
            .iter()
            .try_for_each(|(path, content)| write(&self.root, path, content))
    }
}
