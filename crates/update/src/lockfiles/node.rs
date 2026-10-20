//! Node lockfile engines: aube for pnpm and npm locks, Yarn for `yarn.lock`, Bun for `bun.lock`.
use super::*;

/// Lockfile names the Node ecosystem regenerates in place.
pub(super) const LOCKS: [&str; 5] = [
    "bun.lock",
    "npm-shrinkwrap.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
];

/// Workspace files whose `catalog`/`catalogs` feed `catalog:` specifiers.
const CATALOGS: [&str; 2] = ["pnpm-workspace.yaml", ".yarnrc.yml"];

/// The tool that owns a Node lockfile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Engine {
    /// aube reads and writes pnpm and npm lockfiles in place.
    Aube,
    /// Bun keeps `bun.lock` in Bun's own format.
    Bun,
    /// Native npm keeps `package-lock.json` in npm's own format.
    Npm,
    /// Yarn keeps `yarn.lock` in Yarn's own format.
    Yarn,
}

impl Engine {
    /// pnpm and Yarn locks have a single owner; npm locks follow the repository setting.
    fn for_lock(leaf: &str, config: &Config) -> Engine {
        match leaf {
            "bun.lock" => Engine::Bun,
            "pnpm-lock.yaml" => Engine::Aube,
            "yarn.lock" => Engine::Yarn,
            _ => match config.lockfiles.npm_engine {
                NpmEngine::Aube => Engine::Aube,
                NpmEngine::Npm => Engine::Npm,
            },
        }
    }
}

/// Yarn's release families: Classic 1 and Berry 2 or newer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Family {
    Classic,
    Berry,
}

pub(super) fn family(lock: &str) -> Option<Family> {
    if lock.contains("__metadata:") {
        return Some(Family::Berry);
    }
    if lock.contains("# yarn lockfile v1") {
        return Some(Family::Classic);
    }
    None
}

pub(super) fn major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Accept only exact release versions; ranges and dist-tags are not versions.
pub(super) fn exact(version: &str) -> Option<&str> {
    let core = version.split('+').next().unwrap_or(version);
    if core.is_empty()
        || !core.starts_with(|c: char| c.is_ascii_digit())
        || !core
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return None;
    }
    Some(core)
}

/// Canonical text for a JSON value with sorted object keys.
pub(super) fn canonical(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = String::from("{");
            for (key, value) in map {
                out.push_str(&format!("{key:?}:{}", canonical(value)));
                out.push(',');
            }
            out.push('}');
            out
        }
        serde_json::Value::Array(items) => {
            let mut out = String::from("[");
            for item in items {
                out.push_str(&canonical(item));
                out.push(',');
            }
            out.push(']');
            out
        }
        other => other.to_string(),
    }
}

/// The dependency declarations a lockfile records. Manifest edits outside this
/// projection, such as a description or a script, cannot affect a lockfile.
pub(super) fn dependencies(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let mut projection = serde_json::Map::new();
    for field in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
        "peerDependenciesMeta",
        "bundleDependencies",
        "bundledDependencies",
        "overrides",
        "resolutions",
        "workspaces",
    ] {
        if let Some(value) = object.get(field) {
            projection.insert(field.to_string(), value.clone());
        }
    }
    // pnpm and aube keep their overrides in a namespaced object.
    for namespace in ["pnpm", "aube"] {
        if let Some(inner) = object.get(namespace).and_then(|v| v.as_object()) {
            for field in ["overrides", "packageExtensions"] {
                if let Some(value) = inner.get(field) {
                    projection.insert(format!("{namespace}.{field}"), value.clone());
                }
            }
        }
    }
    Some(canonical(&serde_json::Value::Object(projection)))
}

/// Everything one Node directory needs to regenerate its lockfiles.
pub(super) struct NodeContext<'a> {
    pub driver: &'a Path,
    pub root: &'a Path,
    pub baseline: &'a BTreeMap<String, Blob>,
    pub directory: &'a str,
    pub config: &'a Config,
    pub versions: &'a BTreeMap<String, String>,
    pub maintenance: bool,
}

