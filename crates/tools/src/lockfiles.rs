//! Real ecosystem tools run before any Git Data mutation, in disposable trees.
mod actions;
mod apm;
mod caches;
mod cargo;
mod checkout;
mod context;
mod credentials;
mod dotnet;
mod go;
mod mise;
mod nix;
mod node;
mod php;
mod pin;
mod process;
mod python;
mod ruby;
mod terraform;
mod tool;
mod tools;
pub use patchula_lockfiles::{self as contract, *};
use tool::{Install, Output, Proof, Run, Tool, descriptor, descriptors};
pub use tools::{bootstrap_mise, tools_dir};
mod wrapper;
use patchula_config::{Config, NpmEngine};
use patchula_manager::hash;
use patchula_manager::roles::{lock_names, manifest_names};
use patchula_process::{Executor, SystemExecutor};
use patchula_words::{Manager, Source};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

/// Whether `path`, materialized for a run of `descriptor`'s ecosystem, could
/// steer what its tools fetch: a manifest, lockfile or settings file of the
/// ecosystem, anywhere in the snapshot. A sibling project reached through a
/// `ProjectReference`, a `.sln`, a `file:../x`, a uv or Poetry path
/// dependency or a Composer path repository can steer the run as much as one
/// inside its directory, so the directory does not bound it.
fn steers(descriptor: &Tool, path: &str) -> bool {
    descriptor
        .steers
        .iter()
        .any(|manager| patchula_manager::roles::eco_roles(*manager, path).steers)
}

/// Whether a run for `descriptor`'s ecosystem can fetch only from the
/// ecosystem's well-known public registries: every materialized file of its
/// kinds, anywhere in the snapshot, decided public-only. A file with no
/// verdict is not public. Fail closed.
fn public_only(
    descriptor: &Tool,
    baseline: &BTreeMap<String, BaseFile>,
    verdicts: &BTreeMap<String, bool>,
) -> bool {
    baseline
        .keys()
        .all(|path| !steers(descriptor, path) || verdicts.get(path) == Some(&true))
}

fn under(path: &str, root: &str) -> bool {
    root.is_empty() || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
}
/// Whether `path` is `root` or below it: the reach a `.cargo` configuration
/// has over the projects in its directory and under it.
fn within(path: &str, root: &str) -> bool {
    path == root || under(path, root)
}
/// The Node ecosystem's supported lockfile names, from the managers' roles.
fn node_locks() -> Vec<&'static str> {
    [Manager::Npm, Manager::Bun]
        .iter()
        .flat_map(|manager| lock_names(*manager).supported.iter().copied())
        .collect()
}

#[derive(Clone)]
pub struct Runner {
    credentials: Option<Arc<dyn patchula_github::Credentials>>,
    /// Operator registry entries whose credentials lockfile tools may use.
    registries: Vec<patchula_http::Credential>,
    /// Every operator registry entry, which decides which of `registries`
    /// a URL or host falls under.
    entries: Vec<patchula_http::Credential>,
    cache: PathBuf,
    token: String,
    executor: Arc<dyn Executor>,
    mise_data: PathBuf,
    mise: PathBuf,
    rustup: PathBuf,
    rustup_home: PathBuf,
    /// Fetches the Gradle wrapper checksum; injectable for offline tests.
    http: Arc<dyn patchula_http::Transport>,
    /// Serialises tool usage records within this process.
    usage: Arc<std::sync::Mutex<()>>,
    /// The user's mise data directory, whose installs seed the tool store.
    seed: Option<PathBuf>,
    /// Held by the first nix call while it installs nix, so concurrent
    /// lookups neither install it twice nor clear each other's install.
    nix_install: Arc<tokio::sync::Mutex<()>>,
    /// The persistent tool caches' size cap in GiB; 0 turns them off.
    cache_gib: u64,
    /// The `installation-<id>` partition's id for runs that hold no
    /// operator credentials but are not provably public-only: the hash of
    /// the GitHub credentials scope on the server, `local` for the CLI.
    installation: String,
    /// Bounds the lockfile generations running at once across every
    /// repository a run writes: one per core, up to four, as the tools are
    /// CPU-bound and each batch of another installation runs its own.
    runs: Arc<tokio::sync::Semaphore>,
}

