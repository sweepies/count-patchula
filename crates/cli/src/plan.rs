//! Planning the writes a set of updates makes to a working tree,
//! shared by `patchula apply` and the TUI's apply path: what changes,
//! what cannot be written in place, and writing it safely.
use crate::updates::Update;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Where selected updates can be written. Only local directories are editable.
pub struct Local {
    pub dir: PathBuf,
    pub config: patchula_config::Config,
}

/// What writing a set of updates into a working tree changes.
pub struct Plan {
    /// Each changed file's text before and after.
    pub files: BTreeMap<String, (String, String)>,
    /// Positions, in the updates planned, of those the new files carry.
    pub applied: Vec<usize>,
    /// Positions of the updates that cannot be written in place, with why.
    pub skipped: Vec<(usize, String)>,
}
/// A write that stopped: the files it had already written, and the one it
/// was writing when it failed, which may be left partly written.
#[derive(Debug)]
pub struct WriteError {
    pub written: Vec<String>,
    pub partial: Option<String>,
    pub error: String,
}
impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)?;
        if let Some(partial) = &self.partial {
            write!(f, "; {partial} may be left partly written")?;
        }
        match self.written.len() {
            0 if self.partial.is_none() => write!(f, "; nothing was written"),
            0 => Ok(()),
            1 => write!(f, "; only {} was written", self.written[0]),
            _ => write!(f, "; only {} were written", self.written.join(", ")),
        }
    }
}
impl Plan {
    /// Write every file in place. Each is first checked by opening it for
    /// writing, so a missing file, a permission, or two names for the same
    /// file stops the write with nothing written. On Unix the sameness is
    /// the device and inode, so a symlink or a hard link is caught; on other
    /// platforms only resolved paths count, and the text check below is what
    /// still refuses a second write to one file. Right before a file is
    /// truncated its text is compared with the text it was planned from, so
    /// an edit made since planning is refused rather than overwritten.
    /// Files are then written one at a time, each open only while it is
    /// written, so a plan may touch more files than the process may hold
    /// open. Writing in place keeps each file's owner, mode, links and
    /// attributes, as an editor saving it would. Only a failure while writing,
    /// such as a full disk, can stop partway, and the error names what was
    /// written.
    pub fn write(&self, dir: &Path) -> Result<(), WriteError> {
        use std::io::{Read as _, Seek as _, Write as _};
        let mut checked = Vec::new();
        let mut identities = BTreeSet::new();
        for (file, (old, content)) in &self.files {
            let check = (|| {
                // A symlinked file is written through its link.
                let target = std::fs::canonicalize(dir.join(file))?;
                // Only a regular file: opening a pipe or device to write may block
                // or reach something that is not the manifest.
                if !std::fs::metadata(&target)?.is_file() {
                    return Err(std::io::Error::other("not a regular file"));
                }
                let handle = std::fs::OpenOptions::new().write(true).open(&target)?;
                let id = identity(&target, &handle.metadata()?);
                if !identities.insert(id.clone()) {
                    return Err(std::io::Error::other(
                        "another file in this write is the same file",
                    ));
                }
                Ok((target, id))
            })();
            match check {
                Ok((target, id)) => checked.push((file, target, id, old, content)),
                Err(error) => {
                    return Err(WriteError {
                        written: Vec::new(),
                        partial: None,
                        error: format!("{file}: {error}"),
                    });
                }
            }
        }
        let mut written = Vec::new();
        for (file, target, id, old, content) in checked {
            let mut opened = None;
            let result = std::fs::metadata(&target)
                .and_then(|metadata| {
                    if metadata.is_file() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other("not a regular file"))
                    }
                })
                .and_then(|()| {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&target)
                })
                .and_then(|mut handle| {
                    // The file checked, not one put in its place since.
                    if identity(&target, &handle.metadata()?) != id {
                        return Err(std::io::Error::other(
                            "the file changed since the Count checked it",
                        ));
                    }
                    // The same inode can still hold an edit made after
                    // planning; it must not be written over.
                    let mut current = String::new();
                    handle.read_to_string(&mut current)?;
                    if current != *old {
                        return Err(std::io::Error::other(
                            "the file changed since the Count planned it",
                        ));
                    }
                    let handle = opened.insert(handle);
                    handle.set_len(0)?;
                    handle.rewind()?;
                    handle.write_all(content.as_bytes())?;
                    handle.flush()
                });
            if let Err(error) = result {
                return Err(WriteError {
                    written,
                    // Opened means it may have been cut short.
                    partial: opened.is_some().then(|| file.clone()),
                    error: format!("{file}: {error}"),
                });
            }
            written.push(file.clone());
        }
        Ok(())
    }
}

