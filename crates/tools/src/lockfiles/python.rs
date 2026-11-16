//! Python lockfiles: `uv lock` for `uv.lock` and `poetry lock` for
//! `poetry.lock`, neither of which may build a package.
//!
//! Both tools read a package's metadata by building it whenever the metadata
//! is not static, which runs the package's build backend: code the
//! repository or the package chose. uv runs with `--no-build`, which stops
//! every build except the workspace's own packages, so a workspace package
//! whose metadata is not static is refused before uv starts. Poetry has no
//! such setting for locking, so it only ever runs under the Count's launcher
//! (`poetry_guard.py`), which stops the process at any entry point that
//! would build or load a plugin, and only at the Poetry version the
//! launcher's canary covers.
use super::{pin::Pinned, *};

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Pyproject],
    tools: &["python", "uv", "poetry"],
    versions: true,
    install: Install::Plain,
    proof: Proof::DeclaredAndResolved,
    run: Run::Direct,
    output: Output::Default,
};

/// The Count's Poetry launcher, written beside each run.
const POETRY_GUARD: &str = include_str!("poetry_guard.py");
/// The launcher's exit statuses: Poetry would have built a package or
/// loaded a plugin, or this Poetry is not one the launcher covers.
const POETRY_REFUSED: &str = "exit status: 86";
const POETRY_UNGUARDED: &str = "exit status: 87";

/// The interpreter both tools resolve against: the nearest `.python-version`,
/// otherwise the repository mise pin or the runner default.
fn python(
    baseline: &BTreeMap<String, BaseFile>,
    directory: &str,
    versions: &BTreeMap<String, String>,
) -> Result<String, String> {
    let pinned = upward(baseline, directory, &[".python-version"])
        .last()
        .copied()
        .and_then(|path| {
            baseline[path]
                .content
                .split_whitespace()
                .next()
                .map(str::to_owned)
        })
        .filter(|v| context::numeric(v, None));
    pinned
        .or_else(|| versions.get("python").cloned())
        .ok_or_else(|| "missing python version".into())
}

/// The refusal for `leaf` in `directory`, saying why.
fn refusal(leaf: &str, directory: &str, why: &str) -> Failure {
    Failure::Refused(format!(
        "{leaf} in {} is not regenerated: {why}, and the Count never runs repository or package code",
        context::describe_dir(directory)
    ))
}

/// Refuse `uv lock` in `directory` when uv would build one of its
/// workspace's own packages, which `--no-build` does not stop.
///
/// uv builds a workspace package to read metadata its `pyproject.toml`
/// declares `dynamic`, even a dynamic version alone. The packages checked
/// are every `pyproject.toml` at `directory` or below any ancestor that
/// declares a uv workspace (the workspace's members are below its root),
/// which covers every member however the workspace lists them. A member
/// glob that leaves the workspace root cannot be bounded and is refused
/// too; a file that does not parse fails the run before uv reads it.
pub(super) fn uv_builds(
    root: &Path,
    baseline: &BTreeMap<String, BaseFile>,
    directory: &str,
) -> Result<(), Failure> {
    let refused = |why: String| Err(refusal("uv.lock", directory, &why));
    let unparsed = |path: &str| Err(format!("uv.lock: {path} does not parse").into());
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .ok()
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
    };
    let workspace = |document: &toml_edit::DocumentMut| {
        document
            .get("tool")
            .and_then(|tool| tool.get("uv"))
            .and_then(|uv| uv.get("workspace"))
            .cloned()
    };
    let mut roots = Vec::new();
    for path in upward(baseline, directory, &["pyproject.toml"]) {
        let Some(document) = read(path) else {
            return unparsed(path);
        };
        if let Some(workspace) = workspace(&document) {
            for key in ["members", "exclude"] {
                let globs = workspace.get(key).and_then(toml_edit::Item::as_array);
                for glob in globs.into_iter().flatten() {
                    let bounded = glob.as_str().is_some_and(|glob| {
                        !glob.starts_with('/')
                            && !glob.contains('\\')
                            && glob.split('/').all(|part| part != "..")
                    });
                    if !bounded {
                        return refused(format!(
                            "{path} lists a workspace {key} entry outside the workspace"
                        ));
                    }
                }
            }
            roots.push(contract::directory(path).to_owned());
        }
    }
    let candidates = baseline.keys().filter(|path| {
        path.rsplit('/').next() == Some("pyproject.toml") && {
            let dir = contract::directory(path);
            dir == directory || roots.iter().any(|root| within(dir, root))
        }
    });
    for path in candidates {
        let Some(document) = read(path) else {
            return unparsed(path);
        };
        let dynamic = document
            .get("project")
            .and_then(|project| project.get("dynamic"));
        let fields = match dynamic.map(toml_edit::Item::as_array) {
            None => continue,
            Some(Some(fields)) if fields.is_empty() => continue,
            Some(Some(fields)) => fields
                .iter()
                // Field names as PEP 621 spells them; anything else is not
                // repeated into the reason.
                .map(|field| {
                    field
                        .as_str()
                        .filter(|name| {
                            name.len() <= 32
                                && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                        })
                        .unwrap_or("?")
                })
                .collect::<Vec<_>>()
                .join(", "),
            Some(None) => "?".to_owned(),
        };
        return refused(format!(
            "{path} declares dynamic metadata ({fields}), which uv reads by building the package"
        ));
    }
    Ok(())
}

