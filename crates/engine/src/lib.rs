//! Batch lookup and branch-writing engine shared by CLI and future server callers.
#![forbid(unsafe_code)]
mod catalogs;
mod lookup;
mod overrides;
mod report;
pub mod repository;
pub mod settings;
#[cfg(test)]
mod state_parity;
mod sweep;
mod write;

use futures_util::{StreamExt, stream};
use patchula_cache::{Cache, State};
use patchula_config::{Config, PackageId};
use patchula_datasource::{Client as Sources, Failure};
use patchula_github::{Client as Github, Options as GithubOptions, model};
use patchula_http::{Cached, MAX_CONCURRENCY, PhaseTrace, Transport};
use patchula_lookup::{Evaluation, Evaluator};
use patchula_manager::{Dependency, Extraction, Scanner};
use patchula_report::{
    Current, Dependency as ReportDependency, LookupFailure, PackageFile, Report, Repository,
    SCHEMA_VERSION, Status,
};
use patchula_words::{
    DependencyKind, LookupErrorKind, Recheck, ResolutionKind, Source, Update as UpdateKind,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Configuration(String),
    #[error(transparent)]
    Github(#[from] patchula_github::Error),
    #[error(transparent)]
    Cache(#[from] patchula_cache::Error),
    #[error(transparent)]
    Update(#[from] patchula_update::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Report(#[from] patchula_report::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// The stages of a batch that take long enough to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Listing the repositories the token can reach.
    Discovery,
    /// Reading each repository's manifests; counts repositories.
    Extraction,
    /// Reading releases and current digests; counts unique lookups, one per
    /// package and one per action ref whose digest is read, however many
    /// dependencies share them.
    Lookup,
    /// Resolving the digests updates pin to; counts digests.
    Digests,
    /// Regenerating lockfiles for planned branches; counts branches.
    Lockfiles,
    /// Writing one repository's branches and pull requests; the total is its
    /// branch count, and nothing advances.
    Writing,
}

/// Told what a batch is doing as it goes, for a progress display. Calls come
/// from concurrent tasks and must be cheap.
pub trait Progress: Send + Sync {
    /// A stage starts with this many items, or zero when the count is unknown.
    fn stage(&self, stage: Stage, total: usize);
    /// This many more items of the current stage are done.
    fn advance(&self, done: usize);
}

impl Progress for () {
    fn stage(&self, _: Stage, _: usize) {}
    fn advance(&self, _: usize) {}
}
#[derive(Default)]
pub struct Options {
    pub repositories: Vec<String>,
    pub config: Option<Config>,
    pub config_dir: Option<std::path::PathBuf>,
    /// Read one repository from a local directory instead of GitHub. Lookups
    /// still reach registries; nothing is ever written.
    pub local: Option<std::path::PathBuf>,
}
/// How an engine reaches GitHub: a caller's token, or an installation's
/// credentials. The lockfile tools get the same identity.
#[derive(Clone)]
pub enum Auth {
    /// A user token.
    Token(String),
    /// An installation's credentials.
    Credentials(Arc<dyn patchula_github::Credentials>),
}

pub struct Engine {
    lockfiles: Option<patchula_tools::Runner>,
    /// The persistent tool caches' size cap in GiB, applied to the runner.
    lockfile_cache_gib: u64,
    github: Github,
    http: Cached,
    sources: Sources,
    cache: Cache,
    concurrency: usize,
    extraction_hits: AtomicU64,
    recheck: Recheck,
    versions_host_override: Option<bool>,
    /// Whether repositories' `security.advisories` setting may query OSV.dev.
    advisories: bool,
    /// Whether the GitHub identity is an installation's, which discovers
    /// repositories through `installation/repositories` rather than `user/repos`.
    installation: bool,
    /// The server URL hidden in PRs and the dashboard for the CLI to find.
    advertised_server: Option<String>,
    progress: Arc<dyn Progress>,
}
struct Work {
    report: Repository,
    config: Option<Config>,
    /// Extracted dependencies, each with what it needs and what policy proposed.
    dependencies: Vec<Dep>,
    /// Lockfile packages no manifest declares, for advisory checks.
    locked: Vec<patchula_manager::Locked>,
    /// The repository's policy evaluator, once its config is known.
    evaluator: Option<Evaluator>,
    snapshot: patchula_update::Snapshot,
    inventory: Option<patchula_update::Inventory>,
    controls: patchula_update::dashboard::Controls,
    /// [`sweep::hash`] of the lookup-stage report row, before the write
    /// path adds its tooling and warnings; the sweep compares its own.
    lookup_hash: String,
    /// When a `prs.perHour` hold could first lift — the oldest counted
    /// PR's `created_at` plus the hour — set by the write path for
    /// [`Engine::record_quiet`]'s `next_due`.
    per_hour_due: Option<chrono::DateTime<chrono::Utc>>,
}
impl Work {
    /// A repository's work before extraction: its report and config, with
    /// nothing extracted, fetched or read yet.
    fn empty(report: Repository, config: Option<Config>) -> Self {
        Self {
            report,
            config,
            dependencies: Vec::new(),
            locked: Vec::new(),
            evaluator: None,
            snapshot: patchula_update::Snapshot::default(),
            inventory: None,
            controls: patchula_update::dashboard::Controls::default(),
            lookup_hash: String::new(),
            per_hour_due: None,
        }
    }
}
/// One extracted dependency plus what the lookup stages made of it.
struct Dep {
    /// The dependency as extraction reported it.
    dependency: Dependency,
    needs: Needs,
    evaluation: Evaluation,
}
impl Dep {
    fn new(dependency: Dependency) -> Self {
        Self {
            dependency,
            needs: Needs::default(),
            evaluation: Evaluation::default(),
        }
    }
}
impl std::ops::Deref for Dep {
    type Target = Dependency;
    fn deref(&self) -> &Dependency {
        &self.dependency
    }
}
/// What the batch fetches and checks for one dependency, decided once its
/// evaluator exists and read by every later stage rather than re-derived.
#[derive(Default)]
struct Needs {
    /// The catalog key its versions or forge tags are read under; kept even
    /// when it initiates no fetch, since another dependency may share it.
    catalog_key: Option<LookupKey>,
    /// Its versions are looked up; a forge catalog alone does not count.
    versions: bool,
    /// Its forge-action tag catalog feeds digest pins.
    forge: bool,
    wait_hours: u32,
    /// The OSV query its advisories are read under; whether they are fetched
    /// is the repository's settings' decision, not the query's.
    advisory: Option<patchula_datasource::AdvisoryQuery>,
    current_ref: Option<DigestKey>,
    digest_policy: bool,
    ignored: bool,
    image: bool,
    sboms: bool,
    revalidate: bool,
}
/// Everything the fetch stages produced, read by digest resolution and
/// report assembly.
#[derive(Default)]
struct Lookups {
    /// The batch's catalogs, keyed by versions-host flag and package.
    catalogs: Catalogs,
    /// When each catalog was fetched, and whether it was reused.
    evidence: catalogs::Evidence,
    credentialed: BTreeSet<LookupKey>,
    /// Git ref digests read so far: hints, current refs, proposals.
    digests: BTreeMap<DigestKey, std::result::Result<String, Failure>>,
    advisories:
        BTreeMap<patchula_datasource::AdvisoryQuery, Vec<Arc<patchula_datasource::Advisory>>>,
    image_digests: BTreeMap<DigestKey, std::result::Result<String, Failure>>,
    image_sboms: ImageSboms,
    image_listing: patchula_datasource::AdvisoryListing,
    image_found: patchula_lookup::images::Found,
    osv_failure: Option<String>,
}
type ImageSboms =
    BTreeMap<(String, String, String), std::result::Result<patchula_datasource::ImageSbom, String>>;

/// The SBOM side of one image digest on `platforms`.
fn image_side(
    sboms: &ImageSboms,
    package: &str,
    digest: &str,
    platforms: &[String],
) -> patchula_lookup::images::Side {
    platforms
        .iter()
        .filter_map(|platform| {
            let key = (package.to_owned(), digest.to_owned(), platform.clone());
            Some((platform.clone(), sboms.get(&key)?.clone()))
        })
        .collect()
}
#[derive(Serialize, Deserialize)]
struct CachedExtraction {
    extraction: Extraction,
    snapshot: patchula_update::Snapshot,
}
#[derive(Serialize, Deserialize)]
struct LastRun(i64);
#[derive(Serialize, Deserialize)]
struct BranchHint(String);
#[derive(Serialize, Deserialize)]
struct RepoConfig(Option<Config>);
#[derive(Serialize, Deserialize)]
struct FileExtraction(Extraction);
#[derive(Serialize, Deserialize)]
struct EvaluatedConfig(Config);
#[derive(Serialize)]
pub(crate) struct CredentialScope {
    repository: String,
    credential_scope: String,
}
#[derive(Serialize)]
struct LookupHintScope {
    repository: String,
    inputs_hash: String,
    schema_hash: String,
    credential_scope: String,
}
#[derive(Serialize)]
struct RepoConfigScope {
    repository: String,
    commit: String,
    schema_hash: String,
}
#[derive(Serialize)]
struct ExtractionScope {
    repository: String,
    commit: String,
    config_hash: String,
}
#[derive(Serialize)]
struct FilesScope {
    repository: String,
    files_hash: String,
    config_hash: String,
}
#[derive(Serialize)]
struct ConfigFilesScope {
    repository: String,
    files_hash: String,
    schema_hash: String,
}
impl State for LastRun {
    const NAMESPACE: &'static str = "last-run";
    const VERSION: u32 = 1;
    type Scope = patchula_cache::RepositoryScope;
}
impl State for LookupHint {
    const NAMESPACE: &'static str = "lookup-hint";
    const VERSION: u32 = 2;
    // Not code-versioned: a hint only names what to prefetch. A stale one
    // costs a wasted read — `resolve_digests` re-reads or overrides it —
    // never a wrong answer, so `VERSION` covers the shape alone and a
    // release keeps the entries.
    type Scope = LookupHintScope;
}
impl State for BranchHint {
    const NAMESPACE: &'static str = "branch-hint";
    const VERSION: u32 = 1;
    // Not code-versioned for the same reason as `LookupHint`: a stale
    // default-branch guess costs one re-read of the authoritative branch.
    type Scope = CredentialScope;
}
impl State for RepoConfig {
    const NAMESPACE: &'static str = "config";
    const VERSION: u32 = 2;
    const CODE_VERSIONED: bool = true;
    type Scope = RepoConfigScope;
}
impl State for CachedExtraction {
    const NAMESPACE: &'static str = "extract";
    const VERSION: u32 = 3;
    const CODE_VERSIONED: bool = true;
    type Scope = ExtractionScope;
}
impl State for FileExtraction {
    const NAMESPACE: &'static str = "files";
    const VERSION: u32 = 3;
    const CODE_VERSIONED: bool = true;
    type Scope = FilesScope;
}
impl State for EvaluatedConfig {
    const NAMESPACE: &'static str = "config-files";
    const VERSION: u32 = 2;
    const CODE_VERSIONED: bool = true;
    type Scope = ConfigFilesScope;
}
type Entry = model::TreeEntry;

/// A catalog: whether the versions host may stand in, and the package.
type LookupKey = (bool, PackageId);
/// An image digest to resolve: the image and its reference.
type DigestKey = (String, String);
/// A package whose digest, image or SBOM read carried credentials, so the
/// report can mark what was found private.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Sent {
    /// A git ref's commit, read through `Sources::digest`.
    Git(String),
    /// A container image's tag digest, manifest config or SBOM.
    Image(String),
}
impl Sent {
    /// The key `dep`'s credentialed reads are recorded and checked under.
    fn of(dep: &Dependency) -> Self {
        if matches!(dep.source, Source::Docker | Source::Oci) {
            Self::Image(dep.package.clone())
        } else {
            Self::Git(dep.package.clone())
        }
    }
}
type Catalogs = BTreeMap<LookupKey, std::result::Result<patchula_datasource::Catalog, Failure>>;
#[allow(dead_code)]
mod repository_head {
    use graphql_client::GraphQLQuery;

    /// `GitObjectID` scalar the generated types resolve as
    /// `super::GitObjectID`.
    type GitObjectID = String;

    /// `repository-head.graphql`, checked against the vendored GitHub
    /// schema. `repository` and `defaultBranchRef` are nullable — a
    /// repository the token cannot see or one with no commits — while
    /// `name`, a commit `target` and its `tree` are non-null once the ref
    /// exists.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/repository-head.graphql",
        response_derives = "Debug"
    )]
    pub struct RepositoryHeadQuery;
}
use repository_head::repository_head_query::{
    RepositoryHeadQueryRepositoryDefaultBranchRefTarget as HeadTarget,
    ResponseData as RepositoryHead,
};