/// How the token a lockfile tool authenticates to GitHub with was
/// produced — reported by the tool's run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolTokenKind {
    /// A GitHub App installation token minted for one repository, with
    /// `metadata: read` only.
    Scoped,
    /// The run's own GitHub token, reused as Renovate hands its token to
    /// tools.
    RunToken,
}
/// The GitHub token a lockfile tool runs with. Deliberately not Debug, and
/// the token itself is private: the only way to hand it to a tool is
/// [`grant`](Self::grant), which also puts it on the run's `secrets`, so a
/// failure can never repeat it.
pub struct ToolToken {
    token: String,
    pub kind: ToolTokenKind,
}
impl ToolToken {
    /// Give the tool the token as `GH_TOKEN`, and mask it in the run's
    /// output.
    pub fn grant(self, env: &mut Vec<(String, String)>, secrets: &mut Vec<String>) {
        secrets.push(self.token.clone());
        env.push(("GH_TOKEN".to_owned(), self.token));
    }
}

/// How many lockfile generations the runner works at once, wherever they
/// come from in the batch.
fn lockfile_concurrency() -> usize {
    std::thread::available_parallelism().map_or(1, |cores| cores.get().min(4))
}

/// The persistent tool caches' default size cap in GiB.
pub const DEFAULT_CACHE_GIB: u64 = 20;
impl Runner {
    pub fn new(cache: PathBuf, token: &str) -> Self {
        let home = std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from);
        let mise_data = tools::tools_dir(&cache);
        let mise = tools::find_mise(&mise_data).unwrap_or_else(|| "mise".into());
        let rustup = std::env::var_os("CARGO_HOME")
            .map_or_else(|| home.join(".cargo"), PathBuf::from)
            .join("bin/rustup");
        let rustup = if rustup.is_file() {
            rustup
        } else {
            "rustup".into()
        };
        let rustup_home =
            std::env::var_os("RUSTUP_HOME").map_or_else(|| home.join(".rustup"), PathBuf::from);
        Self {
            credentials: None,
            registries: Vec::new(),
            entries: Vec::new(),
            cache,
            token: token.into(),
            executor: Arc::new(SystemExecutor),
            mise_data,
            mise,
            rustup,
            rustup_home,
            http: Arc::new(NetworkTransport),
            usage: Default::default(),
            seed: None,
            nix_install: Default::default(),
            cache_gib: DEFAULT_CACHE_GIB,
            installation: "local".into(),
            runs: Arc::new(tokio::sync::Semaphore::new(lockfile_concurrency())),
        }
    }
    /// Borrow tool versions from this mise data directory, usually the user's
    /// own, instead of downloading them. Local runs only: the server keeps a
    /// store of its own.
    pub fn with_seed(mut self, mise_data: PathBuf) -> Self {
        self.seed = Some(mise_data);
        self
    }
    pub fn with_http(mut self, http: Arc<dyn patchula_http::Transport>) -> Self {
        self.http = http;
        self
    }
    pub fn with_executor(mut self, executor: Arc<dyn Executor>) -> Self {
        self.executor = executor;
        self
    }
    /// Version lookups through the pinned mise, in this runner's tool store and
    /// through its executor.
    pub fn mise_lookup(&self) -> Arc<patchula_mise::Lookup> {
        let defaults = tool_defaults().expect("embedded tool defaults parse");
        let version = defaults["tools"]["mise"]
            .as_str()
            .expect("embedded defaults pin mise");
        Arc::new(patchula_mise::Lookup::new(
            self.executor.clone(),
            self.mise.clone(),
            self.mise_data.clone(),
            version.into(),
        ))
    }
    pub fn with_credentials(mut self, credentials: Arc<dyn patchula_github::Credentials>) -> Self {
        // The installation's own partition, for runs that are not provably
        // public-only but hold no operator credentials.
        self.installation = hash(credentials.scope().as_bytes());
        self.credentials = Some(credentials);
        self
    }
    /// The GitHub token this run fetches with: the installation's
    /// credentials when it has them, else the runner's own.
    async fn github_token(&self) -> Result<String, String> {
        match &self.credentials {
            Some(credentials) => credentials
                .token()
                .await
                .map_err(|_| "GitHub authentication failed".to_owned()),
            None => Ok(self.token.clone()),
        }
    }
    /// The GitHub token the actions.lock tool authenticates to GitHub with
    /// for `repository` (`owner/name`), and how it was produced: a
    /// repository-scoped `metadata: read` token where the run's credentials
    /// can mint one — a GitHub App installation — else the run's own token,
    /// as Renovate hands its token to tools.
    pub async fn actions_lock_token(&self, repository: &str) -> Result<ToolToken, String> {
        if let Some(credentials) = &self.credentials
            && let Some(token) = credentials.scoped_token(repository)
        {
            return Ok(ToolToken {
                token: token
                    .await
                    .map_err(|_| "GitHub authentication failed".to_owned())?,
                kind: ToolTokenKind::Scoped,
            });
        }
        Ok(ToolToken {
            token: self.github_token().await?,
            kind: ToolTokenKind::RunToken,
        })
    }
    /// Set the persistent tool caches' size cap in GiB; 0 turns them off.
    pub fn set_cache_limit(&mut self, cache_gib: u64) {
        self.cache_gib = cache_gib;
    }
    /// Give lockfile tools the credentials of the entries that allow it
    /// (`tools = true`), among all of `registries`.
    pub fn with_registries(mut self, registries: &[patchula_http::Credential]) -> Self {
        self.entries = registries.to_vec();
        self.registries = registries
            .iter()
            .filter(|entry| entry.tools() && entry.auth.sends() && entry.mirror_of().is_none())
            .cloned()
            .collect();
        self
    }
    /// One branch's lockfile work: check `work`'s tree out, run its jobs'
    /// tools in it and answer the files that differ from base.
    async fn generate(&self, work: &Work<'_>, config: &Config) -> Result<Generated, Failure> {
        // Repositories write side by side, and their plans too: the permit
        // is the batch-wide cap on concurrent tool runs.
        let _run = self
            .runs
            .clone()
            .acquire_owned()
            .await
            .expect("the run semaphore is never closed");
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        self.prune_caches();
        let run = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(&self.cache)
            .map_err(|e| e.to_string())?;
        let root = run.path().join("tree");
        let driver = run.path().join("driver");
        std::fs::create_dir_all(&driver).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let (baseline, cargo_configs) = self.checkout(work, &root, &driver).await?;
        // Backstop for the engine's refusal: every file the tools can
        // read and the engine would check was checked before any of them
        // runs, so a committed credential can never fetch into the shared
        // public cache partition. Paths the repository excludes are not
        // checked, as the engine's are not.
        let scanner = patchula_manager::Scanner::new(config)?;
        for (path, text) in baseline
            .iter()
            .map(|(path, blob)| (path, blob.content.as_str()))
            .chain(
                work.edits
                    .iter()
                    .chain(cargo_configs.iter())
                    .map(|(path, text)| (path, text.as_str())),
            )
            // Source files are checked out so tools can build, but no tool
            // reads registry credentials from them; a test's connection
            // string is not a registry credential.
            .filter(|(path, _)| scanner.credential_checked(path))
        {
            if let Some(kind) = patchula_manager::committed_credentials(path, text) {
                return Err(patchula_manager::credential_reason(path, &kind).into());
            }
        }
        for (path, content) in work.edits {
            write(&root, path, content)?;
        }
        let groups: BTreeSet<_> = work
            .jobs
            .iter()
            .map(|j| (j.ecosystem, j.directory.clone()))
            .collect();
        let mut warnings = Vec::new();
        for (ecosystem, directory) in groups {
            let tool = descriptor(ecosystem);
            let versions = if tool.versions {
                versions(&baseline, &directory)?
            } else {
                BTreeMap::new()
            };
            let jobs: Vec<_> = work
                .jobs
                .iter()
                .filter(|j| j.ecosystem == ecosystem && j.directory == directory)
                .collect();
            // Whether every file the run can read decided public-only, so
            // its tools may share the `public` cache partition. Fail
            // closed: any file without a verdict makes it not public.
            let public = public_only(tool, &baseline, work.public_only);
            let context = context::Context {
                driver: &driver,
                root: &root,
                baseline: &baseline,
                directory: &directory,
                config,
                versions: &versions,
                maintenance: work.maintenance,
                public,
            };
            match ecosystem {
                Ecosystem::Node => self.node(context).await?,
                Ecosystem::Cargo => {
                    // Cargo runs outside the repository and reads none of its
                    // configuration. It gets each credentialed registry the
                    // `.cargo/config.toml` files at or above the project
                    // define, by name, from the environment, and the
                    // directory's committed publish-age cooldown alongside.
                    self.cargo_locks(
                        context,
                        &jobs,
                        &cargo_configs,
                        work.cargo_cooldowns,
                        &mut warnings,
                    )
                    .await?;
                }
                Ecosystem::Go => self.go_locks(context, &jobs).await?,
                Ecosystem::Terraform => self.terraform_lock(context).await?,
                Ecosystem::Mise => self.mise_lock(context, &jobs).await?,
                Ecosystem::Python => self.python_locks(context, &jobs).await?,
                Ecosystem::GradleWrapper => self.gradle_wrapper(context).await?,
                Ecosystem::Dotnet => {
                    let edited = work.edits.keys().any(|p| {
                        context.directory.is_empty()
                            || p.starts_with(&format!("{}/", context.directory))
                    }) && !context.maintenance;
                    self.dotnet_restore(context, edited).await?;
                }
                Ecosystem::Ruby => self.bundle_lock(context, &jobs).await?,
                Ecosystem::Php => self.composer_lock(context, &jobs).await?,
                Ecosystem::Nix => self.flake_lock(context, &jobs).await?,
                Ecosystem::Apm => self.apm_lock(context).await?,
                Ecosystem::Actions => self.actions_lock(context, work).await?,
            }
        }
        // Only manifests and lockfiles can enter the API commit. Reject links,
        // unexpected writes and binary output rather than silently publishing it.
        let generated = collect(&root)?;
        validate_targets(work, config, &generated)?;
        let mut files = BTreeMap::new();
        let all: BTreeSet<_> = baseline.keys().chain(generated.keys()).cloned().collect();
        for path in all {
            let before = baseline.get(&path);
            let after = generated.get(&path);
            if before.map(|b| &b.content) == after {
                continue;
            }
            if !writable(&path) && work.edits.get(&path) != after {
                return Err(format!("tool modified unexpected file {path}").into());
            }
            if path.rsplit('/').next() == Some("mise.lock")
                && !work
                    .jobs
                    .iter()
                    .any(|j| j.ecosystem == Ecosystem::Mise && j.directory == directory(&path))
            {
                return Err(format!("tool modified unexpected mise lockfile {path}").into());
            }
            files.insert(
                path,
                GeneratedFile {
                    base: before.cloned(),
                    content: after.cloned(),
                },
            );
        }
        Ok(Generated { files, warnings })
    }
    /// Evict aged and oversized tool-cache partitions, at most once an hour
    /// per cache root per process, without holding the generation up. Lock
    /// files outlive the partitions they guard.
    fn prune_caches(&self) {
        let root = self.cache.join(caches::ROOT);
        if !caches::due(&root) {
            return;
        }
        let cap = self.cache_gib.saturating_mul(1 << 30);
        tokio::task::spawn_blocking(move || {
            if let Err(error) = caches::prune(&root, cap, std::time::SystemTime::now()) {
                tracing::warn!(%error, "tool cache pruning failed");
            }
        });
    }
    /// The tool's persistent cache partition and its environment, held for the
    /// run. A run's partition is `public` unless it receives registry
    /// credentials, in which case their scopes name it.
    async fn use_caches(
        &self,
        call: &Invocation,
    ) -> Result<(Vec<(String, String)>, Option<caches::Hold>), String> {
        if self.cache_gib == 0 {
            return Ok((Vec::new(), None));
        }
        let Some(cache) = call.bin.cache() else {
            return Ok((Vec::new(), None));
        };
        let mut scopes = call.credentials.clone();
        scopes.sort_unstable();
        scopes.dedup();
        // Credentialed runs partition on the credential set as before.
        // Uncredentialed runs share `public` only when every file they can
        // read decided public-only; anything else gets the installation's
        // own partition, so a run that might fetch private content never
        // leaves it where other installations could pick it up.
        let partition = if !scopes.is_empty() {
            caches::Partition::private(&scopes)
        } else if call.public {
            caches::Partition::Public
        } else {
            caches::Partition::Installation(self.installation.clone())
        };
        let root = self.cache.join(caches::ROOT);
        let call_env = call.env.clone();
        let bin = call.bin;
        let (env, hold) = tokio::task::spawn_blocking(move || {
            caches::acquire(&root, &partition, bin, cache, &call_env)
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok((env, Some(hold)))
    }
    async fn tool(&self, driver: &Path, call: Invocation) -> Result<Vec<u8>, Failure> {
        let mut spec = self.command(driver);
        // A first use installs the tool inside this call, from scratch.
        if !self.installed(&call.tools) {
            {
                let _store = tools::lock_store(&self.mise_data, true).await?;
                if !self.installed(&call.tools) {
                    self.clear_unproven(&call.tools).await;
                    self.seed(&call.tools);
                }
            }
            spec.timeout = tools::INSTALL_TIMEOUT;
        }
        let _store = tools::lock_store(&self.mise_data, false).await?;
        if let Some(cwd) = &call.cwd {
            spec.cwd.clone_from(cwd);
        }
        // A tool whose answer is JSON is read whole.
        if call.descriptor.output == Output::Complete {
            spec.output_limit = 1 << 20;
            spec.complete_output = true;
        }
        for (key, value) in &call.env {
            spec.env.insert(key.into(), value.into());
        }
        let (cache_env, _caches) = self.use_caches(&call).await?;
        for (key, value) in cache_env {
            spec.env.insert(key.into(), value.into());
        }
        spec.args.push("exec".into());
        for (tool, version) in &call.tools {
            spec.args.push(format!("{tool}@{version}"));
        }
        spec.args.push("--".into());
        match call.descriptor.run {
            Run::Rustup(toolchain) => {
                let version = call
                    .tools
                    .iter()
                    .find(|(tool, _)| tool.as_str() == toolchain)
                    .map(|(_, version)| version.clone())
                    .ok_or("missing tool version")?;
                spec.env
                    .insert("RUSTUP_HOME".into(), self.rustup_home.as_os_str().into());
                let cargo_home = spec
                    .env
                    .get(std::ffi::OsStr::new("CARGO_HOME"))
                    .ok_or("missing isolated Cargo home")?
                    .clone();
                let install_home = self.cache.join("tool-cargo");
                std::fs::create_dir_all(&install_home).map_err(|e| e.to_string())?;
                spec.env
                    .insert("CARGO_HOME".into(), install_home.into_os_string());
                let rustc = self.rustup.with_file_name("rustc");
                if rustc.is_file() {
                    spec.env.insert("RUSTC".into(), rustc.into_os_string());
                }
                spec.args.extend([
                    "env".into(),
                    format!("CARGO_HOME={}", cargo_home.to_string_lossy()),
                    self.rustup.to_string_lossy().into_owned(),
                    "run".into(),
                    version,
                    call.bin.name().to_owned(),
                ]);
            }
            Run::Direct => spec.args.push(call.bin.name().into()),
            Run::Shell => spec.args.push("/bin/sh".into()),
        }
        spec.args.extend(call.args);
        spec.secrets = call.secrets;
        let context = call.bin.name().to_owned();
        let output = self
            .executor
            .run(spec)
            .await
            .map_err(|failure| Failure::Tool { context, failure })?;
        self.record_use(&call.tools);
        Ok(output)
    }
    /// Run a tool that may hold registry credentials. When it does, a failure
    /// shows none of its output, which may quote what a private registry
    /// answered: failure reasons reach reports, dashboards and public pages.
    async fn held_tool(&self, driver: &Path, call: Invocation) -> Result<Vec<u8>, Failure> {
        let bin = call.bin.name();
        let held = !call.credentials.is_empty();
        let result = self.tool(driver, call).await;
        if !held {
            return result;
        }
        result.map_err(|_| {
            Failure::Text(format!(
                "{bin} failed while it held registry credentials, so its output is not shown"
            ))
        })
    }
}

impl Generate for Runner {
    fn generate<'a>(
        &'a self,
        work: &'a Work<'a>,
        config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = Result<Generated, Failure>> + Send + 'a>> {
        Box::pin(self.generate(work, config))
    }
}