/// Why uv stopped, when its failure says it would have had to build a
/// package that `--no-build` keeps it from building.
fn uv_refused(failure: &patchula_process::Failure) -> Option<&'static str> {
    let patchula_process::Failure::Exited { stdout, stderr, .. } = failure else {
        return None;
    };
    let needs_build = [stdout, stderr].iter().any(|text| {
        [
            "Building source distributions",
            "building from source is disabled",
            "has no usable wheels",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    });
    needs_build.then_some("uv would have to build a package to read its metadata")
}

/// Why Poetry stopped, when the launcher stopped it.
fn poetry_refused(failure: &patchula_process::Failure) -> Option<&'static str> {
    match failure {
        patchula_process::Failure::Exited { status, .. } if status == POETRY_REFUSED => {
            Some("Poetry would have to build a package to read its metadata, or load a plugin")
        }
        patchula_process::Failure::Exited { status, .. } if status == POETRY_UNGUARDED => {
            Some("the Poetry that would run is not one its guard covers")
        }
        _ => None,
    }
}

impl Runner {
    pub(super) async fn python_locks(
        &self,
        context: context::Context<'_>,
        jobs: &[&Job],
    ) -> Result<(), Failure> {
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
        let targeted = jobs.iter().any(|j| j.package().is_some());
        for &leaf in lock_names(Manager::Pyproject).supported {
            if !cwd.join(leaf).is_file() {
                continue;
            }
            type Refused = fn(&patchula_process::Failure) -> Option<&'static str>;
            let (invocation, refused): (Invocation, Refused) = if leaf == "uv.lock" {
                uv_builds(root, baseline, directory)?;
                let mut args = vec![
                    "lock".to_string(),
                    "--directory".into(),
                    cwd.to_string_lossy().into_owned(),
                    // Every other package comes as a wheel or not at all.
                    "--no-build".into(),
                ];
                if maintenance {
                    args.push("--upgrade".into());
                }
                // A declaration left as it is gets the exact version, so the
                // lock cannot move past it.
                for (job, package) in jobs.iter().filter_map(|j| Some((j, j.package()?))) {
                    args.extend([
                        "--upgrade-package".into(),
                        match (job.exact_version, job.target()) {
                            (true, Some(target)) => format!("{package}=={target}"),
                            _ => package.into(),
                        },
                    ]);
                }
                (
                    Invocation {
                        descriptor: &DESCRIPTOR,
                        tools: vec![
                            ("uv".into(), context::version(versions, "uv")?),
                            ("python".into(), python.clone()),
                        ],
                        bin: caches::Bin::Uv,
                        cwd: None,
                        env: vec![
                            // Resolve with the mise interpreter; never
                            // download another, and never one a
                            // `.python-version` names by path.
                            ("UV_PYTHON_DOWNLOADS".into(), "never".into()),
                            ("UV_PYTHON".into(), python.clone()),
                            ("UV_NO_BUILD".into(), "1".into()),
                            ("UV_NO_PROGRESS".into(), "1".into()),
                        ],
                        credentials: credentials::scopes(netrc_entries.iter().copied()),
                        secrets: credentials::secrets(netrc_entries.iter().copied()),
                        public,
                        args,
                    },
                    uv_refused,
                )
            } else {
                let version = context::version(versions, "poetry")?;
                if version != default_version("poetry")? {
                    return Err(refusal(
                        leaf,
                        directory,
                        &format!(
                            "the repository pins Poetry {version}, and the guard that keeps Poetry from building packages covers only {}",
                            default_version("poetry")?
                        ),
                    ));
                }
                let guard = driver.join("poetry_guard.py");
                std::fs::write(&guard, POETRY_GUARD).map_err(|e| e.to_string())?;
                // `poetry lock` keeps existing pins and re-resolves changed
                // constraints; `--regenerate` refreshes everything within them.
                let mut args = vec![
                    "-I".to_string(),
                    guard.to_string_lossy().into_owned(),
                    version.clone(),
                    "lock".into(),
                    "--no-interaction".into(),
                    "--no-plugins".into(),
                    "--directory".into(),
                    cwd.to_string_lossy().into_owned(),
                ];
                if maintenance {
                    args.push("--regenerate".into());
                }
                (
                    Invocation {
                        descriptor: &DESCRIPTOR,
                        tools: vec![
                            ("poetry".into(), version),
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
                    },
                    poetry_refused,
                )
            };
            let refuse = |why: &str| refusal(leaf, directory, why);
            // Poetry moves a package as far as its constraint allows and cannot be
            // told a version, so a package whose declaration stays as it is has
            // the constraint pinned for one resolution and restored for the next.
            let mut pinned = if leaf == "poetry.lock" {
                Pinned::apply(root, directory, config, Manager::Pyproject, jobs)?
            } else {
                Pinned::none(root)
            };
            self.refusable_tool(driver, invocation.clone(), refused, refuse)
                .await?;
            if !pinned.is_empty() {
                pinned.restore()?;
                self.refusable_tool(driver, invocation, refused, refuse)
                    .await?;
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
            if !maintenance && (edited || targeted) && before == after.as_deref() {
                let trigger = if edited {
                    context::Trigger::ManifestEdit
                } else {
                    context::Trigger::TargetedUpgrade
                };
                return Err(context::unchanged(leaf, directory, trigger));
            }
        }
        Ok(())
    }
}