/// The repository-head document, exported for tests that compare request
/// bodies.
pub const REPOSITORY_HEAD_QUERY: &str = repository_head::repository_head_query::QUERY;

#[derive(Clone, Default, Serialize, Deserialize)]
struct LookupHint {
    catalogs: BTreeSet<LookupKey>,
    digests: BTreeSet<DigestKey>,
    /// The mutable tag keys the last run's digest resolution wanted.
    tags: BTreeSet<DigestKey>,
}

impl Engine {
    /// Long-lived installation engine using the same batch path as caller-token runs.
    pub fn with_credentials(
        credentials: Arc<dyn patchula_github::Credentials>,
        cache: Cache,
        concurrency: usize,
        transport: Arc<dyn Transport>,
        http_options: patchula_http::Options,
        github_options: GithubOptions,
    ) -> Result<Self> {
        let tool_registries = http_options.credentials.clone();
        let github = Github::with_credentials(
            credentials.clone(),
            cache.clone(),
            github_options,
            transport.clone(),
        )?;
        let lockfiles = cache.workspace_dir().map(|path| {
            patchula_tools::Runner::new(path.into(), "")
                .with_credentials(credentials)
                .with_registries(&tool_registries)
        });
        let mut engine = Self::build(
            github,
            cache,
            concurrency,
            transport,
            http_options,
            lockfiles,
        )?;
        engine.installation = true;
        Ok(engine)
    }
    /// Settings change only between batches; warm clients and receipts stay intact.
    pub fn set_batch_options(&mut self, concurrency: usize, recheck: Recheck) -> Result<()> {
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(Error::Invalid("concurrency must be 1..100".into()));
        }
        self.concurrency = concurrency;
        self.recheck = recheck;
        Ok(())
    }
    /// The persistent tool caches' size cap in GiB; 0 turns them off.
    pub fn set_lockfile_cache_limit(&mut self, cache_gib: u64) {
        self.lockfile_cache_gib = cache_gib;
        if let Some(runner) = &mut self.lockfiles {
            runner.set_cache_limit(cache_gib);
        }
    }
    /// A server sets this between batches as its settings change.
    pub fn set_advertised_server(&mut self, url: Option<String>) {
        self.advertised_server = url;
    }
    pub fn with_recheck(mut self, recheck: Recheck) -> Self {
        self.recheck = recheck;
        self
    }
    pub fn with_transport(
        token: &str,
        cache: Cache,
        concurrency: usize,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        Self::with_http_options(token, cache, concurrency, transport, Default::default())
    }
    pub fn with_http_options(
        token: &str,
        cache: Cache,
        concurrency: usize,
        transport: Arc<dyn Transport>,
        http_options: patchula_http::Options,
    ) -> Result<Self> {
        let github = Github::with_transport(
            token,
            cache.clone(),
            GithubOptions {
                concurrency: http_options.github_limit(),
                ..Default::default()
            },
            transport.clone(),
        )?;
        let tool_registries = http_options.credentials.clone();
        let lockfiles = cache.workspace_dir().map(|path| {
            patchula_tools::Runner::new(path.into(), token).with_registries(&tool_registries)
        });
        Self::build(
            github,
            cache,
            concurrency,
            transport,
            http_options,
            lockfiles,
        )
    }
    fn build(
        github: Github,
        cache: Cache,
        concurrency: usize,
        transport: Arc<dyn Transport>,
        http_options: patchula_http::Options,
        lockfiles: Option<patchula_tools::Runner>,
    ) -> Result<Self> {
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(Error::Invalid("concurrency must be 1..100".into()));
        }
        let mut http_options = http_options;
        http_options.rules = patchula_datasource::registry_rules(&http_options);
        let http = Cached::with_options(cache.clone(), transport, http_options)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let sources = Sources::new(github.clone(), http.clone(), cache.clone());
        let mut engine = Self {
            lockfiles,
            lockfile_cache_gib: patchula_tools::DEFAULT_CACHE_GIB,
            github,
            http,
            sources,
            cache,
            concurrency,
            extraction_hits: AtomicU64::new(0),
            recheck: Recheck::default(),
            versions_host_override: None,
            advisories: false,
            installation: false,
            advertised_server: None,
            progress: Arc::new(()),
        };
        engine.look_up_mise_tools_with_runner();
        Ok(engine)
    }
    /// Tool versions come from the pinned mise in the lockfile runner's tool
    /// store, through its executor. An in-memory cache has neither.
    fn look_up_mise_tools_with_runner(&mut self) {
        if let Some(runner) = &self.lockfiles {
            self.sources = self
                .sources
                .clone()
                .with_mise(runner.mise_lookup())
                .with_nix(std::sync::Arc::new(RunnerFlakes(runner.clone())));
        }
    }
    pub fn with_lockfile_runner(mut self, mut runner: patchula_tools::Runner) -> Self {
        runner.set_cache_limit(self.lockfile_cache_gib);
        self.lockfiles = Some(runner);
        self.look_up_mise_tools_with_runner();
        self
    }
    /// Seed the lockfile tool store from this mise data directory's installs.
    pub fn with_tool_seed(mut self, mise_data: std::path::PathBuf) -> Self {
        self.lockfiles = self.lockfiles.map(|runner| runner.with_seed(mise_data));
        self
    }
    /// Report each stage of a batch as it runs.
    pub fn with_progress(mut self, progress: Arc<dyn Progress>) -> Self {
        self.progress = progress;
        self
    }
    pub fn with_versions_host(mut self, enabled: bool) -> Self {
        self.versions_host_override = Some(enabled);
        self
    }
    /// Check dependencies against OSV.dev where a repository's config allows
    /// it. Off by default, so offline callers never reach the advisory API.
    pub fn with_advisories(mut self) -> Self {
        self.advisories = true;
        self
    }
    /// The one engine construction the CLI and the server share. Operator
    /// `settings` supply the HTTP options, GitHub options, registry
    /// `credentials` (already resolved for this caller) and the lockfile
    /// cache limit; `auth` is the GitHub identity; `concurrency` and `recheck`
    /// are the batch's current values. Advisory checks are on, as both entry
    /// points run them.
    pub fn from_settings(
        settings: &patchula_config::settings::Settings,
        credentials: Vec<patchula_http::Credential>,
        auth: Auth,
        concurrency: usize,
        recheck: Recheck,
        cache: Cache,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        self::settings::check(settings)
            .map_err(|(_, message)| Error::Configuration(message.to_owned()))?;
        let mut http_options = self::settings::http_options(settings);
        http_options.credentials = credentials;
        let tool_registries = http_options.credentials.clone();
        let github_options = self::settings::github_options(settings);
        let github = match &auth {
            // A token run is the CLI's: it keeps the host limit it always had.
            Auth::Token(token) => Github::with_transport(
                token,
                cache.clone(),
                GithubOptions {
                    concurrency: http_options.github_limit(),
                    write_burst: github_options.write_burst,
                    ..Default::default()
                },
                transport.clone(),
            )?,
            Auth::Credentials(credentials) => Github::with_credentials(
                credentials.clone(),
                cache.clone(),
                github_options,
                transport.clone(),
            )?,
        };
        let lockfiles = cache.workspace_dir().map(|path| {
            let mut runner = match &auth {
                Auth::Token(token) => patchula_tools::Runner::new(path.into(), token),
                Auth::Credentials(credentials) => patchula_tools::Runner::new(path.into(), "")
                    .with_credentials(credentials.clone()),
            };
            // Set before `build` clones the runner into `sources` for Nix
            // lookups, so every copy shares the limit.
            runner.set_cache_limit(settings.lockfiles.cache_gib);
            runner.with_registries(&tool_registries)
        });
        let mut engine = Self::build(
            github,
            cache,
            concurrency,
            transport,
            http_options,
            lockfiles,
        )?;
        engine.installation = matches!(auth, Auth::Credentials(_));
        let mut engine = engine.with_recheck(recheck);
        engine.advisories = true;
        engine.lockfile_cache_gib = settings.lockfiles.cache_gib;
        Ok(engine)
    }
    fn catalog_key(&self, dep: &Dependency, config: Option<&Config>) -> LookupKey {
        let enabled = self
            .versions_host_override
            .unwrap_or_else(|| config.is_none_or(|config| config.use_versions_host));
        let mut id = dep.id();
        // A flake input is looked up by the reference it was written with: the
        // same repository can be followed on different branches.
        if dep.source == Source::Nix {
            id.name.clone_from(&dep.current);
        }
        (enabled, id)
    }
    /// The catalog for `key`, and whether any request made for it carried
    /// an operator entry's credentials or a registry account's token.
    async fn lookup_key(
        sources: &Sources,
        (enabled, id): &LookupKey,
    ) -> (
        std::result::Result<patchula_datasource::Catalog, Failure>,
        bool,
    ) {
        let (result, credentialed) =
            // Boxed: lookups are large futures, and the run holds many.
            patchula_http::tracked(Box::pin(
                sources.clone().with_versions_host(*enabled).lookup(id),
            ))
            .await;
        (result.map_err(Failure::from), credentialed)
    }
    pub async fn lookup(&self, options: Options) -> Result<Report> {
        self.lookup_at(options, chrono::Utc::now()).await
    }
    pub async fn lookup_at(
        &self,
        options: Options,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Report> {
        self.lookup_work_at(options, now, false)
            .await
            .map(|(report, _)| report)
    }
    pub async fn run(
        &self,
        options: Options,
        dry_run: bool,
        allow_lockfile_pending: bool,
    ) -> Result<Report> {
        self.run_at(options, dry_run, allow_lockfile_pending, chrono::Utc::now())
            .await
    }
    pub async fn run_at(
        &self,
        options: Options,
        dry_run: bool,
        allow_lockfile_pending: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Report> {
        if options.local.is_some() && !dry_run {
            return Err(Error::Configuration(
                "a local directory is never written; read it with lookup or run --dry-run".into(),
            ));
        }
        let started = Instant::now();
        let (mut report, work) = self.lookup_work_at(options, now, !dry_run).await?;
        let write_started = Instant::now();
        let before = self.github.stats().await;
        // Each `Work` carries its own report row, so the write pairs them by
        // identity: no positional zip, and the report is rebuilt from the
        // rows the pipeline actually wrote.
        let write = write::Write::new(self, now, dry_run, allow_lockfile_pending);
        // Repositories write side by side — their reads and lockfile tools
        // overlap each other's governor-paced writes — and the actions they
        // return join the report in the same sorted order they would in a
        // sequential pass, so the report is identical.
        //
        // A repository's error does not cancel the others: dropping one
        // mid-write could leave a published branch without its pull request.
        // Every repository runs to the end, and the run fails with the first
        // error in repository order, so which error it reports does not
        // depend on timing.
        let mut finished: Vec<(String, Result<(Work, write::Written)>)> = stream::iter(work)
            .map(|mut work| {
                let write = &write;
                async move {
                    let name = work.report.repository.clone();
                    let done = write.repository(&mut work).await.map(|done| (work, done));
                    (name, done)
                }
            })
            .buffer_unordered(self.concurrency)
            .collect()
            .await;
        finished.sort_by(|a, b| a.0.cmp(&b.0));
        let written: Vec<(Work, write::Written)> = finished
            .into_iter()
            .map(|(_, done)| done)
            .collect::<Result<_>>()?;
        let mut work = Vec::with_capacity(written.len());
        let mut branches = Vec::new();
        let mut pull_requests = Vec::new();
        for (w, mut done) in written {
            work.push(w);
            branches.append(&mut done.branches);
            pull_requests.append(&mut done.pull_requests);
        }
        write.record_runs(&mut work).await;
        report.repositories = work.into_iter().map(|w| w.report).collect();
        branches.sort_by(|a, b| (&a.repository, &a.branch).cmp(&(&b.repository, &b.branch)));
        report.actions = Some(patchula_report::Actions {
            schema_version: patchula_report::ACTIONS_SCHEMA_VERSION,
            dry_run,
            branches,
            pull_requests,
        });
        if let Some(stats) = &mut report.stats {
            let after = self.github.stats().await;
            stats.requests += after.requests - before.requests;
            stats.not_modified += after.not_modified - before.not_modified;
            stats
                .phase_micros
                .insert("write".into(), micros(write_started));
            stats.phase_micros.insert("total".into(), micros(started));
        }
        report.validate()?;
        Ok(report)
    }
    /// When this repository last completed a real run. Schedule windows are
    /// due when one of their times falls between that run and this one.
    pub(crate) async fn last_run(
        &self,
        repo: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let scope = patchula_cache::RepositoryScope {
            repository: repo.into(),
        };
        let run = self.cache.state::<LastRun>(&scope).await?;
        Ok(run.and_then(|run| chrono::DateTime::from_timestamp(run.0, 0)))
    }
    async fn record_run(&self, repo: &str, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
        let scope = patchula_cache::RepositoryScope {
            repository: repo.into(),
        };
        self.cache
            .set_state(&scope, &LastRun(now.timestamp()))
            .await?;
        Ok(())
    }
    async fn lookup_work_at(
        &self,
        options: Options,
        now: chrono::DateTime<chrono::Utc>,
        read_controls: bool,
    ) -> Result<(Report, Vec<Work>)> {
        if options.config.is_some() && options.config_dir.is_some() {
            return Err(Error::Configuration(
                "--config and --config-dir are mutually exclusive".into(),
            ));
        }
        if options.local.is_some() && !options.repositories.is_empty() {
            return Err(Error::Configuration(
                "a local directory is read alone; name no repositories with it".into(),
            ));
        }
        // Packages any of whose digest, image or SBOM reads carried
        // credentials, as their catalogs' lookups record too.
        let sent = std::sync::Mutex::new(BTreeSet::<Sent>::new());
        let sent = &sent;
        let total = Instant::now();
        let _total_span = PhaseTrace::new("total");
        let before_g = self.github.stats().await;
        let before_h = self.http.stats();
        let before_hits = self.extraction_hits.load(Ordering::Relaxed);
        let started = Instant::now();
        let phase = PhaseTrace::new("discovery");
        let discovery = self.discover(&options).await?;
        let discovery_time = micros(started);
        drop(phase);
        let scanned = self
            .scan_all(&discovery, &options, read_controls, sent)
            .await?;
        let extraction_time = scanned.extraction;
        let tail = self.lookup_tail(&discovery, scanned, now, sent).await?;
        let lookups = tail.lookups;
        let work = tail.work;
        let after_g = self.github.stats().await;
        let after_h = self.http.stats();
        let report = Report {
            schema_version: SCHEMA_VERSION,
            repositories: work.iter().map(|w| w.report.clone()).collect(),
            actions: None,
            stats: Some(patchula_report::Stats {
                requests: after_g.requests + after_h.requests
                    - before_g.requests
                    - before_h.requests,
                not_modified: after_g.not_modified + after_h.not_modified
                    - before_g.not_modified
                    - before_h.not_modified,
                cache_hits: after_h.cache_hits - before_h.cache_hits + after_g.not_modified
                    - before_g.not_modified
                    + lookups.evidence.values().filter(|e| e.reused).count() as u64,
                extraction_hits: self.extraction_hits.load(Ordering::Relaxed) - before_hits,
                lookups: lookups.catalogs.len() as u64,
                phase_micros: BTreeMap::from([
                    ("discovery".into(), discovery_time),
                    ("extraction".into(), extraction_time),
                    ("lookup".into(), tail.lookup),
                    ("policy".into(), tail.policy),
                    ("digest".into(), tail.digest),
                    ("image".into(), tail.image),
                    ("total".into(), micros(total)),
                ]),
            }),
        };
        // A `run` attaches `actions` after the writes and validates again;
        // a repository that raced here is `stale` before that section exists.
        report.validate_before_actions()?;
        Ok((report, work))
    }

    /// The catalogs for `keys`, and the keys whose lookups carried
    /// credentials.
    async fn catalogs(
        &self,
        keys: BTreeSet<LookupKey>,
        sources: &Sources,
    ) -> (Catalogs, BTreeSet<LookupKey>) {
        /// Packages one release-catalog batch asks for.
        const RELEASES_PER_BATCH: usize = 16;
        /// Concurrent release-catalog batch reads.
        const RELEASES_FAN_OUT: usize = 16;
        /// Concurrent per-package lookups inside a failed batch.
        const FALLBACK_FAN_OUT: usize = 16;
        /// Independent catalog lookups run this many at once.
        const LOOKUP_FAN_OUT: usize = 100;
        let (releases, others): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|(_, id)| id.source == Source::GithubReleases && id.is_public());
        let mut groups = Vec::new();
        for enabled in [true, false] {
            let packages: Vec<_> = releases
                .iter()
                .filter(|(host, _)| *host == enabled)
                .map(|(_, id)| id.name.clone())
                .collect();
            groups.extend(
                packages
                    .chunks(RELEASES_PER_BATCH)
                    .map(|chunk| (enabled, chunk.to_vec())),
            );
        }
        let batched = stream::iter(groups)
            .map(|(enabled, packages)| async move {
                let client = sources.clone().with_versions_host(enabled);
                let count = packages.len();
                let (batch, credentialed) =
                    patchula_http::tracked(Box::pin(client.release_catalogs(&packages))).await;
                let catalogs: Vec<(LookupKey, (std::result::Result<_, Failure>, bool))> =
                    match batch {
                        Ok(results) => results
                            .into_iter()
                            .map(|(package, result)| {
                                (
                                    (enabled, PackageId::public(Source::GithubReleases, package)),
                                    (result.map_err(Failure::from), credentialed),
                                )
                            })
                            .collect(),
                        Err(_) => {
                            stream::iter(packages)
                                .map(|package| async move {
                                    let key = (
                                        enabled,
                                        PackageId::public(Source::GithubReleases, package),
                                    );
                                    let result = Self::lookup_key(sources, &key).await;
                                    (key, result)
                                })
                                .buffer_unordered(FALLBACK_FAN_OUT)
                                .collect()
                                .await
                        }
                    };
                self.progress.advance(count);
                catalogs
            })
            .buffer_unordered(RELEASES_FAN_OUT)
            .collect::<Vec<_>>();
        let others = stream::iter(others)
            .map(|key| async move {
                let result = Self::lookup_key(sources, &key).await;
                self.progress.advance(1);
                (key, result)
            })
            .buffer_unordered(LOOKUP_FAN_OUT)
            .collect::<Vec<_>>();
        let (groups, others) = tokio::join!(batched, others);
        let mut results = Catalogs::new();
        let mut credentialed = BTreeSet::new();
        for (key, (result, sent)) in others.into_iter().chain(groups.into_iter().flatten()) {
            if sent {
                credentialed.insert(key.clone());
            }
            results.insert(key, result);
        }
        (results, credentialed)
    }
    async fn repositories(&self) -> Result<Vec<String>> {
        // One unreadable entry fails the listing: a silently skipped
        // repository would never be updated and never be reported.
        let repos = if self.installation {
            self.github
                .rest_pages_envelope(
                    "installation/repositories?per_page=100",
                    |body| {
                        serde_json::from_value::<model::InstallationRepositories>(body.clone())
                            .map(|body| body.repositories)
                            .map_err(|_| patchula_github::Error::Json)
                    },
                    |page| page.len() < 100,
                )
                .await?
        } else {
            self.github
                .rest_pages::<model::ListedRepository>("user/repos?per_page=100", |_| false)
                .await?
        };
        Ok(repos.into_iter().map(|repo| repo.full_name).collect())
    }
    async fn tree(&self, repo: &str, sha: &str) -> Result<Vec<Entry>> {
        // A tree named by SHA cannot change; a cached answer needs no
        // revalidation, and a later full write reads the same tree from it.
        let tree = self
            .github
            .get_immutable::<model::Tree>(&format!("repos/{repo}/git/trees/{sha}?recursive=1"))
            .await?;
        if tree.truncated {
            return Err(Error::Invalid(
                "repository tree is truncated; cannot guarantee complete extraction".into(),
            ));
        }
        Ok(tree.tree)
    }
    /// The repository view for `entries`: the shared selection-and-reading
    /// engine, its blob reads answered by GitHub batches — one `blobs`
    /// call per selection and per round of the include chain.
    async fn repository_view(
        &self,
        repo: &str,
        entries: Vec<Entry>,
        config: &Config,
    ) -> Result<repository::View> {
        let listing = repository::Listing {
            entries: entries
                .into_iter()
                .filter(|entry| entry.kind == "blob")
                .map(|entry| repository::Entry {
                    path: entry.path,
                    sha: Some(entry.sha),
                    mode: entry.mode,
                })
                .collect(),
            hidden: Vec::new(),
        };
        let scanner = Scanner::new(config).map_err(Error::Invalid)?;
        let mut source = Remote {
            client: &self.github,
            repo,
        };
        repository::view(&mut source, listing, &scanner, config).await
    }
    async fn file(&self, repo: &str, sha: &str) -> Result<String> {
        String::from_utf8(self.blob(repo, sha).await?)
            .map_err(|_| Error::Invalid("selected file is not UTF-8".into()))
    }
    async fn blob(&self, repo: &str, sha: &str) -> Result<Vec<u8>> {
        self.github
            .blobs(repo, &[sha.to_owned()])
            .await?
            .remove(sha)
            .ok_or_else(|| Error::Invalid("blob missing from the batch".into()))
    }
    async fn scan(&self, repo: &str, override_config: Option<&Config>) -> Result<Work> {
        let hint_scope = CredentialScope {
            repository: repo.into(),
            credential_scope: self.github.credential_scope().into(),
        };
        let hint = self
            .cache
            .state::<BranchHint>(&hint_scope)
            .await?
            .map(|hint| hint.0);
        let (branch, commit, tree_sha) = if let Some(hint) = hint {
            // Conditional REST heads are faster than GraphQL snapshots on the
            // measured warm path. Revalidate metadata and the hinted branch in
            // parallel; a rename retries only the newly authoritative branch.
            let metadata_path = format!("repos/{repo}");
            let (metadata, head) = tokio::join!(
                self.github.get_fresh::<model::Repository>(&metadata_path),
                self.head(repo, &hint)
            );
            let metadata = metadata?;
            let branch = metadata.default_branch;
            let head = if branch == hint {
                head?
            } else {
                self.head(repo, &branch).await?
            };
            let commit = head.commit.sha;
            let tree = head
                .commit
                .commit
                .map(|detail| detail.tree.sha)
                .ok_or_else(|| Error::Invalid("commit has no tree".into()))?;
            (branch, commit, tree)
        } else {
            let (owner, name) = repo.split_once('/').expect("validated repository");
            let data = self
                .github
                .graphql::<RepositoryHead>(
                    REPOSITORY_HEAD_QUERY,
                    serde_json::json!({"owner":owner,"name":name}),
                )
                .await?;
            let head = data
                .repository
                .and_then(|repository| repository.default_branch_ref)
                .ok_or_else(|| Error::Invalid("repository has no default branch".into()))?;
            let Some(HeadTarget::Commit(target)) = head.target else {
                return Err(Error::Invalid("default branch head is not a commit".into()));
            };
            (head.name, target.oid, target.tree.oid)
        };
        self.cache
            .set_state(&hint_scope, &BranchHint(branch.clone()))
            .await?;
        let mut report = repo_report(repo, Status::Complete, None);
        report.default_branch = Some(branch);
        report.commit = Some(commit.clone());
        self.extract(repo, override_config, report, commit, &tree_sha)
            .await
    }
    /// A repository read from disk: the same config discovery, file selection
    /// and Git blob identities as one read through the API.
    async fn scan_local(
        &self,
        dir: &std::path::Path,
        repo: &str,
        override_config: Option<&Config>,
    ) -> Result<Work> {
        let config = match override_config {
            Some(config) => config.clone(),
            None => {
                match patchula_config::discover(dir).map_err(|e| Error::Invalid(e.to_string()))? {
                    Some(path) => tokio::task::spawn_blocking(move || {
                        patchula_config::load(&path).map_err(|e| format!("{e:?}"))
                    })
                    .await
                    .map_err(|e| Error::Invalid(e.to_string()))?
                    .map_err(Error::Invalid)?,
                    None => {
                        return Ok(Work::empty(
                            repo_report(
                                repo,
                                Status::Skipped,
                                Some("no patchula.pkl: the Count visits only repositories that invite him; add one to opt in".into()),
                            ),
                            None,
                        ));
                    }
                }
            }
        };
        let scanner = Scanner::new(&config).map_err(Error::Invalid)?;
        // The local source reads synchronously; the whole view runs on the
        // blocking pool so a large checkout does not stall engine workers.
        let dir = dir.to_owned();
        let view_config = config.clone();
        let (view, scanner) = tokio::task::spawn_blocking(move || {
            futures_executor::block_on(repository::local_view(&dir, &scanner, &view_config))
                .map(|view| (view, scanner))
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))??;
        let repository::View {
            paths,
            files,
            blobs,
            unsupported,
            warnings,
            refusal,
        } = view;
        if let Some(reason) = refusal {
            return Ok(Work::empty(
                repo_report(repo, Status::Error, Some(reason)),
                Some(config),
            ));
        }
        // Extraction is a function of the config and the files it reads, so
        // a checkout that leaves those as they were reuses the earlier
        // result without scanning, keyed as the remote path's is.
        let files_scope = FilesScope {
            repository: repo.into(),
            files_hash: patchula_manager::hash(&serde_json::to_vec(&files)?),
            config_hash: patchula_manager::hash(&serde_json::to_vec(&config)?),
        };
        let mut extraction =
            if let Some(saved) = self.cache.state::<FileExtraction>(&files_scope).await? {
                self.extraction_hits.fetch_add(1, Ordering::Relaxed);
                saved.0
            } else {
                let extraction = tokio::task::spawn_blocking(move || scanner.extract(&files))
                    .await
                    .map_err(|e| Error::Invalid(e.to_string()))?
                    .map_err(Error::Invalid)?;
                let saved = FileExtraction(extraction);
                self.cache.set_state(&files_scope, &saved).await?;
                saved.0
            };
        extraction.skipped.extend(unsupported);
        extraction.warnings.extend(warnings);
        let mut report = repo_report(repo, Status::Complete, None);
        report.warnings = extraction.warnings;
        report.skipped = skipped(&extraction.skipped);
        // Plans are based on a commit; a directory's stands in as a hash of
        // its selected files, stable until they change.
        report.commit = Some(patchula_manager::hash(&serde_json::to_vec(
            &blobs
                .iter()
                .map(|(path, blob)| (path, &blob.sha))
                .collect::<Vec<_>>(),
        )?));
        report.default_branch = Some("local".into());
        Ok(Work {
            dependencies: extraction.dependencies.into_iter().map(Dep::new).collect(),
            locked: extraction.locked,
            snapshot: patchula_update::Snapshot {
                paths,
                files: blobs,
                public_only: extraction.public_only.clone(),
            },
            ..Work::empty(report, Some(config))
        })
    }
    async fn head(&self, repo: &str, branch: &str) -> Result<model::BranchHead> {
        let mut url = self
            .github
            .api_url()
            .join(&format!("repos/{repo}/branches/"))
            .expect("validated repository");
        url.path_segments_mut()
            .expect("https URL")
            .pop_if_empty()
            .push(branch);
        Ok(self.github.get_fresh(url.as_str()).await?)
    }
    async fn extract(
        &self,
        repo: &str,
        override_config: Option<&Config>,
        mut report: Repository,
        commit: String,
        tree_sha: &str,
    ) -> Result<Work> {
        let mut tree = None;
        let config = if let Some(config) = override_config {
            Some(config.clone())
        } else {
            let scope = RepoConfigScope {
                repository: repo.into(),
                commit: commit.clone(),
                schema_hash: patchula_manager::hash(patchula_config::SCHEMA.as_bytes()),
            };
            if let Some(saved) = self.cache.state::<RepoConfig>(&scope).await? {
                saved.0
            } else {
                let entries = self.tree(repo, tree_sha).await?;
                let path = Some(patchula_config::CONFIG_PATH).filter(|p| {
                    entries
                        .iter()
                        .any(|entry| entry.path == *p && entry.kind == "blob")
                });
                let saved = RepoConfig(if let Some(path) = path {
                    Some(self.config(repo, path, &entries).await?)
                } else {
                    None
                });
                self.cache.set_state(&scope, &saved).await?;
                tree = Some(entries);
                saved.0
            }
        };
        let Some(config) = config else {
            report.status = Status::Skipped;
            report.reason = Some("no patchula.pkl: the Count visits only repositories that invite him; add one to opt in".into());
            return Ok(Work::empty(report, None));
        };
        let config_hash = patchula_manager::hash(&serde_json::to_vec(&config)?);
        let scope = ExtractionScope {
            repository: repo.into(),
            commit,
            config_hash: config_hash.clone(),
        };
        let cached: CachedExtraction = if let Some(cached) =
            self.cache.state::<CachedExtraction>(&scope).await?
        {
            self.extraction_hits.fetch_add(1, Ordering::Relaxed);
            cached
        } else {
            let scanner = Scanner::new(&config).map_err(Error::Invalid)?;
            let entries = if let Some(tree) = tree {
                tree
            } else {
                self.tree(repo, tree_sha).await?
            };
            let view = self.repository_view(repo, entries, &config).await?;
            let repository::View {
                paths,
                files,
                blobs,
                unsupported,
                warnings,
                refusal,
            } = view;
            if let Some(reason) = refusal {
                report.status = Status::Error;
                report.reason = Some(reason);
                return Ok(Work::empty(report, Some(config)));
            }
            // Extraction is a function of the config and the files it reads,
            // so a commit that leaves those as they were, whatever else it
            // changes, reuses the earlier result without scanning.
            // Unlike a commit, the same files recur across releases, so the
            // key carries the version as well as the namespace.
            let files_scope = FilesScope {
                repository: repo.into(),
                files_hash: patchula_manager::hash(&serde_json::to_vec(&files)?),
                config_hash: config_hash.clone(),
            };
            let mut extraction =
                if let Some(saved) = self.cache.state::<FileExtraction>(&files_scope).await? {
                    self.extraction_hits.fetch_add(1, Ordering::Relaxed);
                    saved.0
                } else {
                    let extraction = tokio::task::spawn_blocking(move || scanner.extract(&files))
                        .await
                        .map_err(|e| Error::Invalid(e.to_string()))?
                        .map_err(Error::Invalid)?;
                    let saved = FileExtraction(extraction);
                    self.cache.set_state(&files_scope, &saved).await?;
                    saved.0
                };
            extraction.skipped.extend(unsupported);
            extraction.warnings.extend(warnings);
            let public_only = extraction.public_only.clone();
            let cached = CachedExtraction {
                extraction,
                snapshot: patchula_update::Snapshot {
                    paths,
                    files: blobs,
                    public_only,
                },
            };
            self.cache.set_state(&scope, &cached).await?;
            cached
        };
        report.warnings = cached.extraction.warnings;
        report.skipped = skipped(&cached.extraction.skipped);
        Ok(Work {
            dependencies: cached
                .extraction
                .dependencies
                .into_iter()
                .map(Dep::new)
                .collect(),
            locked: cached.extraction.locked,
            snapshot: cached.snapshot,
            ..Work::empty(report, Some(config))
        })
    }
    async fn config(&self, repo: &str, path: &str, entries: &[Entry]) -> Result<Config> {
        // The config's local-module closure is the config crate's walk; the
        // module texts it asks for come from the tree's blobs.
        // Symlinks are never read, as in the view: a linked config module
        // is missing here, not fetched as the link's target text.
        let shas: BTreeMap<String, String> = entries
            .iter()
            .filter(|entry| entry.kind == "blob" && entry.mode != "120000")
            .map(|entry| (entry.path.clone(), entry.sha.clone()))
            .collect();
        let mut read = |module: &str| {
            let module = module.to_owned();
            let shas = &shas;
            async move {
                match shas.get(&module) {
                    Some(sha) => self
                        .file(repo, sha)
                        .await
                        .map(Some)
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                }
            }
        };
        let modules = patchula_config::import_files(
            path,
            patchula_config::SCHEMA_NAME,
            patchula_config::SCHEMA,
            &mut read,
        )
        .await
        .map_err(Error::Invalid)?;
        let files: BTreeMap<String, String> = modules
            .into_iter()
            .map(|(path, text)| (format!("/repo/{path}"), text))
            .collect();
        for (path, text) in &files {
            let path = path.strip_prefix("/repo/").unwrap_or(path);
            if let Some(kind) = patchula_manager::committed_credentials(path, text) {
                return Err(Error::Invalid(patchula_manager::credential_reason(
                    path, &kind,
                )));
            }
        }
        let path = format!("/repo/{path}");
        // Evaluation is a function of the modules read, so the same modules
        // on another commit reuse it.
        let scope = ConfigFilesScope {
            repository: repo.into(),
            files_hash: patchula_manager::hash(&serde_json::to_vec(&(&path, &files))?),
            // CODE_VERSIONED covers the decoding and validation the schema
            // text does not.
            schema_hash: patchula_manager::hash(patchula_config::SCHEMA.as_bytes()),
        };
        if let Some(saved) = self.cache.state::<EvaluatedConfig>(&scope).await? {
            return Ok(saved.0);
        }
        let config = tokio::task::spawn_blocking(move || {
            patchula_config::load_files(&path, files).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?
        .map_err(Error::Invalid)?;
        let saved = EvaluatedConfig(config);
        self.cache.set_state(&scope, &saved).await?;
        Ok(saved.0)
    }
}
/// The remote side of a repository view: each read is one `blobs` batch
/// over the round's entries, answered in their order.
struct Remote<'a> {
    client: &'a Github,
    repo: &'a str,
}

impl repository::Source for Remote<'_> {
    async fn read(&mut self, entries: &[repository::Entry]) -> repository::Contents {
        let shas: Vec<String> = entries
            .iter()
            .filter_map(|entry| entry.sha.clone())
            .collect();
        let contents = self.client.blobs(self.repo, &shas).await?;
        let mut out = Vec::with_capacity(entries.len());
        let mut missing = false;
        for entry in entries {
            match contents.get(entry.sha.as_deref().unwrap_or_default()) {
                Some(bytes) => out.push(Ok(bytes.clone())),
                None => missing = true,
            }
        }
        if missing {
            return Err(Error::Invalid("blob missing from the batch".into()));
        }
        Ok(out)
    }
}
/// `Runner::flake_prefetch` as the datasource's `Flakes`: the tool crate
/// cannot name datasource, so the engine adapts the runner here.
struct RunnerFlakes(patchula_tools::Runner);
impl patchula_datasource::Flakes for RunnerFlakes {
    fn prefetch<'a>(
        &'a self,
        reference: &'a str,
        token: Option<&'a str>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<Vec<u8>, String>> + Send + 'a>,
    > {
        Box::pin(self.0.flake_prefetch(reference, token))
    }
}

