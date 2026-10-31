//! Python lockfiles: `uv lock` for `uv.lock` and `poetry lock` for `poetry.lock`.
use super::{pin::Pinned, *};

/// Lockfile names the Python ecosystem regenerates in place.
pub(super) const LOCKS: [&str; 2] = ["uv.lock", "poetry.lock"];

/// Lockfiles of other Python tools, which nothing here regenerates.
pub(super) const UNSUPPORTED_LOCKS: [&str; 3] = ["pdm.lock", "pylock.toml", "Pipfile.lock"];

/// The interpreter both tools resolve against: the nearest `.python-version`,
/// otherwise the repository mise pin or the runner default.
fn python(
    baseline: &BTreeMap<String, Blob>,
    directory: &str,
    versions: &BTreeMap<String, String>,
) -> Result<String, String> {
    let pinned = baseline
        .iter()
        .filter(|(path, _)| {
            path.rsplit('/').next() == Some(".python-version")
                && (directory == self::directory(path) || under(directory, self::directory(path)))
        })
        .max_by_key(|(path, _)| path.len())
        .and_then(|(_, blob)| blob.content.split_whitespace().next().map(str::to_owned))
        .filter(|v| {
            v.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        });
    pinned
        .or_else(|| versions.get("python").cloned())
        .ok_or_else(|| "missing python version".into())
}

impl Runner {
    pub(super) async fn python_locks(
        &self,
        context: node::NodeContext<'_>,
        jobs: &[&Job],
    ) -> Result<(), String> {
        let node::NodeContext {
            driver,
            root,
            baseline,
            directory,
            config,
            versions,
            maintenance,
        } = context;
        let cwd = root.join(directory);
        let python = python(baseline, directory, versions)?;
        for leaf in LOCKS {
            if !cwd.join(leaf).is_file() {
                continue;
            }
            let packages: BTreeSet<_> = jobs.iter().filter_map(|j| j.package.clone()).collect();
            let invocation = if leaf == "uv.lock" {
                let mut args = vec![
                    "lock".to_string(),
                    "--directory".into(),
                    cwd.to_string_lossy().into_owned(),
                ];
                if maintenance {
                    args.push("--upgrade".into());
                }
                // A declaration left as it is gets the exact version, so the
                // lock cannot move past it.
                for job in jobs.iter().filter(|j| j.package.is_some()) {
                    let package = job.package.as_deref().unwrap_or_default();
                    args.extend([
                        "--upgrade-package".into(),
                        match (job.lock_only, &job.target) {
                            (true, Some(target)) => format!("{package}=={target}"),
                            _ => package.into(),
                        },
                    ]);
                }
                Invocation {
                    tools: vec![
                        ("uv".into(), node::version(versions, "uv")?),
                        ("python".into(), python.clone()),
                    ],
                    bin: "uv".into(),
                    cwd: None,
                    // Resolve with the mise interpreter; never download another.
                    env: vec![
                        ("UV_PYTHON_DOWNLOADS".into(), "never".into()),
                        ("UV_NO_PROGRESS".into(), "1".into()),
                    ],
                    args,
                }
            } else {
                // `poetry lock` keeps existing pins and re-resolves changed
                // constraints; `--regenerate` refreshes everything within them.
                let mut args = vec![
                    "lock".to_string(),
                    "--no-interaction".into(),
                    "--no-plugins".into(),
                    "--directory".into(),
                    cwd.to_string_lossy().into_owned(),
                ];
                if maintenance {
                    args.push("--regenerate".into());
                }
                Invocation {
                    tools: vec![
                        ("poetry".into(), node::version(versions, "poetry")?),
                        ("python".into(), python.clone()),
                    ],
                    bin: "poetry".into(),
                    cwd: None,
                    env: vec![
                        ("POETRY_VIRTUALENVS_CREATE".into(), "false".into()),
                        ("POETRY_NO_INTERACTION".into(), "1".into()),
                    ],
                    args,
                }
            };
            // Poetry moves a package as far as its constraint allows and cannot be
            // told a version, so a package whose declaration stays as it is has
            // the constraint pinned for one resolution and restored for the next.
            let pinned = if leaf == "poetry.lock" {
                Pinned::apply(root, directory, config, Manager::Pyproject, jobs)?
            } else {
                Pinned::none(root)
            };
            self.tool(driver, invocation.clone()).await?;
            if !pinned.is_empty() {
                pinned.restore()?;
                self.tool(driver, invocation).await?;
            }
            if !cwd.join(leaf).is_file() {
                return Err(format!("tool did not produce {leaf}"));
            }
            let before = baseline
                .get(&node::join(directory, leaf))
                .map(|b| b.content.as_str());
            let after = std::fs::read_to_string(cwd.join(leaf)).ok();
            let manifest = node::join(directory, "pyproject.toml");
            let edited = baseline.get(&manifest).map(|b| b.content.clone())
                != std::fs::read_to_string(root.join(&manifest)).ok();
            // A targeted upgrade must move the lock just like a manifest edit.
            if !maintenance && (edited || !packages.is_empty()) && before == after.as_deref() {
                return Err(format!(
                    "{leaf} did not change after the manifest edit in {}",
                    if directory.is_empty() {
                        "the repository root"
                    } else {
                        directory
                    }
                ));
            }
        }
        Ok(())
    }
}
