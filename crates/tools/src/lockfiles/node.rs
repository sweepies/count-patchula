//! Node lockfile engines: aube for pnpm, npm and Yarn Classic locks, Yarn for
//! Berry `yarn.lock`, Bun for `bun.lock`.
use super::*;

mod sources;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Npm, Manager::Bun],
    tools: &["node", "bun", "aube", "yarn"],
    install: Install::Plain,
    proof: Proof::DeclaredAndResolved,
    run: Run::Direct,
    output: Output::Default,
};

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

fn major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Accept only exact release versions; ranges and dist-tags are not versions.
fn exact(version: &str) -> Option<&str> {
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

/// The dependency declarations a lockfile records. Manifest edits outside this
/// projection, such as a description or a script, cannot affect a lockfile.
/// JSON values compare without regard to key order.
pub(super) fn dependencies(text: &str) -> Option<serde_json::Value> {
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
    Some(serde_json::Value::Object(projection))
}

impl Runner {
    /// Regenerate every Node lockfile committed in one directory.
    pub(super) async fn node(&self, context: context::Context<'_>) -> Result<(), Failure> {
        // Operator registry credentials the entries allow tools to use, in
        // the run's own user configuration, gone when the tools finish.
        let _credentials = registry_credentials(context.driver, &self.registries)?;
        // Tools read registries from the repository's `.npmrc`, so a private
        // scope never resolves to the public package of its name. They get
        // only those lines, never tokens or settings that change how they
        // run, and the committed files come back afterwards.
        let mut npmrcs = context::Replaced::default();
        for (path, blob) in context
            .baseline
            .iter()
            .filter(|(path, _)| path.rsplit('/').next() == Some(".npmrc"))
        {
            npmrcs.replace(
                &context.root.join(path),
                blob.content.clone(),
                &npmrc(&blob.content),
            )?;
        }
        let result = self.node_locks(context).await;
        npmrcs.restore()?;
        result
    }
    async fn node_locks(&self, context: context::Context<'_>) -> Result<(), Failure> {
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
        for leaf in node_locks() {
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
            refuse_code_sources(engine, root, baseline, directory, leaf)?;
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
                Engine::Yarn => self.yarn(driver, root, &cwd, versions, public).await?,
            }
            if !cwd.join(leaf).is_file() {
                return Err(format!("tool did not produce {leaf}").into());
            }
            // A registry package can bring in a source the files above did
            // not name; a lock that records one is thrown away. One the
            // Count cannot read is left to the checks that follow.
            if let sources::Lock::Refused(reason) = lock_sources(
                engine,
                &format!("the regenerated {}", context::join(directory, leaf)),
                &std::fs::read_to_string(cwd.join(leaf)).unwrap_or_default(),
            ) {
                return Err(Failure::Refused(reason));
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
                return Err(context::unchanged(
                    leaf,
                    directory,
                    context::Trigger::ManifestEdit,
                ));
            }
        }
        Ok(())
    }
    /// A manifest edit that changes recorded dependencies must change the lock.
    fn lock_unchanged(
        &self,
        baseline: &BTreeMap<String, BaseFile>,
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
    async fn node_tool(&self, driver: &Path, mut call: Invocation) -> Result<Vec<u8>, Failure> {
        call.held = credentials::Held::of(self.registries.iter());
        self.tool(driver, call).await
    }
    async fn bun(
        &self,
        driver: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        maintenance: bool,
        public: bool,
    ) -> Result<(), Failure> {
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
                held: credentials::Held::default(),
                refused: None,
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
    ) -> Result<(), Failure> {
        let args = vec![
            if maintenance { "update" } else { "install" }.to_string(),
            "--prefix".into(),
            cwd.to_string_lossy().into_owned(),
            "--package-lock-only".into(),
            "--ignore-scripts".into(),
            "--no-audit".into(),
            "--no-fund".into(),
        ];
        let guard = driver.join("npm-prepare-guard.cjs");
        std::fs::write(&guard, NPM_PREPARE_GUARD).map_err(|e| e.to_string())?;
        self.node_tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("node".into(), context::version(versions, "node")?)],
                bin: caches::Bin::Npm,
                cwd: None,
                env: vec![
                    ("GIT_ALLOW_PROTOCOL".into(), NO_GIT_TRANSPORT.into()),
                    (
                        "NODE_OPTIONS".into(),
                        format!("--require={}", guard.to_string_lossy()),
                    ),
                ],
                held: credentials::Held::default(),
                refused: None,
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
    ) -> Result<(), Failure> {
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
                held: credentials::Held::default(),
                refused: None,
                public,
                args,
            },
        )
        .await
        .map(drop)
    }
    pub(super) async fn yarn(
        &self,
        driver: &Path,
        root: &Path,
        cwd: &Path,
        versions: &BTreeMap<String, String>,
        public: bool,
    ) -> Result<(), Failure> {
        let manifest = std::fs::read_to_string(cwd.join("package.json")).ok();
        let lock = std::fs::read_to_string(cwd.join("yarn.lock")).unwrap_or_default();
        if family(&lock) != Some(Family::Berry) {
            return Err("cannot determine the Yarn lockfile format".into());
        }
        let yarn = yarn_version(manifest.as_deref(), versions, default_version("yarn")?)?;
        let args = vec!["install".into(), "--mode=update-lockfile".into()];
        // The manifest as planned, before Yarn may rewrite it in its own layout.
        let planned = manifest;
        let call = Invocation {
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
                ("YARN_ENABLE_SCRIPTS".into(), "0".into()),
                ("GIT_ALLOW_PROTOCOL".into(), NO_GIT_TRANSPORT.into()),
            ],
            held: credentials::Held::default(),
            refused: None,
            public,
            args,
        };
        // Yarn reads registries, cache keys and catalogs from every
        // `.yarnrc.yml` from its working directory up to the filesystem root,
        // but the scratch tree holds no `.yarn/` for a `yarnPath` release or
        // `plugins`, `${VAR}` references name variables the run never has,
        // and `networkSettings` could reopen the hosts the run's user
        // configuration closes.
        // Run Yarn on copies without them, each one from the project up to
        // the tree root, and restore the committed files afterwards. Above
        // the tree root are the Count's own directories.
        let mut rcs = context::Replaced::default();
        for dir in cwd.ancestors().take_while(|dir| dir.starts_with(root)) {
            let rc = dir.join(".yarnrc.yml");
            if let Ok(committed) = std::fs::read_to_string(&rc) {
                let sanitized = yarnrc(&committed)?;
                rcs.replace(&rc, committed, &sanitized)?;
            }
        }
        let result = self.node_tool(driver, call).await;
        rcs.restore()?;
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