/// Mark a repository failed. The first failure of a run names the reason;
/// later ones add nothing, so the cause is never replaced by a consequence.
fn fail(repo: &mut patchula_report::Repository, reason: impl Into<String>) {
    if repo.status != Status::Error {
        repo.status = Status::Error;
        repo.reason = Some(reason.into());
    }
}

/// Mark a repository stale: it changed under the run, so the run stopped
/// before writing against the old view and a rerun retries it. Only a
/// still-complete repository can go stale — a failure outranks a later race.
fn stale(repo: &mut patchula_report::Repository, reason: impl Into<String>) {
    if repo.status == Status::Complete {
        repo.status = Status::Stale;
        repo.reason = Some(reason.into());
    }
}

fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros().min(u64::MAX as u128) as u64
}
fn validate_repo(repo: &str) -> Result<()> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(Error::Invalid(format!("expected owner/repo, got {repo}")));
    };
    if !repo_part(owner) || !repo_part(name) {
        return Err(Error::Invalid("invalid repository identity".into()));
    }
    Ok(())
}
/// Whether `part` can be a repository's owner or name.
fn repo_part(part: &str) -> bool {
    !part.is_empty()
        && !matches!(part, "." | "..")
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// A local directory reports as `local/<directory name>`; a name that is
/// not a valid repository name reports as `local/<hash of the name>` so the
/// identity validates and stays the same on every run.
fn local_name(dir: &std::path::Path) -> String {
    let name = dir
        .canonicalize()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "directory".into());
    if repo_part(&name) {
        format!("local/{name}")
    } else {
        format!("local/{}", patchula_manager::hash(name.as_bytes()))
    }
}
fn lookup_failure(dep: &Dependency, failure: &Failure, message: String) -> LookupFailure {
    LookupFailure {
        manager: dep.manager,
        path: dep.file.clone(),
        name: dep.name.clone(),
        package: dep.package.clone(),
        datasource: dep.source,
        kind: failure.kind,
        message,
    }
}
fn repo_report(repo: &str, status: Status, reason: Option<String>) -> Repository {
    Repository {
        repository: repo.into(),
        status,
        reason,
        package_files: Vec::new(),
        warnings: Vec::new(),
        skipped: Vec::new(),
        notices: Vec::new(),
        lookup_failures: Vec::new(),
        tooling: Vec::new(),
        default_branch: None,
        commit: None,
    }
}
fn skipped(skipped: &[patchula_manager::Skipped]) -> Vec<patchula_report::Skipped> {
    skipped
        .iter()
        .map(|s| patchula_report::Skipped {
            manager: s.manager,
            path: s.file.clone(),
            line: s.line,
            value: s.value.clone(),
            reason: s.reason.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_http::{Exchange, Transport};

    struct Never;
    impl Transport for Never {
        fn send(&self, _: reqwest::Request) -> Exchange<'_> {
            panic!("construction issues no requests")
        }
    }

    /// A token run, the CLI's, paces writes with the operator's
    /// `github.writeBurst`, as a credentialed run does.
    #[tokio::test]
    async fn a_token_run_honours_the_write_burst() {
        let mut settings = patchula_config::settings::Settings::default();
        settings.github.write_burst = 1;
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::from_settings(
            &settings,
            Vec::new(),
            Auth::Token("t".into()),
            4,
            patchula_words::Recheck::NextRun,
            cache,
            Arc::new(Never),
        )
        .unwrap();
        assert_eq!(engine.github.write_burst(), 1);
    }

    /// An extraction one release writes is read by the next: the
    /// namespace carries the guarded version and no build stamp. One
    /// written under the previous version — what the parity guard's bump
    /// produces — is missed.
    #[tokio::test]
    async fn a_saved_extraction_survives_a_release() {
        #[derive(Serialize, Deserialize)]
        struct OldFiles(Extraction);
        impl State for OldFiles {
            const NAMESPACE: &'static str = "files";
            const VERSION: u32 = FileExtraction::VERSION - 1;
            type Scope = FilesScope;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let scope = FilesScope {
            repository: "a/b".into(),
            files_hash: "f".into(),
            config_hash: "c".into(),
        };
        let cache = Cache::open(&path, patchula_cache::Options::default())
            .await
            .unwrap();
        cache
            .set_state(
                &scope,
                &FileExtraction(Extraction {
                    warnings: vec!["current".into()],
                    ..Extraction::default()
                }),
            )
            .await
            .unwrap();
        cache
            .set_state(&scope, &OldFiles(Extraction::default()))
            .await
            .unwrap();
        drop(cache);
        let next = Cache::open(&path, patchula_cache::Options::default())
            .await
            .unwrap();
        let saved = next
            .state::<FileExtraction>(&scope)
            .await
            .unwrap()
            .expect("the current version's entry is read back");
        assert_eq!(saved.0.warnings, ["current"]);
    }
}
