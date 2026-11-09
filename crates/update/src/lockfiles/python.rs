//! Python lockfiles: `uv lock` for `uv.lock` and `poetry lock` for `poetry.lock`.
use super::{pin::Pinned, *};

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Pyproject],
    tools: &["python", "uv", "poetry"],
    versions: true,
    install: Install::Plain,
    proof: Proof::DeclaredAndResolved,
    run: Run::Direct,
    refresh: Refresh::Locks(&[Manager::Pyproject]),
    output: Output::Default,
};

/// Lockfile names the Python ecosystem recognizes, from the manager's
/// file roles: `supported` are regenerated in place, `unsupported` are
/// names of other tools' lockfiles nothing here regenerates.
fn locks() -> patchula_manager::roles::LockNames {
    patchula_manager::roles::lock_names(Manager::Pyproject)
}

/// The interpreter both tools resolve against: the nearest `.python-version`,
/// otherwise the repository mise pin or the runner default.
fn python(
    baseline: &BTreeMap<String, Blob>,
    directory: &str,
    versions: &BTreeMap<String, String>,
) -> Result<String, String> {
    let pinned = upward(baseline.keys(), directory, &[".python-version"])
        .last()
        .copied()
        .and_then(|path| {
            baseline[path]
                .content
                .split_whitespace()
                .next()
                .map(str::to_owned)
        })
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
        context: context::Context<'_>,
        jobs: &[&Job],
    ) -> Result<(), Error> {
        let context::Context {
            driver,
            root,
            baseline,
            directory,
            config,
            versions,
            maintenance,
            public,
        } = context;
        let cwd = root.join(directory);
        let python = python(baseline, directory, versions)?;
        // uv reads basic credentials by host from `~/.netrc`, gone when the
        // locks are done, and Poetry by source name; Poetry's HTTP client may
        // read `~/.netrc` too.
        let (netrc, hosts, netrc_entries) = self.netrc();
        let mut _netrc = context::Private::default();
        if !hosts.is_empty() {
            let home = driver.join("home");
            std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
            _netrc.write(&home.join(".netrc"), &netrc)?;
        }
        let sources = [std::fs::read_to_string(cwd.join("pyproject.toml"))
            .map(|text| super::credentials::poetry_sources(&text))
            .unwrap_or_default()];
        let (poetry, poetry_entries) = self.poetry_env(&sources);
        for &leaf in locks().supported {
            if !cwd.join(leaf).is_file() {
                continue;
            }
            let packages: BTreeSet<_> = jobs
                .iter()
                .filter_map(|j| j.package().map(str::to_owned))
                .collect();
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
                for job in jobs.iter().filter(|j| j.package().is_some()) {
                    let package = job.package().unwrap_or_default();
                    args.extend([
                        "--upgrade-package".into(),
                        match (job.exact_version, job.target()) {
                            (true, Some(target)) => format!("{package}=={target}"),
                            _ => package.into(),
                        },
                    ]);
                }
                Invocation {
                    descriptor: &DESCRIPTOR,
                    tools: vec![
                        ("uv".into(), context::version(versions, "uv")?),
                        ("python".into(), python.clone()),
                    ],
                    bin: caches::Bin::Uv,
                    cwd: None,
                    // Resolve with the mise interpreter; never download another.
                    env: vec![
                        ("UV_PYTHON_DOWNLOADS".into(), "never".into()),
                        ("UV_NO_PROGRESS".into(), "1".into()),
                    ],
                    credentials: credentials::scopes(netrc_entries.iter().copied()),
                    secrets: credentials::secrets(netrc_entries.iter().copied()),
                    public,
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
                    descriptor: &DESCRIPTOR,
                    tools: vec![
                        ("poetry".into(), context::version(versions, "poetry")?),
                        ("python".into(), python.clone()),
                    ],
                    bin: caches::Bin::Poetry,
                    cwd: None,
                    env: [
                        ("POETRY_VIRTUALENVS_CREATE".into(), "false".into()),
                        ("POETRY_NO_INTERACTION".into(), "1".into()),
                    ]
                    .into_iter()
                    .chain(poetry.iter().cloned())
                    .collect(),
                    credentials: credentials::scopes(
                        poetry_entries.iter().chain(netrc_entries.iter()).copied(),
                    ),
                    secrets: credentials::secrets(
                        poetry_entries.iter().chain(netrc_entries.iter()).copied(),
                    ),
                    public,
                    args,
                }
            };
            // Poetry moves a package as far as its constraint allows and cannot be
            // told a version, so a package whose declaration stays as it is has
            // the constraint pinned for one resolution and restored for the next.
            let mut pinned = if leaf == "poetry.lock" {
                Pinned::apply(root, directory, config, Manager::Pyproject, jobs)?
            } else {
                Pinned::none(root)
            };
            self.held_tool(driver, invocation.clone()).await?;
            if !pinned.is_empty() {
                pinned.restore()?;
                self.held_tool(driver, invocation).await?;
            }
            if !cwd.join(leaf).is_file() {
                return Err(format!("tool did not produce {leaf}").into());
            }
            let before = baseline
                .get(&context::join(directory, leaf))
                .map(|b| b.content.as_str());
            let after = std::fs::read_to_string(cwd.join(leaf)).ok();
            let manifest = context::join(directory, "pyproject.toml");
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
                )
                .into());
            }
        }
        Ok(())
    }
}