/// The tool name a refusal gives for `engine`, or `None` for an engine that
/// locks git dependencies without running their scripts (aube, Bun).
fn refusing(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Npm => Some("npm"),
        Engine::Yarn => Some("Yarn"),
        Engine::Aube | Engine::Bun => None,
    }
}

/// What the lock text `text`, labelled `path`, says about its sources to
/// `engine`.
fn lock_sources(engine: Engine, path: &str, text: &str) -> sources::Lock {
    match engine {
        Engine::Npm => sources::npm_lock(path, text),
        Engine::Yarn => sources::berry_lock(path, text),
        Engine::Aube | Engine::Bun => sources::Lock::Clean,
    }
}

/// Refuse a run of npm or Yarn whose files name a source the tool can only
/// lock by running code the dependency chose (see `sources`): every
/// `package.json` in the tree, since a workspace or a local path can reach
/// any of them, the lock itself, and for Yarn every `.yarnrc.yml` it reads,
/// from the project up to the tree root, for catalogs and package
/// extensions.
fn refuse_code_sources(
    engine: Engine,
    root: &Path,
    baseline: &BTreeMap<String, BaseFile>,
    directory: &str,
    leaf: &str,
) -> Result<(), Failure> {
    let Some(tool) = refusing(engine) else {
        return Ok(());
    };
    for path in baseline
        .keys()
        .filter(|path| path.rsplit('/').next() == Some("package.json"))
    {
        if let Ok(text) = std::fs::read_to_string(root.join(path))
            && let Some(reason) = sources::manifest(tool, path, &text)
        {
            return Err(Failure::Refused(reason));
        }
    }
    let lock = context::join(directory, leaf);
    match lock_sources(
        engine,
        &lock,
        &std::fs::read_to_string(root.join(&lock)).unwrap_or_default(),
    ) {
        sources::Lock::Clean => {}
        sources::Lock::Refused(reason) => return Err(Failure::Refused(reason)),
        sources::Lock::Unreadable => {
            return Err(Failure::Refused(format!(
                "{lock} is not a lock the Count can read, so nothing proves {tool} would regenerate it without running code"
            )));
        }
    }
    if engine == Engine::Yarn {
        for dir in root
            .join(directory)
            .ancestors()
            .take_while(|dir| dir.starts_with(root))
        {
            let rc = dir.join(".yarnrc.yml");
            if let Ok(text) = std::fs::read_to_string(&rc) {
                let path = rc.strip_prefix(root).unwrap_or(&rc).to_string_lossy();
                if let Some(reason) = sources::yarnrc(&path, &text) {
                    return Err(Failure::Refused(reason));
                }
            }
        }
    }
    Ok(())
}