/// What makes two paths the same file: the device and inode where there are
/// such, else the resolved path. A filesystem that makes up inode numbers
/// (some FUSE and network ones) can make distinct files look the same, which
/// refuses the write rather than risking it.
#[cfg(unix)]
fn identity(_: &Path, metadata: &std::fs::Metadata) -> (u64, u64, PathBuf) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino(), PathBuf::new())
}
#[cfg(not(unix))]
fn identity(target: &Path, _: &std::fs::Metadata) -> (u64, u64, PathBuf) {
    (0, 0, target.to_owned())
}

/// Plan the edits for these updates in the working tree, the way the Count
/// edits a branch. An update that cannot be written in place is skipped with
/// its reason, and the first update to claim a declaration keeps it.
pub fn plan(local: &Local, updates: &[&Update]) -> Result<Plan, String> {
    let scanner = patchula_manager::Scanner::new(&local.config)?;
    // Declarations are found again from the tree as it is now, every file the
    // lookup read included, so an edit can only land where the same
    // dependency still is.
    // The view's reads are synchronous filesystem reads, so its future
    // completes in one poll and needs no runtime.
    let view = futures_executor::block_on(patchula_engine::repository::local_view(
        &local.dir,
        &scanner,
        &local.config,
    ))
    .map_err(|e| format!("{e}"))?;
    let files = view.files;
    let dependencies = scanner.extract(&files)?.dependencies;
    let mut skipped = Vec::new();
    let mut claimed = BTreeMap::<(&str, usize), &Update>::new();
    let mut edits = BTreeMap::<String, Vec<patchula_manager::Edit>>::new();
    let mut applied = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        if update.transitive {
            skipped.push((
                index,
                format!(
                    "{} is only in {}; update it with your package manager",
                    update.name, update.file
                ),
            ));
            continue;
        }
        // The update was reported against every occurrence of this
        // declaration; the tree must still hold exactly those, or the edit
        // would land on only some of them.
        let occurrence = |d: &&patchula_manager::Dependency| {
            d.file == update.file
                && d.name == update.name
                && d.package == update.package
                && d.current == update.current
        };
        let found: BTreeSet<usize> = dependencies
            .iter()
            .filter(|d| occurrence(d))
            .map(|d| d.location.start)
            .collect();
        let reported: BTreeSet<usize> = update.starts.iter().copied().collect();
        if found.is_empty() || found != reported {
            skipped.push((
                index,
                format!(
                    "{} in {} has changed since the lookup; run it again",
                    update.name, update.file
                ),
            ));
            continue;
        }
        let targets: Vec<_> = dependencies.iter().filter(|d| occurrence(d)).collect();
        if let Some(other) = targets
            .iter()
            .find_map(|d| claimed.get(&(d.file.as_str(), d.location.start)))
        {
            skipped.push((
                index,
                format!(
                    "{} in {} already takes {} {}",
                    update.name,
                    update.file,
                    other.kind,
                    other.target()
                ),
            ));
            continue;
        }
        let value = update.value.as_deref().unwrap_or(&update.current);
        let text = match files[&update.file].read() {
            Ok(text) => text,
            Err(unread) => {
                skipped.push((index, format!("{}: {}", update.file, unread.why())));
                continue;
            }
        };
        let mut found = Vec::new();
        let mut failed = None;
        for dep in &targets {
            match patchula_manager::edits(dep, text, value, update.digest.as_deref()) {
                Ok(more) => found.extend(more),
                Err(error) => failed = Some(format!("{}: {error}", update.file)),
            }
        }
        if let Some(reason) = failed {
            skipped.push((index, reason));
            continue;
        }
        if found
            .iter()
            .all(|e| text.get(e.start..e.end) == Some(e.replacement.as_str()))
        {
            skipped.push((
                index,
                format!(
                    "{} in {} already allows {}; only its lockfile changes, so update it with your package manager",
                    update.name,
                    update.file,
                    update.target()
                ),
            ));
            continue;
        }
        for dep in targets {
            claimed.insert((&dep.file, dep.location.start), update);
        }
        edits.entry(update.file.clone()).or_default().extend(found);
        applied.push(index);
    }
    let mut changed = BTreeMap::new();
    for (file, edits) in edits {
        let old = files[&file]
            .read()
            .map_err(|unread| format!("{file}: {}", unread.why()))?;
        let new = patchula_manager::apply(old, edits)?;
        if new != old {
            changed.insert(file, (old.to_owned(), new));
        }
    }
    skipped.sort();
    Ok(Plan {
        files: changed,
        applied,
        skipped,
    })
}