/// One ecosystem tool invocation: the ecosystem's descriptor, the mise
/// tools it needs, the binary to run and the arguments to pass.
#[derive(Clone)]
struct Invocation {
    /// The ecosystem the call belongs to: its output and toolchain
    /// behavior, not the binary's name, decide how it runs.
    descriptor: &'static Tool,
    tools: Vec<(String, String)>,
    bin: caches::Bin,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    /// The `Credential::scope()`s of the registry entries this run receives.
    /// They decide its cache partition and whether a failure shows its output.
    credentials: Vec<String>,
    /// Every secret value the run was given, in every form: a failure must
    /// never repeat one.
    secrets: Vec<String>,
    /// Whether the run can fetch only from its ecosystem's well-known
    /// public registry, which lets it share the `public` cache partition
    /// when it holds no operator credentials.
    public: bool,
    args: Vec<String>,
}

fn validate_targets(
    work: &Work<'_>,
    config: &Config,
    files: &BTreeMap<String, String>,
) -> Result<(), String> {
    if work.targets.is_empty() {
        return Ok(());
    }
    let extraction =
        patchula_manager::Scanner::new(config)?.extract(&patchula_manager::all_read(files))?;
    let paths: BTreeSet<String> = files.keys().cloned().collect();
    for target in &work.targets {
        let leaf = target.file.rsplit('/').next().unwrap_or("");
        // A lockfile-only change must have moved the lock to its exact target.
        if lock_names(Manager::Cargo).supported.contains(&leaf) {
            let lock = files
                .get(target.file)
                .and_then(|text| toml_edit::Document::parse(text.as_str()).ok());
            let moved = lock.as_ref().is_some_and(|lock| {
                lock.get("package")
                    .and_then(toml_edit::Item::as_array_of_tables)
                    .is_some_and(|packages| {
                        packages.iter().any(|p| {
                            p.get("name").and_then(toml_edit::Item::as_str) == Some(target.package)
                                && p.get("source")
                                    .and_then(toml_edit::Item::as_str)
                                    .is_some_and(patchula_manager::crates_io)
                                && p.get("version")
                                    .and_then(toml_edit::Item::as_str)
                                    .zip(target.version)
                                    .is_some_and(|(locked, requested)| {
                                        same_release(locked, requested)
                                    })
                        })
                    })
            });
            if !moved {
                return Err(format!(
                    "lockfile did not resolve {} to requested version {}",
                    target.package,
                    target.version.unwrap_or("?")
                ));
            }
            continue;
        }
        // A manifest proves its declared target only for managers whose
        // ecosystem's descriptor says `validate_targets` reads declarations
        // back — the same mapping `job()` dispatches through.
        let manifest = Manager::ALL.iter().any(|manager| {
            ecosystem(*manager).is_some_and(|e| {
                matches!(
                    descriptor(e).proof,
                    Proof::Declared | Proof::DeclaredAndResolved
                )
            }) && manifest_names(*manager).contains(&leaf)
        });
        if !manifest && !target.file.ends_with(".tf") {
            continue;
        }
        // A lockfile-only change leaves the declaration as it was, so the lock
        // alone has to show the target.
        let declared = match (target.value, target.version) {
            (Some(value), _) => value,
            (None, Some(_)) => target.current,
            (None, None) => continue,
        };
        // The registry is left out: it records where the lock says the package
        // came from, which the tool's own rewrite may state differently.
        let Some(dep) = extraction.dependencies.iter().find(|d| {
            d.file == target.file && d.package == target.package && d.current == *declared
        }) else {
            return Err(format!(
                "tool did not retain requested target {} in {}",
                target.package, target.file
            ));
        };
        // pnpm and Yarn locks are not read back (aube-lock.yaml is), so the unchanged-lockfile rule
        // checks them instead. Every lock this runner reads must prove the target.
        let unread = matches!(dep.manager, Manager::Npm | Manager::Bun)
            && nearest_dir(&paths, directory(target.file), &node_locks()).is_some_and(|dir| {
                ![Manager::Npm, Manager::Bun]
                    .iter()
                    .flat_map(|manager| lock_names(*manager).readable.iter().copied())
                    .any(|leaf| paths.contains(&context::join(dir, leaf)))
            });
        if !unread
            && (ecosystem(dep.manager)
                .is_some_and(|e| descriptor(e).proof == Proof::DeclaredAndResolved)
                || (dep.source == Source::Terraform
                    && dep.kind == patchula_words::DependencyKind::Provider))
            && let Some(version) = target.version
            && !dep
                .resolved
                .as_deref()
                .is_some_and(|resolved| same_release(resolved, version))
        {
            return Err(format!(
                "lockfile did not resolve {} to requested version {version}",
                target.package
            ));
        }
    }
    Ok(())
}