impl Runner {
    /// Regenerate every Node lockfile committed in one directory.
    pub(super) async fn node(&self, context: NodeContext<'_>) -> Result<(), String> {
        let NodeContext {
            driver,
            root,
            baseline,
            directory,
            config,
            versions,
            maintenance,
        } = context;
        let cwd = root.join(directory);
        for leaf in LOCKS {
            if !cwd.join(leaf).is_file() {
                continue;
            }
            let mut engine = Engine::for_lock(leaf, config);
            if engine == Engine::Aube
                && matches!(leaf, "package-lock.json" | "npm-shrinkwrap.json")
                && aube_cannot_write_npm(&cwd, leaf)?
            {
                engine = Engine::Npm;
            }
            match engine {
                Engine::Bun => self.bun(driver, &cwd, versions, maintenance).await?,
                Engine::Npm => self.npm(driver, &cwd, versions, maintenance).await?,
                Engine::Aube => self.aube(driver, &cwd, leaf, versions, maintenance).await?,
                Engine::Yarn => self.yarn(driver, &cwd, versions).await?,
            }
            if !cwd.join(leaf).is_file() {
                return Err(format!("tool did not produce {leaf}"));
            }
            if engine == Engine::Aube && cwd.join("aube-lock.yaml").exists() {
                return Err("aube wrote aube-lock.yaml instead of the existing lockfile".into());
            }
            if !maintenance && self.lock_unchanged(baseline, root, directory, leaf) {
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
    /// A manifest edit that changes recorded dependencies must change the lock.
    fn lock_unchanged(
        &self,
        baseline: &BTreeMap<String, Blob>,
        root: &Path,
        directory: &str,
        leaf: &str,
    ) -> bool {
        let manifest = join(directory, "package.json");
        let lock = join(directory, leaf);
        let before = baseline.get(&manifest).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&manifest)).ok();
        let changed = match (before, after) {
            (Some(before), Some(after)) => dependencies(before) != dependencies(&after),
            (Some(_), None) => true,
            (None, Some(_)) => true,
            (None, None) => false,
        } || CATALOGS.iter().any(|leaf| {
            // A catalog edit changes recorded dependencies as much as a manifest edit.
            let path = join(directory, leaf);
            baseline.get(&path).map(|b| b.content.clone())
                != std::fs::read_to_string(root.join(&path)).ok()
        });
        if !changed {
            return false;
        }
        let before = baseline
            .get(&lock)
            .map(|b| b.content.as_str())
            .unwrap_or("");
        let after = std::fs::read_to_string(root.join(&lock)).unwrap_or_default();
        before == after
    }
    async fn bun(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
    ) -> Result<(), String> {
        let version = version(versions, "bun")?;
        if bun_too_old(&version) {
            return Err(format!(
                "bun.lock requires Bun >=1.2; repository pins {version}; update its mise.toml first"
            ));
        }
        if maintenance {
            std::fs::remove_file(cwd.join("bun.lock")).map_err(|e| e.to_string())?;
        }
        self.tool(
            driver,
            Invocation {
                tools: vec![("bun".into(), version)],
                bin: "bun".into(),
                cwd: None,
                env: Vec::new(),
                args: vec![
                    "install".into(),
                    "--cwd".into(),
                    cwd.to_string_lossy().into_owned(),
                    "--lockfile-only".into(),
                    "--ignore-scripts".into(),
                ],
            },
        )
        .await
    }
    async fn npm(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
    ) -> Result<(), String> {
        let args = vec![
            if maintenance { "update" } else { "install" }.to_string(),
            "--prefix".into(),
            cwd.to_string_lossy().into_owned(),
            "--package-lock-only".into(),
            "--ignore-scripts".into(),
            "--no-audit".into(),
            "--no-fund".into(),
        ];
        self.tool(
            driver,
            Invocation {
                tools: vec![("node".into(), version(versions, "node")?)],
                bin: "npm".into(),
                cwd: None,
                env: Vec::new(),
                args,
            },
        )
        .await
    }
    async fn aube(
        &self,
        driver: &Path,
        cwd: &Path,
        leaf: &str,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
    ) -> Result<(), String> {
        // aube picks the lockfile it writes from the files it finds. Name the
        // committed lockfile explicitly so a second lockfile cannot redirect it.
        let config = driver.join(format!("aube-{}.npmrc", hash(leaf.as_bytes())));
        std::fs::write(&config, format!("default-lockfile={leaf}\n")).map_err(|e| e.to_string())?;
        let mut args = vec!["-C".into(), cwd.to_string_lossy().into_owned()];
        if maintenance {
            args.extend([
                "update".into(),
                "--lockfile-only".into(),
                "--no-save".into(),
                "--ignore-scripts".into(),
            ]);
        } else {
            args.extend([
                "install".into(),
                "--lockfile-only".into(),
                "--ignore-scripts".into(),
            ]);
        }
        self.tool(
            driver,
            Invocation {
                tools: vec![("aube".into(), version(versions, "aube")?)],
                bin: "aube".into(),
                cwd: None,
                env: vec![(
                    "NPM_CONFIG_USERCONFIG".into(),
                    config.to_string_lossy().into_owned(),
                )],
                args,
            },
        )
        .await
    }
    async fn yarn(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let manifest = std::fs::read_to_string(cwd.join("package.json")).ok();
        let lock = std::fs::read_to_string(cwd.join("yarn.lock")).unwrap_or_default();
        let defaults = tool_defaults()?;
        let classic = defaults["tools"]["yarn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let berry = defaults["yarn"]["berry"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let yarn = yarn_version(manifest.as_deref(), &lock, versions, &classic, &berry)?;
        let berry = family(&lock) == Some(Family::Berry);
        let args = if berry {
            vec!["install".into(), "--mode=update-lockfile".into()]
        } else {
            vec!["install".into(), "--ignore-scripts".into()]
        };
        // The manifest as planned, before Yarn may rewrite it in its own layout.
        let planned = manifest.clone();
        // Yarn reads registries, cache keys and catalogs from `.yarnrc.yml`, but
        // its `yarnPath` release and `plugins` are repository code, and `${VAR}`
        // references need credentials the runner never passes. Run Yarn on a
        // copy without them and restore the committed file afterwards.
        let rc = cwd.join(".yarnrc.yml");
        let committed = std::fs::read_to_string(&rc).ok();
        if let Some(text) = &committed {
            std::fs::write(&rc, yarnrc(text)?).map_err(|e| e.to_string())?;
        }
        let result = self
            .tool(
                driver,
                Invocation {
                    tools: vec![
                        ("node".into(), version(versions, "node")?),
                        ("yarn".into(), yarn),
                    ],
                    bin: "yarn".into(),
                    cwd: Some(cwd.to_path_buf()),
                    env: vec![
                        ("YARN_ENABLE_IMMUTABLE_INSTALLS".into(), "0".into()),
                        ("YARN_IGNORE_PATH".into(), "1".into()),
                        ("YARN_ENABLE_TELEMETRY".into(), "0".into()),
                        ("YARN_ENABLE_PROGRESS_BARS".into(), "0".into()),
                        (
                            "YARN_GLOBAL_FOLDER".into(),
                            driver.join("home/yarn").to_string_lossy().into_owned(),
                        ),
                    ],
                    args,
                },
            )
            .await;
        if let Some(text) = &committed {
            std::fs::write(&rc, text).map_err(|e| e.to_string())?;
        }
        result?;
        // Berry rewrites the manifest in its own layout. Keep the planned text
        // when only the formatting changed, and refuse a real manifest edit.
        if let Some(planned) = planned
            && let Ok(after) = std::fs::read_to_string(cwd.join("package.json"))
            && after != planned
        {
            if dependencies(&after) != dependencies(&planned) {
                return Err("yarn changed the manifest dependency declarations".into());
            }
            std::fs::write(cwd.join("package.json"), &planned).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// npm lockfiles aube 2.8.0 cannot keep valid: format v1, which it cannot read,
/// and trees with local `file:`/`link:` dependencies, whose entries it drops so
/// `npm ci` rejects the lock (aube#1796). Native npm owns these locks.
pub(super) fn aube_cannot_write_npm(cwd: &Path, leaf: &str) -> Result<bool, String> {
    let lock = std::fs::read_to_string(cwd.join(leaf)).map_err(|e| e.to_string())?;
    if serde_json::from_str::<serde_json::Value>(&lock)
        .ok()
        .and_then(|value| value.get("lockfileVersion")?.as_u64())
        .is_some_and(|version| version < 2)
    {
        return Ok(true);
    }
    fn local(dir: &Path) -> Result<bool, String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let name = entry.file_name();
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                if !matches!(name.to_str(), Some("node_modules" | ".git")) && local(&path)? {
                    return Ok(true);
                }
                continue;
            }
            if name != "package.json" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let found = [
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
            ]
            .iter()
            .filter_map(|field| manifest.get(*field)?.as_object())
            .flat_map(|deps| deps.values())
            .filter_map(serde_json::Value::as_str)
            .any(|spec| spec.starts_with("file:") || spec.starts_with("link:"));
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }
    local(cwd)
}

pub(super) fn join(directory: &str, leaf: &str) -> String {
    if directory.is_empty() {
        leaf.to_string()
    } else {
        format!("{directory}/{leaf}")
    }
}

fn version(versions: &BTreeMap<String, String>, tool: &str) -> Result<String, String> {
    versions
        .get(tool)
        .cloned()
        .ok_or_else(|| format!("missing {tool} version"))
}

fn bun_too_old(version: &str) -> bool {
    let mut parts = version
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    matches!(
        (parts.next(), parts.next()),
        (Some(0), _) | (Some(1), Some(0 | 1))
    )
}

/// Yarn's version comes from `packageManager`, then the repository pin, then the
/// format of the committed lockfile.
pub(super) fn yarn_version(
    manifest: Option<&str>,
    lock: &str,
    versions: &BTreeMap<String, String>,
    classic: &str,
    berry: &str,
) -> Result<String, String> {
    let pinned = manifest
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| {
            value
                .get("packageManager")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .and_then(|field| field.strip_prefix("yarn@").map(str::to_owned))
        .and_then(|version| exact(&version).map(str::to_owned));
    if let Some(version) = pinned {
        return Ok(version);
    }
    let family = family(lock).ok_or("cannot determine the Yarn lockfile format")?;
    let candidate = versions.get("yarn").map(String::as_str).unwrap_or("");
    let wanted = |major: u32| match family {
        Family::Classic => major == 1,
        Family::Berry => major >= 2,
    };
    if major(candidate).is_some_and(wanted) {
        return Ok(candidate.to_string());
    }
    let fallback = match family {
        Family::Classic => classic,
        Family::Berry => berry,
    };
    if fallback.is_empty() {
        return Err("missing pinned Yarn version".into());
    }
    Ok(fallback.to_string())
}

/// `.yarnrc.yml` without repository code (`yarnPath`, `plugins`) or `${VAR}`
/// interpolations, which would fail without the credentials they name.
pub(super) fn yarnrc(text: &str) -> Result<String, String> {
    fn clean(value: &mut serde_yaml_ng::Value) {
        if let Some(map) = value.as_mapping_mut() {
            map.retain(|key, value| {
                !matches!(key.as_str(), Some("yarnPath" | "plugins"))
                    && !value.as_str().is_some_and(|s| s.contains("${"))
            });
            for (_, value) in map.iter_mut() {
                clean(value);
            }
        }
    }
    let mut value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    clean(&mut value);
    serde_yaml_ng::to_string(&value).map_err(|e| e.to_string())
}
