//! NuGet `packages.lock.json` files through `dotnet restore --force-evaluate`,
//! against the SDK `global.json` asks for or the pinned default.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Nuget],
    tools: &["dotnet"],
    versions: true,
    install: Install::Plain,
    proof: Proof::None,
    run: Run::Direct,
    refresh: Refresh::Locks(&[Manager::Nuget]),
    output: Output::Default,
};

/// The SDK the nearest `global.json` at or above the restore directory asks
/// for, else the pinned default.
fn sdk(baseline: &BTreeMap<String, Blob>, root: &Path, directory: &str, pinned: &str) -> String {
    for path in upward(baseline, directory, &["global.json"])
        .into_iter()
        .rev()
    {
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(version) = value["sdk"]["version"].as_str() else {
            continue;
        };
        if version.split('.').count() == 3
            && version
                .split('.')
                .all(|p| p.bytes().all(|b| b.is_ascii_digit()))
        {
            return version.into();
        }
    }
    pinned.into()
}

/// The single solution, else the single project, `dotnet restore` builds.
fn target(cwd: &Path) -> Result<PathBuf, String> {
    let entries: Vec<PathBuf> = std::fs::read_dir(cwd)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let pick = |extensions: &[&str]| -> Vec<PathBuf> {
        entries
            .iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| extensions.contains(&e))
            })
            .cloned()
            .collect()
    };
    for found in [
        pick(&["sln", "slnx"]),
        pick(&["csproj", "fsproj", "vbproj"]),
    ] {
        match found.len() {
            0 => continue,
            1 => return Ok(found[0].clone()),
            _ => return Err("several solutions or projects; restore is ambiguous".into()),
        }
    }
    Err("no solution or project to restore".into())
}

fn locks(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n != "obj" && n != "bin" && n != ".git")
            {
                locks(&path, out);
            }
        } else if path.file_name().is_some_and(|n| n == "packages.lock.json") {
            out.push(path);
        }
    }
}
/// The text of every `nuget.config` under `dir`, in any case, or why it
/// could not be read.
fn nuget_configs(dir: &Path, out: &mut Vec<std::io::Result<String>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n != "obj" && n != "bin" && n != ".git")
            {
                nuget_configs(&path, out);
            }
        } else if entry
            .file_name()
            .to_str()
            .is_some_and(patchula_manager::roles::nuget_config)
        {
            out.push(std::fs::read_to_string(&path));
        }
    }
}
/// Restore writes `obj/` intermediates, which never belong in a commit.
fn remove_intermediates(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "obj") {
                std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
            } else if path.file_name().is_some_and(|n| n != ".git") {
                remove_intermediates(&path)?;
            }
        }
    }
    Ok(())
}

const INVARIANT: &str = "DOTNET_SYSTEM_GLOBALIZATION_INVARIANT";

/// The restore's environment. The server image has no ICU and sets
/// `DOTNET_SYSTEM_GLOBALIZATION_INVARIANT` instead; the tool's environment
/// starts empty, so the server's setting is passed on. The NuGet caches are
/// the cache partition's.
pub(super) fn restore_env(invariant: Option<String>) -> Vec<(String, String)> {
    let mut env = vec![
        ("DOTNET_CLI_TELEMETRY_OPTOUT".into(), "1".into()),
        ("DOTNET_NOLOGO".into(), "1".into()),
        ("DOTNET_SKIP_FIRST_TIME_EXPERIENCE".into(), "1".into()),
    ];
    if let Some(invariant) = invariant {
        env.push((INVARIANT.into(), invariant));
    }
    env
}

impl Runner {
    pub(super) async fn dotnet_restore(
        &self,
        context: context::Context<'_>,
        edited: bool,
    ) -> Result<(), Error> {
        let context::Context {
            driver,
            root,
            baseline,
            directory,
            versions,
            public,
            ..
        } = context;
        let cwd = root.join(directory);
        let mut before = Vec::new();
        locks(&cwd, &mut before);
        let before: BTreeMap<_, _> = before
            .into_iter()
            .map(|p| {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                (p, text)
            })
            .collect();
        let target = target(&cwd)?;
        // Each source under a tools entry gets its credentials by key. NuGet
        // reads `nuget.config` from the project up, and a project can name
        // any other, so every one in the tree counts: a key defined two ways
        // gets nothing, and a config the Count cannot read, which could define
        // any key, leaves every source without.
        let credentials = if self.registries.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let mut configs = Vec::new();
            nuget_configs(root, &mut configs);
            configs
                .iter()
                .map(|text| {
                    text.as_deref()
                        .ok()
                        .and_then(super::credentials::nuget_sources)
                })
                .collect::<Option<Vec<_>>>()
                .map(|sources| self.nuget_env(&sources))
                .unwrap_or_default()
        };
        let (credentials, entries) = credentials;
        let mut env = restore_env(std::env::var(INVARIANT).ok());
        env.extend(credentials);
        let pinned = context::version(versions, "dotnet")?;
        self.held_tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("dotnet".into(), sdk(baseline, root, directory, &pinned))],
                bin: caches::Bin::Dotnet,
                cwd: Some(cwd.clone()),
                env,
                credentials: credentials::scopes(entries.iter().copied()),
                secrets: credentials::secrets(entries.iter().copied()),
                public,
                args: vec![
                    "restore".into(),
                    target.to_string_lossy().into_owned(),
                    "--force-evaluate".into(),
                ],
            },
        )
        .await?;
        remove_intermediates(&cwd)?;
        let changed = before
            .iter()
            .any(|(path, text)| std::fs::read_to_string(path).ok().as_ref() != Some(text));
        if edited && !before.is_empty() && !changed {
            return Err(format!(
                "packages.lock.json did not change after the manifest edit in {}",
                if directory.is_empty() {
                    "the repository root"
                } else {
                    directory
                }
            )
            .into());
        }
        Ok(())
    }
}