/// What writing a plan does with its skips: the applicable subset is written
/// and every skip is reported with its reason. This is `patchula apply`'s
/// behaviour, which the TUI shares — a plan never writes nothing just
/// because some of it cannot land.
pub fn outcomes<'a>(
    plan: &'a Plan,
    updates: &'a [&'a Update],
) -> (Vec<&'a Update>, Vec<(&'a Update, &'a str)>) {
    (
        plan.applied.iter().map(|&i| updates[i]).collect(),
        plan.skipped
            .iter()
            .map(|(i, reason)| (updates[*i], reason.as_str()))
            .collect(),
    )
}

/// The local directory's config, as the lookup used it.
pub fn local(dir: &Path, config: Option<patchula_config::Config>) -> Result<Local, String> {
    let config = match config {
        Some(config) => config,
        None => patchula_config::load(dir).map_err(|e| format!("{e:?}"))?,
    };
    Ok(Local {
        dir: dir.to_owned(),
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::updates;
    use patchula_report::Report;
    use patchula_words::Update as UpdateKind;

    fn report(updates: &[(&str, &str, &str, &str)]) -> Report {
        let dependencies: Vec<_> = updates
            .iter()
            .map(|(name, current, target, kind)| {
                serde_json::json!({
                    "manager": "npm", "name": name, "package": name,
                    "current": {"value": current, "version": current},
                    "updates": [{"newValue": target, "newVersion": target, "updateType": kind}],
                    "metadata": {"source": "npm", "kinds": ["production"],
                        "locations": [{"start": 0, "end": 0, "line": 1, "column": 1, "pointer": "/"}]}
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 6,
            "repositories": [{"repository": "local/app", "status": "complete",
                "packageFiles": [{"path": "package.json", "dependencies": dependencies}]}]
        }))
        .unwrap()
    }

    #[test]
    fn plan_skips_what_cannot_be_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        // hono was edited after the lookup, at the same place.
        let manifest = "{\n  \"dependencies\": {\n    \"zod\": \"3.22.0\",\n    \"hono\": \"4.7.0\",\n    \"ms\": \"^2.1.0\"\n  }\n}\n";
        std::fs::write(dir.path().join("package.json"), manifest).unwrap();
        std::fs::write(
            dir.path().join("patchula.pkl"),
            "amends \"PatchulaConfig.pkl\"\n",
        )
        .unwrap();
        let mut report = report(&[
            ("zod", "3.22.0", "4.1.0", "major"),
            ("hono", "4.6.0", "4.7.2", "minor"),
            ("lodash", "4.17.20", "4.17.21", "patch"),
            ("ms", "^2.1.0", "^2.1.0", "patch"),
        ]);
        let deps = &mut report.repositories[0].package_files[0].dependencies;
        for (dep, value) in deps.iter_mut().zip(["3.22.0", "4.7.0", "", "^2.1.0"]) {
            if let Some(start) = manifest.find(&format!("\"{value}\"")) {
                dep.metadata.as_mut().unwrap().locations[0].start = start;
            }
        }
        deps[3].updates[0].new_version = Some("2.1.3".into());
        let mut minor = deps[0].updates[0].clone();
        minor.new_value = Some("3.25.0".into());
        minor.update_type = UpdateKind::Minor;
        deps[0].updates.push(minor);
        let mut updates = updates(&report);
        let lodash = updates.iter_mut().find(|u| u.name == "lodash").unwrap();
        lodash.transitive = true;
        lodash.file = "package-lock.json".into();
        let local = local(dir.path(), None).unwrap();
        let plan = plan(&local, &updates.iter().collect::<Vec<_>>()).unwrap();
        let targets: Vec<_> = plan.applied.iter().map(|&i| updates[i].target()).collect();
        assert_eq!(targets, ["4.1.0"]);
        let reasons: Vec<_> = plan.skipped.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "zod in package.json already takes major 4.1.0",
                "hono in package.json has changed since the lookup; run it again",
                "lodash is only in package-lock.json; update it with your package manager",
                "ms in package.json already allows ^2.1.0; only its lockfile changes, so update it with your package manager",
            ]
        );
        assert_eq!(
            plan.files["package.json"].1,
            manifest.replace("3.22.0", "4.1.0")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("package.json")).unwrap(),
            manifest,
            "planning writes nothing"
        );
    }
}