fn versions(
    baseline: &BTreeMap<String, BaseFile>,
    directory: &str,
) -> Result<BTreeMap<String, String>, String> {
    let defaults = tool_defaults()?;
    let mut versions: BTreeMap<String, String> = BTreeMap::new();
    for tool in descriptors()
        .flat_map(|descriptor| descriptor.tools.iter().copied())
        .collect::<BTreeSet<_>>()
    {
        versions.insert(
            tool.into(),
            defaults["tools"][tool]
                .as_str()
                .ok_or("invalid tool defaults")?
                .into(),
        );
    }
    // mise reads `.tool-versions` below `mise.toml` and `.mise.toml` in the
    // same directory, and a deeper file over a shallower one; `upward`
    // orders the same way, so applying in order leaves the winner last.
    for path in upward(
        baseline,
        directory,
        &[".tool-versions", "mise.toml", ".mise.toml"],
    ) {
        let blob = &baseline[path];
        for (name, pin) in mise::pins(path, &blob.content)? {
            let Some(slot) = versions.get_mut(name.as_str()) else {
                continue;
            };
            let version = match &pin {
                // `.tool-versions` pins the first word, as it always did.
                mise::Pin::Words(words) => words.first().map(String::as_str),
                mise::Pin::Spec(spec) => spec
                    .as_str()
                    .or_else(|| spec.get("version").and_then(toml_edit::Item::as_str)),
            };
            let Some(version) = version else {
                if matches!(pin, mise::Pin::Spec(_)) {
                    return Err(format!("{name}: expected a version string or table"));
                }
                continue;
            };
            check_tool_version(&name, version)?;
            *slot = version.into();
        }
    }
    Ok(versions)
}
/// A repository's pin for a tool the runner knows, accepted only as a plain
/// version spec.
fn check_tool_version(tool: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        return Err(format!("{tool}: unsupported tool version"));
    }
    Ok(())
}
/// The pinned tool versions used when a repository does not pin its own,
/// parsed once: the file is embedded, so it cannot change between calls.
fn tool_defaults() -> Result<&'static toml_edit::Document<&'static str>, String> {
    static DEFAULTS: std::sync::OnceLock<Result<toml_edit::Document<&'static str>, String>> =
        std::sync::OnceLock::new();
    DEFAULTS
        .get_or_init(|| {
            toml_edit::Document::parse(include_str!("lockfiles/defaults.toml"))
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}
/// A Cargo configuration file, read only for its `registries`.
fn cargo_config(path: &str) -> bool {
    patchula_manager::roles::cargo_config(path)
}
fn writable(path: &str) -> bool {
    patchula_manager::roles::roles(path).writable
}
fn materialized(path: &str) -> bool {
    let roles = patchula_manager::roles::roles(path);
    roles.writable || roles.read
}
/// The directory whose `.cargo` holds `path`, a Cargo configuration file.
fn directory_of_cargo_config(path: &str) -> &str {
    directory(directory(path))
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c,std::path::Component::Normal(s) if s != ".git"))
}
fn write(root: &Path, path: &str, content: &str) -> Result<(), String> {
    if !safe_path(path) {
        return Err("unsafe repository path".into());
    }
    let target = root.join(path);
    std::fs::create_dir_all(target.parent().ok_or("missing parent")?).map_err(|e| e.to_string())?;
    std::fs::write(target, content).map_err(|e| e.to_string())
}
fn collect(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if matches!(
                entry.file_name().to_str(),
                Some(
                    "node_modules"
                        | ".terraform"
                        | "target"
                        | ".git"
                        | ".yarn"
                        | ".aube"
                        | "apm_modules"
                )
            ) {
                continue;
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("tool produced a symbolic link".into());
            }
            if kind.is_dir() {
                walk(root, &path, files)?;
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .to_str()
                    .ok_or("non UTF-8 path")?
                    .to_owned();
                if entry.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
                    return Err("generated file exceeds 16 MiB".into());
                }
                files.insert(
                    relative,
                    std::fs::read_to_string(path).map_err(|e| e.to_string())?,
                );
            } else {
                return Err("tool produced a special file".into());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests;

/// The default checksum transport: a network client built on first use, so a
/// runner that never touches a Gradle wrapper needs no network setup.
struct NetworkTransport;
impl patchula_http::Transport for NetworkTransport {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        Box::pin(async move {
            static NETWORK: std::sync::OnceLock<Option<patchula_http::Network>> =
                std::sync::OnceLock::new();
            let network = NETWORK
                .get_or_init(|| patchula_http::Network::new().ok())
                .as_ref()
                .ok_or(patchula_http::Error::Configuration)?;
            network.send(request).await
        })
    }
}
