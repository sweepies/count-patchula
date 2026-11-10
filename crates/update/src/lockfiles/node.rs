//! Node lockfile engines: aube for pnpm, npm and Yarn Classic locks, Yarn for
//! Berry `yarn.lock`, Bun for `bun.lock`.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Npm, Manager::Bun],
    tools: &["node", "bun", "aube", "yarn"],
    versions: true,
    install: Install::Plain,
    proof: Proof::DeclaredAndResolved,
    run: Run::Direct,
    refresh: Refresh::Locks(&[Manager::Npm, Manager::Bun]),
    output: Output::Default,
};

/// Lockfile names the Node ecosystem regenerates in place, from the
/// managers' file roles.
fn locks() -> Vec<&'static str> {
    super::node_locks()
}

/// Workspace files whose `catalog`/`catalogs` feed `catalog:` specifiers.
const CATALOGS: [&str; 2] = ["pnpm-workspace.yaml", ".yarnrc.yml"];

/// The tool that owns a Node lockfile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Engine {
    /// aube reads and writes `aube-lock.yaml`, pnpm, npm and Yarn Classic
    /// lockfiles in place.
    Aube,
    /// Bun keeps `bun.lock` in Bun's own format.
    Bun,
    /// Native npm keeps `package-lock.json` in npm's own format.
    Npm,
    /// Yarn Berry keeps its `yarn.lock` in Berry's own format.
    Yarn,
}

