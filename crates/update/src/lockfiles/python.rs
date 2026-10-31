//! Python lockfiles: `uv lock` for `uv.lock` and `poetry lock` for `poetry.lock`.
use super::*;

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
            versions,
            maintenance,
            ..
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
                for package in &packages {
                    args.extend(["--upgrade-package".into(), package.clone()]);
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
            self.tool(driver, invocation).await?;
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