/// A `GIT_ALLOW_PROTOCOL` naming no transport Git has, so every clone,
/// fetch and ls-remote a run starts fails before it connects. npm and Yarn
/// get it: a git dependency the Count's checks did not see, such as one a
/// registry package declares, never reaches the packing that would run its
/// scripts.
pub(super) const NO_GIT_TRANSPORT: &str = "none";

/// Loaded into every Node process of an npm run (`NODE_OPTIONS=--require`).
/// pacote prepares a git dependency by starting `npm install` in its
/// checkout with `_PACOTE_NO_PREPARE_` set, and before pacote 22 it does so
/// whatever `--ignore-scripts` says. A GitHub dependency pinned to a commit
/// arrives as a codeload tarball over HTTPS, so Git's transport setting does
/// not stop it. The guard stops that process before it reads anything the
/// dependency holds.
pub(super) const NPM_PREPARE_GUARD: &str = "if (process.env._PACOTE_NO_PREPARE_ !== undefined) {\n  process.stderr.write('patchula: git dependency preparation refused\\n');\n  process.exit(97);\n}\n";

/// Yarn's user configuration for every run: its GitHub fetcher downloads a
/// repository archive over HTTPS and packs it, running its scripts, so the
/// hosts it downloads from are closed. Project `networkSettings` are dropped
/// from the run's copies of `.yarnrc.yml`, so none can reopen them.
const YARN_CLOSED_HOSTS: &[&str] = &["github.com", "codeload.github.com"];

/// npm lockfiles aube cannot keep valid: format v1, which aube cannot read.
/// Native npm owns these locks.
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

/// Yarn's user configuration: each registry entry's credentials, and the
/// GitHub hosts closed (`YARN_CLOSED_HOSTS`).
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
    if !entries.is_empty() {
        root.insert("npmRegistries".into(), entries.into());
    }
    let mut closed = serde_yaml_ng::Mapping::new();
    for host in YARN_CLOSED_HOSTS {
        let mut settings = serde_yaml_ng::Mapping::new();
        settings.insert("enableNetwork".into(), false.into());
        closed.insert((*host).into(), settings.into());
    }
    root.insert("networkSettings".into(), closed.into());
    serde_yaml_ng::to_string(&root).map_err(|e| e.to_string())
}

/// The run's npm, Bun and Yarn user configuration, holding registry
/// credentials when there are any, gone when dropped. Yarn's is written
/// for every run: it also closes the hosts `YARN_CLOSED_HOSTS` names.
fn registry_credentials(
    driver: &Path,
    registries: &[patchula_http::Credential],
) -> Result<context::Private, String> {
    let mut written = context::Private::default();
    let home = driver.join("home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    written.write(&home.join(".yarnrc.yml"), &yarn_auth(registries)?)?;
    if registries.is_empty() {
        return Ok(written);
    }
    let npm = npm_auth(registries);
    // npm reads the sandbox's user configuration, Bun `~/.npmrc`.
    for name in ["npm-user.cfg", ".npmrc"] {
        written.write(&home.join(name), &npm)?;
    }
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
/// `plugins`, and the variables `${VAR}` interpolations name; and without
/// `networkSettings`, which could reopen a host the run closes.
pub(super) fn yarnrc(text: &str) -> Result<String, String> {
    fn clean(value: &mut serde_yaml_ng::Value) {
        if let Some(map) = value.as_mapping_mut() {
            map.retain(|key, value| {
                // A `${VAR}` registry stays, so Yarn stops on it rather than
                // reading the public registry instead.
                !matches!(
                    key.as_str(),
                    Some("yarnPath" | "plugins" | "networkSettings")
                ) && (key.as_str() == Some("npmRegistryServer")
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
        // With no entries, no npm credentials are written, and Yarn's
        // configuration only closes the GitHub hosts.
        let _none = registry_credentials(driver.path(), &[]).unwrap();
        assert!(!home.join(".npmrc").exists());
        assert!(!home.join("npm-user.cfg").exists());
        let yarn: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&std::fs::read_to_string(home.join(".yarnrc.yml")).unwrap())
                .unwrap();
        assert!(yarn.get("npmRegistries").is_none(), "{yarn:?}");
        for host in YARN_CLOSED_HOSTS {
            assert_eq!(
                yarn["networkSettings"][*host]["enableNetwork"],
                serde_yaml_ng::Value::Bool(false),
                "{host}"
            );
        }
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
