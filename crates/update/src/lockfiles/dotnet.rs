//! NuGet `packages.lock.json` files through `dotnet restore --force-evaluate`,
//! against the SDK `global.json` asks for or the pinned default.
use super::*;

fn sdk(cwd: &Path, pinned: &str) -> String {
    // The nearest global.json at or above the restore directory.
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join("global.json"))
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(version) = value["sdk"]["version"].as_str()
            && version.split('.').count() == 3
            && version
                .split('.')
                .all(|p| p.bytes().all(|b| b.is_ascii_digit()))
        {
            return version.into();
        }
        dir = d.parent();
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
/// starts empty, so the server's setting is passed on.
pub(super) fn restore_env(packages: &Path, invariant: Option<String>) -> Vec<(String, String)> {
    let mut env = vec![
        ("DOTNET_CLI_TELEMETRY_OPTOUT".into(), "1".into()),
        ("DOTNET_NOLOGO".into(), "1".into()),
        ("DOTNET_SKIP_FIRST_TIME_EXPERIENCE".into(), "1".into()),
        (
            "NUGET_PACKAGES".into(),
            packages.to_string_lossy().into_owned(),
        ),
    ];
    if let Some(invariant) = invariant {
        env.push((INVARIANT.into(), invariant));
    }
    env
}

impl Runner {
    pub(super) async fn dotnet_restore(
        &self,
        driver: &Path,
        root: &Path,
        directory: &str,
        versions: &BTreeMap<String, String>,
        edited: bool,
    ) -> Result<(), String> {
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
        let env = restore_env(
            &driver.join("nuget-packages"),
            std::env::var(INVARIANT).ok(),
        );
        self.tool(
            driver,
            Invocation {
                tools: vec![(
                    "dotnet".into(),
                    sdk(&cwd, &node::version(versions, "dotnet")?),
                )],
                bin: "dotnet".into(),
                cwd: Some(cwd.clone()),
                env,
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
            ));
        }
        Ok(())
    }
}