impl Engine {
    /// aube, pnpm and Yarn locks have a single owner; npm locks follow the
    /// repository setting. A Classic `yarn.lock` goes to aube, any other to Yarn.
    fn for_lock(cwd: &Path, leaf: &str, config: &Config) -> Engine {
        match leaf {
            "bun.lock" => Engine::Bun,
            "aube-lock.yaml" | "pnpm-lock.yaml" => Engine::Aube,
            "yarn.lock" => match std::fs::read_to_string(cwd.join(leaf))
                .ok()
                .as_deref()
                .and_then(family)
            {
                Some(Family::Classic) => Engine::Aube,
                _ => Engine::Yarn,
            },
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

impl Runner {
    /// Regenerate every Node lockfile committed in one directory.
    pub(super) async fn node(&self, context: context::Context<'_>) -> Result<(), Error> {
        // Operator registry credentials the entries allow tools to use, in
        // the run's own user configuration, gone when the tools finish.
        let _credentials = registry_credentials(context.driver, &self.registries)?;
        // Tools read registries from the repository's `.npmrc`, so a private
        // scope never resolves to the public package of its name. They get
        // only those lines, never tokens or settings that change how they
        // run, and the committed files come back afterwards.
        let npmrcs: Vec<_> = context
            .baseline
            .iter()
            .filter(|(path, _)| path.rsplit('/').next() == Some(".npmrc"))
            .map(|(path, blob)| (context.root.join(path), blob.content.clone()))
            .collect();
        for (path, text) in &npmrcs {
            std::fs::write(path, npmrc(text)).map_err(|e| e.to_string())?;
        }
        let result = self.node_locks(context).await;
        for (path, text) in &npmrcs {
            std::fs::write(path, text).map_err(|e| e.to_string())?;
        }
        result
    }
    async fn node_locks(&self, context: context::Context<'_>) -> Result<(), Error> {
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
        for leaf in locks() {
            if !cwd.join(leaf).is_file() {
                continue;
            }
            let mut engine = Engine::for_lock(&cwd, leaf, config);
            if engine == Engine::Aube
                && matches!(leaf, "package-lock.json" | "npm-shrinkwrap.json")
                && aube_cannot_write_npm(&cwd, leaf)?
            {
                engine = Engine::Npm;
            }
            match engine {
                Engine::Bun => {
                    self.bun(driver, &cwd, versions, maintenance, public)
                        .await?
                }
                Engine::Npm => {
                    self.npm(driver, &cwd, versions, maintenance, public)
                        .await?
                }
                Engine::Aube => {
                    self.aube(driver, &cwd, leaf, versions, maintenance, public)
                        .await?
                }
                Engine::Yarn => self.yarn(driver, &cwd, versions, public).await?,
            }
            if !cwd.join(leaf).is_file() {
                return Err(format!("tool did not produce {leaf}").into());
            }
            // aube writes its own lockfile when it is not told which one to keep.
            if engine == Engine::Aube
                && leaf != "aube-lock.yaml"
                && cwd.join("aube-lock.yaml").exists()
                && !baseline.contains_key(&context::join(directory, "aube-lock.yaml"))
            {
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
                )
                .into());
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
        let manifest = context::join(directory, "package.json");
        let lock = context::join(directory, leaf);
        let before = baseline.get(&manifest).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&manifest)).ok();
        let changed = match (before, after) {
            (Some(before), Some(after)) => dependencies(before) != dependencies(&after),
            (Some(_), None) => true,
            (None, Some(_)) => true,
            (None, None) => false,
        } || CATALOGS.iter().any(|leaf| {
            // A catalog edit changes recorded dependencies as much as a manifest edit.
            let path = context::join(directory, leaf);
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
    /// Run an npm-family tool, which holds every tools entry's credentials.
    async fn node_tool(&self, driver: &Path, mut call: Invocation) -> Result<Vec<u8>, Error> {
        call.credentials = credentials::scopes(self.registries.iter());
        call.secrets = credentials::secrets(self.registries.iter());
        self.held_tool(driver, call).await
    }
    async fn bun(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
        public: bool,
    ) -> Result<(), Error> {
        let version = context::version(versions, "bun")?;
        if bun_too_old(&version) {
            return Err(format!(
                "bun.lock requires Bun >=1.2; repository pins {version}; update its mise.toml first"
            )
            .into());
        }
        if maintenance {
            std::fs::remove_file(cwd.join("bun.lock")).map_err(|e| e.to_string())?;
        }
        self.node_tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("bun".into(), version)],
                bin: caches::Bin::Bun,
                cwd: None,
                env: Vec::new(),
                credentials: Vec::new(),
                secrets: Vec::new(),
                public,
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
        .map(drop)
    }
    async fn npm(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
        public: bool,
    ) -> Result<(), Error> {
        let args = vec![
            if maintenance { "update" } else { "install" }.to_string(),
            "--prefix".into(),
            cwd.to_string_lossy().into_owned(),
            "--package-lock-only".into(),
            "--ignore-scripts".into(),
            "--no-audit".into(),
            "--no-fund".into(),
        ];
        self.node_tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("node".into(), context::version(versions, "node")?)],
                bin: caches::Bin::Npm,
                cwd: None,
                env: Vec::new(),
                credentials: Vec::new(),
                secrets: Vec::new(),
                public,
                args,
            },
        )
        .await
        .map(drop)
    }
    async fn aube(
        &self,
        driver: &Path,
        cwd: &Path,
        leaf: &str,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
        public: bool,
    ) -> Result<(), Error> {
        // aube picks the lockfile it writes from the files it finds. Name the
        // committed lockfile explicitly so a second lockfile cannot redirect it.
        // It may hold registry credentials, which no later tool reads.
        let config = driver.join(format!("aube-{}.npmrc", hash(leaf.as_bytes())));
        let mut written = context::Private::default();
        written.write(
            &config,
            &format!("default-lockfile={leaf}\n{}", npm_auth(&self.registries)),
        )?;
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
        self.node_tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("aube".into(), context::version(versions, "aube")?)],
                bin: caches::Bin::Aube,
                cwd: None,
                env: vec![(
                    "NPM_CONFIG_USERCONFIG".into(),
                    config.to_string_lossy().into_owned(),
                )],
                credentials: Vec::new(),
                secrets: Vec::new(),
                public,
                args,
            },
        )
        .await
        .map(drop)
    }
    async fn yarn(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        public: bool,
    ) -> Result<(), Error> {
        let manifest = std::fs::read_to_string(cwd.join("package.json")).ok();
        let lock = std::fs::read_to_string(cwd.join("yarn.lock")).unwrap_or_default();
        if family(&lock) != Some(Family::Berry) {
            return Err("cannot determine the Yarn lockfile format".into());
        }
        let default = tool_defaults()?["tools"]["yarn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let yarn = yarn_version(manifest.as_deref(), versions, &default)?;
        let args = vec!["install".into(), "--mode=update-lockfile".into()];
        // The manifest as planned, before Yarn may rewrite it in its own layout.
        let planned = manifest.clone();
        // Yarn reads registries, cache keys and catalogs from `.yarnrc.yml`, but
        // the scratch tree holds no `.yarn/` for a `yarnPath` release or
        // `plugins`, and `${VAR}` references name variables the run never has. Run Yarn on a
        // copy without them and restore the committed file afterwards.
        let rc = cwd.join(".yarnrc.yml");
        let committed = std::fs::read_to_string(&rc).ok();
        if let Some(text) = &committed {
            std::fs::write(&rc, yarnrc(text)?).map_err(|e| e.to_string())?;
        }
        let result = self
            .node_tool(
                driver,
                Invocation {
                    descriptor: &DESCRIPTOR,
                    tools: vec![
                        ("node".into(), context::version(versions, "node")?),
                        ("yarn".into(), yarn),
                    ],
                    bin: caches::Bin::Yarn,
                    cwd: Some(cwd.to_path_buf()),
                    env: vec![
                        ("YARN_ENABLE_IMMUTABLE_INSTALLS".into(), "0".into()),
                        ("YARN_IGNORE_PATH".into(), "1".into()),
                        ("YARN_ENABLE_TELEMETRY".into(), "0".into()),
                        ("YARN_ENABLE_PROGRESS_BARS".into(), "0".into()),
                    ],
                    credentials: Vec::new(),
                    secrets: Vec::new(),
                    public,
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

/// npm lockfiles aube cannot keep valid: format v1, which aube 2.8.1 still
/// cannot read. Native npm owns these locks.
pub(super) fn aube_cannot_write_npm(cwd: &Path, leaf: &str) -> Result<bool, String> {
    let lock = std::fs::read_to_string(cwd.join(leaf)).map_err(|e| e.to_string())?;
    Ok(serde_json::from_str::<serde_json::Value>(&lock)
        .ok()
        .and_then(|value| value.get("lockfileVersion")?.as_u64())
        .is_some_and(|version| version < 2))
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

/// Yarn Berry's version comes from `packageManager`, then a Berry repository
/// pin, then the pinned default.
pub(super) fn yarn_version(
    manifest: Option<&str>,
    versions: &BTreeMap<String, String>,
    default: &str,
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
    let candidate = versions.get("yarn").map(String::as_str).unwrap_or("");
    if major(candidate).is_some_and(|major| major >= 2) {
        return Ok(candidate.to_string());
    }
    if default.is_empty() {
        return Err("missing pinned Yarn version".into());
    }
    Ok(default.to_string())
}

/// `.npmrc` lines giving each registry entry its credentials, keyed by its
/// prefix as npm, aube and Bun match them: `//host/path/:_authToken=` for a
/// bearer token, which registry entries hold only in the plain characters
/// bearer tokens use, and `//host/path/:_auth=` for a user and password.
pub(super) fn npm_auth(registries: &[patchula_http::Credential]) -> String {
    use base64::Engine;
    let mut out = String::new();
    for entry in registries {
        let Some(rest) = entry.prefix().strip_prefix("https:") else {
            continue;
        };
        let key = format!("{}/", rest.trim_end_matches('/'));
        match &entry.auth {
            patchula_http::Auth::Bearer { token } => {
                out.push_str(&format!("{key}:_authToken={token}\n"));
            }
            patchula_http::Auth::Basic { username, password } => {
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                out.push_str(&format!("{key}:_auth={encoded}\n"));
            }
            _ => {}
        }
    }
    out
}

/// Yarn's user configuration giving each registry entry its credentials.
pub(super) fn yarn_auth(registries: &[patchula_http::Credential]) -> Result<String, String> {
    let mut entries = serde_yaml_ng::Mapping::new();
    for entry in registries {
        let mut auth = serde_yaml_ng::Mapping::new();
        match &entry.auth {
            patchula_http::Auth::Bearer { token } => {
                auth.insert("npmAuthToken".into(), token.clone().into());
            }
            patchula_http::Auth::Basic { username, password } => {
                auth.insert(
                    "npmAuthIdent".into(),
                    format!("{username}:{password}").into(),
                );
            }
            _ => continue,
        }
        auth.insert("npmAlwaysAuth".into(), true.into());
        entries.insert(entry.prefix().trim_end_matches('/').into(), auth.into());
    }
    let mut root = serde_yaml_ng::Mapping::new();
    root.insert("npmRegistries".into(), entries.into());
    serde_yaml_ng::to_string(&root).map_err(|e| e.to_string())
}

/// The run's npm, Bun and Yarn user configuration holding registry
/// credentials, gone when dropped.
fn registry_credentials(
    driver: &Path,
    registries: &[patchula_http::Credential],
) -> Result<context::Private, String> {
    let mut written = context::Private::default();
    if registries.is_empty() {
        return Ok(written);
    }
    let home = driver.join("home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let npm = npm_auth(registries);
    // npm reads the sandbox's user configuration, Bun `~/.npmrc`.
    for name in ["npm-user.cfg", ".npmrc"] {
        written.write(&home.join(name), &npm)?;
    }
    written.write(&home.join(".yarnrc.yml"), &yarn_auth(registries)?)?;
    Ok(written)
}

/// `.npmrc` with only the registries it names: `registry=` and
/// `@scope:registry=` lines, as written. Tokens, and settings that change how
/// a tool runs, are left out. A `${VAR}` registry stays, so npm stops on it
/// rather than reading the public registry instead.
pub(super) fn npmrc(text: &str) -> String {
    text.lines()
        .filter(|line| {
            line.split_once('=').is_some_and(|(key, _)| {
                let key = key.trim();
                key == "registry"
                    || (key.starts_with('@')
                        && key.len() > "@:registry".len()
                        && key.ends_with(":registry"))
            })
        })
        .map(|line| format!("{}\n", line.trim()))
        .collect()
}

/// `.yarnrc.yml` without what the scratch tree lacks: a `yarnPath` release,
/// `plugins`, and the variables `${VAR}` interpolations name.
pub(super) fn yarnrc(text: &str) -> Result<String, String> {
    fn clean(value: &mut serde_yaml_ng::Value) {
        if let Some(map) = value.as_mapping_mut() {
            map.retain(|key, value| {
                // A `${VAR}` registry stays, so Yarn stops on it rather than
                // reading the public registry instead.
                !matches!(key.as_str(), Some("yarnPath" | "plugins"))
                    && (key.as_str() == Some("npmRegistryServer")
                        || !value.as_str().is_some_and(|s| s.contains("${")))
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

#[cfg(test)]
mod registry_credentials {
    use super::*;

    fn entries() -> Vec<patchula_http::Credential> {
        let token = patchula_http::Credential::new(
            "https://npm.acme.dev/repo",
            patchula_http::Auth::Bearer {
                token: "t0ken".into(),
            },
        )
        .unwrap()
        .for_tools();
        let basic = patchula_http::Credential::new(
            "https://npm.other.dev",
            patchula_http::Auth::Basic {
                username: "ci".into(),
                password: "pw".into(),
            },
        )
        .unwrap()
        .for_tools();
        vec![token, basic]
    }

    #[test]
    fn npm_and_yarn_get_each_registrys_credentials_by_its_prefix() {
        let npm = npm_auth(&entries());
        assert_eq!(
            npm,
            "//npm.acme.dev/repo/:_authToken=t0ken\n//npm.other.dev/:_auth=Y2k6cHc=\n"
        );
        let yarn: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&yarn_auth(&entries()).unwrap()).unwrap();
        assert_eq!(
            yarn["npmRegistries"]["https://npm.acme.dev/repo"]["npmAuthToken"],
            "t0ken"
        );
        assert_eq!(
            yarn["npmRegistries"]["https://npm.other.dev"]["npmAuthIdent"],
            "ci:pw"
        );
    }

    #[test]
    fn credential_files_are_private_and_gone_when_the_tools_finish() {
        let driver = tempfile::tempdir().unwrap();
        let home = driver.path().join("home");
        {
            let _written = registry_credentials(driver.path(), &entries()).unwrap();
            for name in ["npm-user.cfg", ".npmrc", ".yarnrc.yml"] {
                let path = home.join(name);
                assert!(path.is_file(), "{name}");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                    assert_eq!(mode & 0o077, 0, "{name}");
                }
            }
        }
        for name in ["npm-user.cfg", ".npmrc", ".yarnrc.yml"] {
            assert!(!home.join(name).exists(), "{name}");
        }
        // With no entries, nothing is written.
        let _none = registry_credentials(driver.path(), &[]).unwrap();
        assert!(!home.join(".npmrc").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_credential_file_replaces_a_link_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "kept").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        {
            let mut written = context::Private::default();
            written.write(&link, "secret").unwrap();
            assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
        assert!(!link.exists());
    }
}
