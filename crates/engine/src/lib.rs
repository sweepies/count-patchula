//! Batch lookup and branch-writing engine shared by CLI and future server callers.
#![forbid(unsafe_code)]
mod catalogs;
mod overrides;
pub use patchula_update::Recheck;

use futures_util::{StreamExt, stream};
use patchula_cache::{Cache, ExtractionKey};
use patchula_config::{Config, Manager, Source};
use patchula_datasource::Client as Sources;
use patchula_github::{Client as Github, Options as GithubOptions};
use patchula_http::{Cached, PhaseTrace, Transport};
use patchula_lookup::{Evaluation, Evaluator};
use patchula_manager::{Dependency, Extraction, Files, Scanner, word};
use patchula_report::{
    Current, Dependency as ReportDependency, PackageFile, Report, Repository, SCHEMA_VERSION,
    Status,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Report(#[from] patchula_report::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Default)]
pub struct Options {
    pub repositories: Vec<String>,
    pub config: Option<Config>,
    pub config_dir: Option<std::path::PathBuf>,
    /// Read one repository from a local directory instead of GitHub. Lookups
    /// still reach registries; nothing is ever written.
    pub local: Option<std::path::PathBuf>,
}
pub struct Engine {
    lockfiles: Option<patchula_update::lockfiles::Runner>,
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
    /// The server URL hidden in PRs and the dashboard for the CLI to find.
    advertised_server: Option<String>,
}
struct Work {
    report: Repository,
    config: Option<Config>,
    dependencies: Vec<Dependency>,
    /// Lockfile packages no manifest declares, for advisory checks.
    locked: Vec<patchula_manager::Locked>,
    snapshot: patchula_update::Snapshot,
    inventory: Option<patchula_update::Inventory>,
    controls: patchula_update::dashboard::Controls,
}
#[derive(Serialize, Deserialize)]
struct CachedExtraction {
    extraction: Extraction,
    snapshot: patchula_update::Snapshot,
}
#[derive(Clone, Deserialize)]
struct Entry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default = "file_mode")]
    mode: String,
}
fn file_mode() -> String {
    "100644".into()
}

type LookupKey = (String, String);
type Catalogs = BTreeMap<LookupKey, std::result::Result<patchula_datasource::Catalog, String>>;
pub const REPOSITORY_HEAD_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){isPrivate defaultBranchRef{name target{... on Commit{oid tree{oid}}}}}}";
#[derive(Default, Serialize, Deserialize)]
struct LookupHint {
    catalogs: BTreeSet<LookupKey>,
    digests: BTreeSet<LookupKey>,
}

impl Engine {
    /// Long-lived installation engine using the same batch path as caller-token runs.
    pub fn with_credentials(
        credentials: Arc<dyn patchula_github::Credentials>,
        cache: Cache,
        concurrency: usize,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        let mut engine = Self::with_transport(
            "installation",
            cache.clone(),
            concurrency,
            transport.clone(),
        )?;
        engine.github = Github::with_credentials(
            credentials.clone(),
            cache.clone(),
            GithubOptions::default(),
            transport,
        )?;
        engine.sources = Sources::new(engine.github.clone(), engine.http.clone(), cache.clone());
        engine.lockfiles = cache.workspace_dir().map(|path| {
            patchula_update::lockfiles::Runner::new(path.into(), "").with_credentials(credentials)
        });
        engine.look_up_mise_tools_with_runner();
        Ok(engine)
    }
    /// Settings change only between batches; warm clients and receipts stay intact.
    pub fn set_batch_options(&mut self, concurrency: usize, recheck: Recheck) -> Result<()> {
        if !(1..=100).contains(&concurrency) {
            return Err(Error::Invalid("concurrency must be 1..100".into()));
        }
        self.concurrency = concurrency;
        self.recheck = recheck;
        Ok(())
    }
    /// A server sets this between batches as its settings change.
    pub fn set_advertised_server(&mut self, url: Option<String>) {
        self.advertised_server = url;
    }
    pub fn with_recheck(mut self, recheck: Recheck) -> Self {
        self.recheck = recheck;
        self
    }
    pub fn with_dockerhub_credentials(mut self, username: String, token: String) -> Self {
        self.sources = self.sources.with_dockerhub_credentials(username, token);
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
        if concurrency == 0 || concurrency > 100 {
            return Err(Error::Invalid("concurrency must be 1..100".into()));
        }
        let github = Github::with_transport(
            token,
            cache.clone(),
            GithubOptions {
                concurrency: http_options.host_limit("api.github.com"),
                ..Default::default()
            },
            transport.clone(),
        )?;
        let http = Cached::with_options(cache.clone(), transport, http_options)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let sources = Sources::new(github.clone(), http.clone(), cache.clone());
        let lockfiles = cache
            .workspace_dir()
            .map(|path| patchula_update::lockfiles::Runner::new(path.into(), token));
        let mut engine = Self {
            lockfiles,
            github,
            http,
            sources,
            cache,
            concurrency,
            extraction_hits: AtomicU64::new(0),
            recheck: Recheck::default(),
            versions_host_override: None,
            advisories: false,
            advertised_server: None,
        };
        engine.look_up_mise_tools_with_runner();
        Ok(engine)
    }
    /// Tool versions come from the pinned mise in the lockfile runner's tool
    /// store, through its executor. An in-memory cache has neither.
    fn look_up_mise_tools_with_runner(&mut self) {
        if let Some(runner) = &self.lockfiles {
            self.sources = self.sources.clone().with_mise(runner.mise_lookup());
        }
    }
    pub fn with_lockfile_runner(mut self, runner: patchula_update::lockfiles::Runner) -> Self {
        self.lockfiles = Some(runner);
        self.look_up_mise_tools_with_runner();
        self
    }
    /// Seed the lockfile tool store from this mise data directory's installs.
    pub fn with_tool_seed(mut self, mise_data: std::path::PathBuf) -> Self {
        self.lockfiles = self.lockfiles.map(|runner| runner.with_seed(mise_data));
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
    fn catalog_key(&self, dep: &Dependency, config: Option<&Config>) -> LookupKey {
        let enabled = self
            .versions_host_override
            .unwrap_or(config.is_none_or(|config| config.use_versions_host));
        (
            format!("{enabled}:{}", word(&dep.source)),
            dep.package.clone(),
        )
    }
    async fn lookup_key(
        sources: &Sources,
        key: &LookupKey,
    ) -> std::result::Result<patchula_datasource::Catalog, String> {
        let (enabled, source) = key.0.split_once(':').ok_or("invalid catalog key")?;
        let sources = sources.clone().with_versions_host(enabled == "true");
        let source: Source =
            serde_json::from_value(Value::String(source.into())).map_err(|e| e.to_string())?;
        sources
            .lookup(source, &key.1)
            .await
            .map_err(|e| e.to_string())
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
        let mut branches = Vec::new();
        let mut pull_requests = Vec::new();
        let note_sources = self.sources.notes_batch();
        for mut work in work {
            let Some(config) = work.config else {
                continue;
            };
            let repo = report
                .repositories
                .iter_mut()
                .find(|r| r.repository == work.report.repository)
                .expect("lookup repository");
            if repo.status != Status::Complete {
                continue;
            }
            let inventory = work.inventory;
            let since = self.last_run(&repo.repository).await;
            let plans = patchula_update::plan(
                repo,
                &config,
                &work.dependencies,
                &work.snapshot,
                allow_lockfile_pending,
                since,
                now,
            );
            let mut plans = match plans {
                Ok(plans) => plans,
                Err(error) => {
                    repo.status = Status::Error;
                    repo.reason = Some(error);
                    continue;
                }
            };
            if let Some(mut maintenance) =
                patchula_update::lockfiles::maintenance(repo, &config, &work.snapshot, since, now)
                    .map_err(Error::Invalid)?
            {
                patchula_update::lockfiles::defer_completed_refresh(
                    &self.cache,
                    &mut maintenance,
                    &config,
                    now,
                )
                .await
                .map_err(Error::Invalid)?;
                plans.push(maintenance);
            }
            if !dry_run {
                work.controls.apply(&mut plans, &work.dependencies);
                let open = inventory.as_ref().expect("write inventory");
                match patchula_update::batch::apply(
                    &mut plans,
                    repo,
                    &config,
                    &work.dependencies,
                    &work.snapshot,
                    &open.pull_requests(&repo.repository),
                    &open.actor,
                    allow_lockfile_pending,
                    since,
                    now,
                ) {
                    Ok(batched) => work.controls.batched = batched,
                    Err(error) => {
                        repo.status = Status::Error;
                        repo.reason = Some(error);
                        continue;
                    }
                }
                let restored = patchula_update::restore_inventory(
                    &self.cache,
                    repo,
                    &config,
                    &mut plans,
                    self.recheck,
                    self.advertised_server.as_deref(),
                    inventory.as_ref().expect("write inventory"),
                )
                .await;
                match restored {
                    Ok(true) => {
                        let inventory = inventory.as_ref().expect("write inventory");
                        let merge_actions = patchula_update::automerge::reconcile(
                            &self.github,
                            &self.cache,
                            repo,
                            &config,
                            inventory,
                            &mut plans,
                            now,
                        )
                        .await;
                        if merge_actions.iter().any(|a| {
                            matches!(
                                a.status,
                                patchula_report::ActionStatus::Error
                                    | patchula_report::ActionStatus::Protected
                            )
                        }) {
                            repo.status = Status::Error;
                            repo.reason =
                                Some("auto-merge failed or was protected; inspect actions".into());
                        }
                        if let Err(error) = patchula_update::dashboard::reconcile(
                            &self.github,
                            &self.cache,
                            repo,
                            &config,
                            inventory,
                            patchula_update::dashboard::render(
                                repo,
                                &plans,
                                &patchula_update::automerge::dashboard_prs(
                                    inventory,
                                    &repo.repository,
                                    &plans,
                                ),
                                self.recheck,
                                self.advertised_server.as_deref(),
                            ),
                        )
                        .await
                        {
                            repo.status = Status::Error;
                            repo.reason = Some(format!("dependency dashboard: {error}"));
                        }
                        pull_requests.extend(
                            plans
                                .iter()
                                .filter(|p| {
                                    !merge_actions.iter().any(|a| a.branch == p.action.branch)
                                })
                                .filter_map(|p| {
                                    p.cached_pr.as_ref().map(|pr| {
                                        patchula_report::PullRequestAction {
                                            repository: repo.repository.clone(),
                                            branch: p.action.branch.clone(),
                                            number: pr["number"].as_u64(),
                                            url: pr["html_url"].as_str().map(str::to_owned),
                                            status: patchula_report::ActionStatus::Unchanged,
                                            reason: None,
                                        }
                                    })
                                }),
                        );
                        pull_requests.extend(merge_actions);
                        branches.extend(plans.into_iter().map(|p| p.action));
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        repo.status = Status::Error;
                        repo.reason = Some(error);
                        continue;
                    }
                }
            }
            let mut lifecycle = if dry_run {
                pull_requests.extend(
                    plans
                        .iter()
                        .filter(|p| p.action.status == patchula_report::ActionStatus::Planned)
                        .map(|p| patchula_report::PullRequestAction {
                            repository: repo.repository.clone(),
                            branch: p.action.branch.clone(),
                            number: None,
                            url: None,
                            status: patchula_report::ActionStatus::Planned,
                            reason: Some(
                                "create or reconcile after authoritative live inventory".into(),
                            ),
                        }),
                );
                None
            } else {
                match patchula_update::Lifecycle::prepare_with_controls(
                    &self.github,
                    repo,
                    &mut plans,
                    &config,
                    now,
                    self.recheck,
                    &work.controls,
                )
                .await
                {
                    Ok(mut state) => {
                        state.set_advertised_server(self.advertised_server.clone());
                        state.set_merge_settings(
                            inventory
                                .as_ref()
                                .expect("write inventory")
                                .merge_settings
                                .clone(),
                        );
                        Some(state)
                    }
                    Err(error) => {
                        repo.status = Status::Error;
                        repo.reason = Some(error);
                        continue;
                    }
                }
            };
            if !dry_run {
                for plan in &mut plans {
                    if let Some(runner) = &self.lockfiles {
                        runner.prepare(plan, &mut work.snapshot, &config).await;
                    } else if plan.action.status == patchula_report::ActionStatus::Planned
                        && !plan.lockfiles_ready
                    {
                        plan.action.status = patchula_report::ActionStatus::Error;
                        plan.action.reason = Some(
                            "lockfile generation requires a disk cache or an injected runner"
                                .into(),
                        );
                    }
                }
            }
            let cleanup = if repo.warnings.is_empty() {
                patchula_update::cleanup(
                    &self.github,
                    &repo.repository,
                    repo.commit.as_deref().expect("extracted commit"),
                    repo.default_branch.as_deref().expect("configured branch"),
                    &plans,
                    dry_run,
                )
                .await
            } else {
                Ok(Vec::new())
            };
            match cleanup {
                Ok(actions) => branches.extend(actions),
                Err(error) => {
                    repo.status = Status::Error;
                    repo.reason = Some(error);
                    continue;
                }
            }
            patchula_update::apply_all(
                &self.github,
                &mut plans,
                &work.snapshot,
                repo.default_branch.as_deref().expect("configured branch"),
                config.prs.rebase,
                dry_run,
            )
            .await;
            if let Some(state) = &mut lifecycle {
                state
                    .publish_with_sources(&self.github, &note_sources, &mut plans)
                    .await;
                let merge_actions = patchula_update::automerge::reconcile(
                    &self.github,
                    &self.cache,
                    repo,
                    &config,
                    inventory.as_ref().expect("write inventory"),
                    &mut plans,
                    now,
                )
                .await;
                state.record_merges(&plans, merge_actions);
                if let Err(error) = state.close_batched(&self.github, &plans).await {
                    repo.warnings.push(format!("batched PR closure: {error}"));
                }
                if let Err(error) = state.acknowledge_rechecks(&self.github).await {
                    repo.warnings
                        .push(format!("PR checkbox acknowledgement: {error}"));
                }
                if let Err(error) = patchula_update::dashboard::reconcile(
                    &self.github,
                    &self.cache,
                    repo,
                    &config,
                    inventory.as_ref().expect("write inventory"),
                    patchula_update::dashboard::render(
                        repo,
                        &plans,
                        &patchula_update::automerge::dashboard_history(
                            inventory.as_ref().expect("write inventory"),
                            state.dashboard_prs(),
                            &plans,
                        ),
                        self.recheck,
                        self.advertised_server.as_deref(),
                    ),
                )
                .await
                {
                    repo.status = Status::Error;
                    repo.reason = Some(format!("dependency dashboard: {error}"));
                }
                if let Err(error) = patchula_update::lockfiles::record_refresh(
                    &self.cache,
                    &plans,
                    &state.actions,
                    now,
                )
                .await
                {
                    repo.warnings.push(format!("maintenance cache: {error}"));
                }
                if let Err(error) =
                    patchula_update::save_receipts(&self.cache, repo, &config, &plans, state).await
                {
                    repo.warnings.push(format!("write receipt cache: {error}"));
                }
                if state.actions.iter().any(|a| {
                    matches!(
                        a.status,
                        patchula_report::ActionStatus::Error
                            | patchula_report::ActionStatus::Protected
                    )
                }) {
                    repo.status = Status::Error;
                    repo.reason = Some("one or more pull request actions failed or were protected; inspect actions".into());
                }
                pull_requests.append(&mut state.actions);
            }
            for plan in &plans {
                if matches!(
                    plan.action.status,
                    patchula_report::ActionStatus::Error | patchula_report::ActionStatus::Protected
                ) {
                    repo.status = Status::Error;
                    repo.reason.get_or_insert_with(|| {
                        "one or more branch actions failed or were protected; inspect actions"
                            .into()
                    });
                }
            }
            branches.extend(plans.into_iter().map(|p| p.action));
        }
        // Every repository that finished cleanly, including ones settled from
        // their write receipts, has taken this run's schedule turn.
        if !dry_run {
            for repo in report
                .repositories
                .iter_mut()
                .filter(|repo| repo.status == Status::Complete)
            {
                if let Err(error) = self.record_run(&repo.repository, now).await {
                    repo.warnings.push(format!("last run cache: {error}"));
                }
            }
        }
        branches.sort_by(|a, b| (&a.repository, &a.branch).cmp(&(&b.repository, &b.branch)));
        report.actions = Some(patchula_report::Actions {
            schema_version: 1,
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
    pub(crate) async fn last_run(&self, repo: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        let bytes = self
            .cache
            .extraction_get(&last_run_key(repo))
            .await
            .ok()??;
        let seconds: i64 = serde_json::from_slice(&bytes).ok()?;
        chrono::DateTime::from_timestamp(seconds, 0)
    }
    async fn record_run(&self, repo: &str, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
        let bytes = serde_json::to_vec(&now.timestamp())?;
        self.cache
            .extraction_put(&last_run_key(repo), &bytes)
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
        let local = options.local.clone();
        let sources = self.sources.clone();
        let total = Instant::now();
        let _total_span = PhaseTrace::new("total");
        let before_g = self.github.stats().await;
        let before_h = self.http.stats();
        let before_hits = self.extraction_hits.load(Ordering::Relaxed);
        let started = Instant::now();
        let phase = PhaseTrace::new("discovery");
        let repos = if let Some(dir) = &local {
            vec![local_name(dir)]
        } else if options.repositories.is_empty() {
            self.repositories().await?
        } else {
            options.repositories
        };
        let repos: BTreeSet<_> = repos.into_iter().collect();
        for repo in &repos {
            validate_repo(repo)?;
        }
        let overrides = overrides::load(options.config_dir.as_deref(), &repos)?;
        let discovery = micros(started);
        drop(phase);
        let override_config = options.config.as_ref();
        // Hints overlap mutable digest reads with the repository head read.
        // Catalogs wait for current policy, age-cache and checkbox decisions.
        let hint_hash = format!(
            "lookup-hint-v3:{}:{}:{}",
            hash(&serde_json::to_vec(&(
                &options.config,
                &overrides,
                self.versions_host_override
            ))?),
            hash(patchula_config::SCHEMA.as_bytes()),
            self.github.credential_scope()
        );
        let mut hint = LookupHint::default();
        for repo in &repos {
            let key = ExtractionKey {
                repo: format!("lookup-hint:{repo}"),
                default_branch_commit: String::new(),
                config_hash: hint_hash.clone(),
            };
            if let Some(bytes) = self.cache.extraction_get(&key).await? {
                let known: LookupHint = serde_json::from_slice(&bytes)?;
                hint.catalogs.extend(known.catalogs);
                hint.digests.extend(known.digests);
            }
        }
        let scans = async {
            let started = Instant::now();
            let _phase = PhaseTrace::new("extraction");
            let overrides = &overrides;
            let local = local.as_deref();
            let work: Vec<_> = stream::iter(repos)
                .map(|repo| async move {
                    let scanned = match local {
                        Some(dir) => self.scan_local(dir, &repo, override_config).await,
                        None => {
                            self.scan(&repo, overrides.get(&repo).or(override_config))
                                .await
                        }
                    };
                    let mut work = match scanned {
                        Ok(work) => work,
                        Err(error) => Work {
                            report: repository(&repo, Status::Error, Some(error.to_string())),
                            config: None,
                            dependencies: Vec::new(),
                            locked: Vec::new(),
                            snapshot: patchula_update::Snapshot::default(),
                            inventory: None,
                            controls: patchula_update::dashboard::Controls::default(),
                        },
                    };
                    if read_controls
                        && work.report.status == Status::Complete
                        && let Some(config) = &work.config
                    {
                        match patchula_update::Inventory::read(
                            &self.github,
                            &self.cache,
                            &work.report,
                            config,
                        )
                        .await
                        {
                            Ok(inventory) => {
                                match patchula_update::dashboard::Controls::read(
                                    &inventory,
                                    &repo,
                                    config.prs.dashboard,
                                ) {
                                    Ok(mut controls) => {
                                        controls.age_permits.extend(
                                            patchula_update::dashboard::load_permits(
                                                &self.cache,
                                                &repo,
                                                &inventory.actor,
                                            )
                                            .await
                                            .map_err(Error::Invalid)?,
                                        );
                                        work.controls = controls;
                                        work.inventory = Some(inventory);
                                    }
                                    Err(error) => {
                                        work.report.status = Status::Error;
                                        work.report.reason = Some(error);
                                    }
                                }
                            }
                            Err(error) => {
                                work.report.status = Status::Error;
                                work.report.reason = Some(error);
                            }
                        }
                    }
                    Ok::<_, Error>(work)
                })
                .buffer_unordered(self.concurrency)
                .collect()
                .await;
            (
                work.into_iter().collect::<Result<Vec<_>>>(),
                micros(started),
            )
        };
        let prefetch = async {
            let started = Instant::now();
            let _phase = PhaseTrace::new("lookup.prefetch");
            let result = tokio::join!(async { Catalogs::new() }, self.digests(hint.digests));
            (result, micros(started))
        };
        let ((work, extraction_time), ((mut catalogs, mut digests), prefetch_time)) =
            tokio::join!(scans, prefetch);
        let mut work = work?;
        work.sort_by(|a, b| a.report.repository.cmp(&b.report.repository));
        let phase = PhaseTrace::new("lookup");
        let started = Instant::now();
        let mut keys = BTreeSet::new();
        let mut waits = BTreeMap::<LookupKey, u32>::new();
        // Catalog reads wait for authoritative policy and checkbox state. Only
        // digest hints are speculative, so a warm cache hit sends no list read.
        let retry_keys = self.forced_catalog_keys(&work).await?;
        let fresh_sources = sources.revalidating();
        // Digest retries use the same scoped HTTP client later in the batch.
        let evaluators: Vec<_> = work
            .iter()
            .map(|repo| {
                repo.config
                    .as_ref()
                    .map(Evaluator::new)
                    .transpose()
                    .map_err(Error::Invalid)
            })
            .collect::<Result<_>>()?;
        for (repo, eval) in work.iter().zip(&evaluators) {
            if let Some(eval) = eval {
                for dep in &repo.dependencies {
                    if eval.needs_lookup(dep) || eval.needs_forge_catalog(dep) {
                        let key = self.catalog_key(dep, repo.config.as_ref());
                        keys.insert(key.clone());
                        let hours = eval.catalog_wait_hours(dep);
                        waits
                            .entry(key)
                            .and_modify(|wait| *wait = (*wait).min(hours))
                            .or_insert(hours);
                    }
                }
            }
        }
        // One lookup per source/package over the entire batch, even across
        // schemes and extractor-specific tag filters. Keep raw catalogs shared.
        let mut current_refs = BTreeSet::new();
        for (repo, eval) in work.iter().zip(&evaluators) {
            if let Some(eval) = eval {
                for dep in &repo.dependencies {
                    if dep.manager == Manager::GithubActions
                        && dep.source == Source::GithubTags
                        && !(dep.digest.is_some() && dep.resolved.is_none())
                        && eval.digest_proposal(dep, "probe").is_some()
                        && !eval.needs_lookup(dep)
                    {
                        current_refs.insert((
                            dep.package.clone(),
                            dep.resolved.as_deref().unwrap_or(&dep.current).to_owned(),
                        ));
                    }
                }
            }
        }
        // Replace per-repository hints only after successful extraction.
        for (repo, eval) in work.iter().zip(&evaluators) {
            if repo.report.status == Status::Complete
                && let Some(eval) = eval
            {
                let catalogs = repo
                    .dependencies
                    .iter()
                    .filter(|dep| eval.needs_lookup(dep) || eval.needs_forge_catalog(dep))
                    .map(|dep| self.catalog_key(dep, repo.config.as_ref()))
                    .collect();
                let digests = current_refs
                    .iter()
                    .filter(|(package, _)| {
                        repo.dependencies.iter().any(|dep| &dep.package == package)
                    })
                    .cloned()
                    .collect();
                let key = ExtractionKey {
                    repo: format!("lookup-hint:{}", repo.report.repository),
                    default_branch_commit: String::new(),
                    config_hash: hint_hash.clone(),
                };
                self.cache
                    .extraction_put(
                        &key,
                        &serde_json::to_vec(&LookupHint { catalogs, digests })?,
                    )
                    .await?;
            }
        }
        // Hints may refer to dependencies removed by the new commit/config.
        catalogs.retain(|key, _| keys.contains(key));
        keys.retain(|key| !catalogs.contains_key(key));
        current_refs.retain(|key| !digests.contains_key(key));
        // Revalidate current action refs alongside source heads. These network
        // reads used to serialize behind all catalogs inside the policy phase.
        let mut advisory_queries = BTreeSet::new();
        for (repo, eval) in work.iter().zip(&evaluators) {
            if let (Some(eval), Some(config)) = (eval, &repo.config)
                && self.advisories
                && config.security.advisories
            {
                advisory_queries.extend(
                    repo.dependencies
                        .iter()
                        .cloned()
                        .chain(repo.locked.iter().map(patchula_manager::Locked::dependency))
                        .filter(|dep| !eval.ignored(dep))
                        .filter_map(|dep| patchula_lookup::advisory_query(&dep)),
                );
            }
        }
        let (remaining, refs, advisories) = tokio::join!(
            self.age_catalogs(keys, &waits, &retry_keys, &sources, now),
            self.digests(current_refs),
            async {
                if advisory_queries.is_empty() {
                    Ok(BTreeMap::new())
                } else {
                    self.sources.advisories(&advisory_queries).await
                }
            }
        );
        let advisories = advisories.unwrap_or_else(|error| {
            for (repo, eval) in work.iter_mut().zip(&evaluators) {
                if eval.as_ref().is_some_and(|eval| {
                    repo.dependencies.iter().any(|dep| {
                        !eval.ignored(dep)
                            && patchula_lookup::advisory_query(dep)
                                .is_some_and(|q| advisory_queries.contains(&q))
                    })
                }) {
                    repo.report
                        .warnings
                        .push(format!("advisory lookup on OSV.dev failed: {error}"));
                }
            }
            BTreeMap::new()
        });
        let (mut remaining, mut catalog_evidence) = remaining?;
        // A lockfile-only package joins the dependencies once it is vulnerable.
        for (repo, eval) in work.iter_mut().zip(&evaluators) {
            if eval.is_some() {
                let vulnerable: Vec<_> = repo
                    .locked
                    .iter()
                    .map(patchula_manager::Locked::dependency)
                    .filter(|dep| {
                        patchula_lookup::advisory_query(dep)
                            .and_then(|q| advisories.get(&q))
                            .is_some_and(|found| !found.is_empty())
                    })
                    .collect();
                repo.dependencies.extend(vulnerable);
            }
        }
        // A vulnerable dependency's fix may be newer than a catalog reused
        // under its release-age wait, so those catalogs are read again. So is a
        // forge action's: its tags may have moved, and their commits are its pins.
        let mut vulnerable = BTreeSet::new();
        for (repo, eval) in work.iter().zip(&evaluators) {
            if let Some(eval) = eval {
                for dep in &repo.dependencies {
                    let key = self.catalog_key(dep, repo.config.as_ref());
                    let reused = catalog_evidence.get(&key).is_some_and(|e| e.reused);
                    if reused
                        && (eval.needs_forge_catalog(dep)
                            || (eval.needs_lookup(dep)
                                && patchula_lookup::advisory_query(dep)
                                    .and_then(|q| advisories.get(&q))
                                    .is_some_and(|found| !found.is_empty())))
                    {
                        vulnerable.insert(key);
                    }
                }
            }
        }
        if !vulnerable.is_empty() {
            let (fresh, evidence) = self
                .age_catalogs(
                    vulnerable,
                    &BTreeMap::new(),
                    &BTreeSet::new(),
                    &sources,
                    now,
                )
                .await?;
            remaining.extend(fresh);
            catalog_evidence.extend(evidence);
        }
        catalogs.extend(remaining);
        digests.extend(refs);
        let lookup_time = prefetch_time + micros(started);
        drop(phase);
        let started = Instant::now();
        let phase = PhaseTrace::new("policy.evaluate");
        let mut proposals: Vec<Vec<Evaluation>> = Vec::with_capacity(work.len());
        let mut digest_keys = BTreeSet::new();
        let mut image_keys = BTreeSet::new();
        for (repo, eval) in work.iter_mut().zip(&evaluators) {
            let mut deps = Vec::with_capacity(repo.dependencies.len());
            for dep in &repo.dependencies {
                let mut evaluation = Evaluation::default();
                if let Some(eval) = eval
                    && !eval.ignored(dep)
                {
                    let found = patchula_lookup::advisory_query(dep)
                        .and_then(|query| advisories.get(&query).map(|found| (query, found)));
                    let found = found
                        .as_ref()
                        .map(|(query, found)| (query, found.as_slice()));
                    match catalogs.get(&self.catalog_key(dep, repo.config.as_ref())) {
                        Some(Ok(catalog)) => {
                            match eval.evaluate_with(dep, catalog, now, &BTreeSet::new(), found) {
                                Ok(p) => {
                                    let dep_key = patchula_update::dashboard::extracted_key(dep);
                                    let overrides: BTreeSet<_> = p
                                        .held_back
                                        .iter()
                                        .flat_map(|h| {
                                            h.versions
                                                .iter()
                                                .chain(std::iter::once(&h.version))
                                                .map(|v| (v.clone(), h.update_type.clone()))
                                        })
                                        .filter(|(v, k)| {
                                            let target = patchula_update::dashboard::held_target(
                                                &dep_key, v, k,
                                            );
                                            repo.controls.create.contains(&target)
                                                || repo.controls.age_permits.contains(&target)
                                        })
                                        .collect();
                                    if overrides.is_empty() {
                                        evaluation = p;
                                    } else {
                                        match eval
                                            .evaluate_with(dep, catalog, now, &overrides, found)
                                        {
                                            Ok(p) => {
                                                for (v, k) in overrides {
                                                    repo.controls.age_permits.insert(
                                                        patchula_update::dashboard::held_target(
                                                            &dep_key, &v, &k,
                                                        ),
                                                    );
                                                }
                                                evaluation = p;
                                            }
                                            Err(e) => repo
                                                .report
                                                .warnings
                                                .push(format!("{}: {e}", dep.name)),
                                        }
                                    }
                                }
                                Err(e) => repo.report.warnings.push(format!("{}: {e}", dep.name)),
                            }
                        }
                        Some(Err(error)) => repo
                            .report
                            .warnings
                            .push(format!("{} ({}): {error}", dep.name, dep.package)),
                        None => {}
                    }
                    if matches!(dep.source, Source::Docker | Source::Oci) {
                        for proposal in &evaluation.updates {
                            if eval.needs_digest(dep, proposal)
                                && let Some(tag) = &proposal.new_version
                            {
                                image_keys.insert((dep.package.clone(), tag.clone()));
                            }
                        }
                        if eval.digest_proposal(dep, "probe").is_some() {
                            image_keys.insert((dep.package.clone(), dep.current.clone()));
                        }
                    }
                    if dep.tagged_action() {
                        for proposal in &evaluation.updates {
                            if eval.needs_digest(dep, proposal)
                                && let Some(reference) = proposal
                                    .new_version
                                    .as_deref()
                                    .or(proposal.new_value.as_deref())
                            {
                                digest_keys.insert((dep.package.clone(), reference.to_owned()));
                            }
                        }
                        if dep.digest.is_some() && dep.resolved.is_none() {
                            repo.report.warnings.push(format!(
                                "{}: SHA pin has no version comment; replacement tag is unknown",
                                dep.name
                            ));
                        } else if dep.source == Source::GithubTags || eval.needs_forge_catalog(dep)
                        {
                            // A forge resolves only tags, which its catalog
                            // already carries; branch refs stay unpinned.
                            let reference = dep.resolved.as_deref().unwrap_or(&dep.current);
                            if eval.digest_proposal(dep, "probe").is_some() {
                                digest_keys.insert((dep.package.clone(), reference.to_owned()));
                            }
                        }
                    }
                }
                deps.push(evaluation);
            }
            proposals.push(deps);
        }
        let proposal_cpu_time = micros(started);
        for repo in &work {
            if let Some(inventory) = &repo.inventory {
                patchula_update::dashboard::save_permits(
                    &self.cache,
                    &repo.report.repository,
                    &inventory.actor,
                    repo.controls.age_permits.clone(),
                )
                .await
                .map_err(Error::Invalid)?;
            }
        }
        drop(phase);
        // The revalidated complete tag namespace already carries peeled target
        // commits. Reuse those for both current and proposed action tags instead
        // of adding a serial /commits/{tag} round trip after policy evaluation.
        for (package, reference) in &digest_keys {
            if let Some(Ok(catalog)) = catalogs.iter().find_map(|((source, pkg), result)| {
                (pkg == package
                    && (source.ends_with(":forge-tags")
                        || (source.ends_with(":github-tags")
                            && catalog_evidence
                                .get(&(source.clone(), pkg.clone()))
                                .is_none_or(|e| !e.reused))))
                .then_some(result)
            }) && let Some(digest) = catalog
                .versions
                .iter()
                .find(|version| &version.version == reference)
                .and_then(|version| version.digest.as_ref())
            {
                digests.insert((package.clone(), reference.clone()), Ok(digest.clone()));
            }
        }
        digest_keys.retain(|key| !digests.contains_key(key));
        // Forge refs resolve only through their tag catalog. A ref missing
        // from it is not sent to GitHub.
        digest_keys.retain(|(package, reference)| {
            let Some(result) = catalogs.iter().find_map(|((source, pkg), result)| {
                (source.ends_with(":forge-tags") && pkg == package).then_some(result)
            }) else {
                return true;
            };
            if result.is_ok() {
                digests.insert(
                    (package.clone(), reference.clone()),
                    Err(format!("tag {reference} not found")),
                );
            }
            false
        });
        let started = Instant::now();
        let phase = PhaseTrace::new("digest");
        let mutable_tags: Vec<_> = digest_keys
            .iter()
            .filter(|(package, _)| {
                catalog_evidence.iter().any(|((source, pkg), e)| {
                    source.ends_with(":github-tags") && pkg == package && e.reused
                })
            })
            .cloned()
            .collect();
        digests.extend(self.sources.fresh_tag_digests(&mutable_tags).await);
        digest_keys.retain(|key| !digests.contains_key(key));
        digests.extend(self.digests(digest_keys).await);
        let retry_keys = &retry_keys;
        let fresh_sources = &fresh_sources;
        let image_digests: BTreeMap<_, _> = stream::iter(image_keys)
            .map(|(package, tag)| async move {
                let sources = if retry_keys
                    .iter()
                    .any(|(s, p)| (s.ends_with(":docker") || s.ends_with(":oci")) && p == &package)
                {
                    fresh_sources
                } else {
                    &self.sources
                };
                let result = sources
                    .registry_digest(&package, &tag)
                    .await
                    .map_err(|e| e.to_string());
                ((package, tag), result)
            })
            .buffer_unordered(100)
            .collect()
            .await;
        let digest_time = micros(started);
        drop(phase);
        let started = Instant::now();
        let _phase = PhaseTrace::new("policy.report");
        for ((repo, eval), dep_proposals) in work.iter_mut().zip(&evaluators).zip(proposals) {
            let mut files: BTreeMap<String, Vec<ReportDependency>> = BTreeMap::new();
            for (dep, evaluation) in repo.dependencies.iter().zip(dep_proposals) {
                let mut proposals = evaluation.updates;
                if let Some(eval) = eval
                    && matches!(dep.source, Source::Docker | Source::Oci)
                    && !eval.ignored(dep)
                {
                    for proposal in &mut proposals {
                        if eval.needs_digest(dep, proposal)
                            && let Some(tag) = &proposal.new_version
                        {
                            match image_digests.get(&(dep.package.clone(), tag.clone())) {
                                Some(Ok(digest)) => proposal.new_digest = Some(digest.clone()),
                                Some(Err(e)) => repo
                                    .report
                                    .warnings
                                    .push(format!("{}@{tag}: {e}", dep.name)),
                                None => {}
                            }
                        }
                    }
                    if let Some(result) =
                        image_digests.get(&(dep.package.clone(), dep.current.clone()))
                    {
                        match result {
                            Ok(digest) => {
                                if let Some(p) = eval.digest_proposal(dep, digest) {
                                    proposals.push(p);
                                }
                            }
                            Err(e) => repo.report.warnings.push(format!("{}: {e}", dep.name)),
                        }
                    }
                }
                if let Some(eval) = eval
                    && dep.tagged_action()
                    && !eval.ignored(dep)
                {
                    for proposal in &mut proposals {
                        if eval.needs_digest(dep, proposal)
                            && let Some(reference) = proposal
                                .new_version
                                .as_deref()
                                .or(proposal.new_value.as_deref())
                        {
                            match digests.get(&(dep.package.clone(), reference.to_owned())) {
                                Some(Ok(sha)) => proposal.new_digest = Some(sha.clone()),
                                Some(Err(e)) => {
                                    proposal.new_digest = None;
                                    repo.report.warnings.push(format!(
                                        "{}@{reference}: cannot pin target digest: {e}",
                                        dep.name
                                    ));
                                }
                                None => {}
                            }
                        }
                    }
                    let reference = dep.resolved.as_deref().unwrap_or(&dep.current);
                    if let Some(result) = digests.get(&(dep.package.clone(), reference.to_owned()))
                    {
                        match result {
                            Ok(sha) => {
                                if let Some(proposal) = eval.digest_proposal_with_catalog(
                                    dep,
                                    sha,
                                    catalogs
                                        .get(&self.catalog_key(dep, repo.config.as_ref()))
                                        .and_then(|c| c.as_ref().ok()),
                                ) {
                                    proposals.push(proposal);
                                }
                            }
                            Err(e) => repo.report.warnings.push(format!(
                                "{}@{reference}: cannot resolve digest: {e}",
                                dep.name
                            )),
                        }
                    }
                }
                let advisory = patchula_lookup::advisory_query(dep)
                    .and_then(|query| advisories.get(&query).map(|found| (query, found)))
                    .filter(|(_, found)| !found.is_empty());
                if let (Some(eval), Some((query, found))) = (eval, &advisory)
                    && let Some(fix) = eval.transitive_fix(dep, (query, found))
                {
                    proposals.push(fix);
                }
                if let Some((query, found)) = &advisory {
                    let label = repo
                        .config
                        .as_ref()
                        .map(|c| c.security.label.as_str())
                        .unwrap_or_default();
                    for proposal in &mut proposals {
                        let Some(target) = proposal
                            .new_version
                            .as_ref()
                            .or(proposal.new_value.as_ref())
                        else {
                            continue;
                        };
                        proposal.fixes = patchula_lookup::fixes(dep, query, found, target);
                        if let Some(policy) = &mut proposal.policy
                            && !proposal.fixes.is_empty()
                            && !label.is_empty()
                            && !policy.labels.iter().any(|l| l == label)
                        {
                            policy.labels.push(label.into());
                        }
                    }
                }
                proposals.sort();
                proposals.dedup();
                let stale = if matches!(dep.source, Source::Docker | Source::Oci) {
                    let mut stale = self.sources.registry_stale(&dep.package).await;
                    stale.retain(|key, _| {
                        key == "catalog"
                            || key.strip_prefix("digest:").is_some_and(|tag| {
                                tag == dep.current.replace('+', "_")
                                    || proposals.iter().any(|p| {
                                        p.new_version
                                            .as_ref()
                                            .is_some_and(|v| v.replace('+', "_") == tag)
                                    })
                            })
                    });
                    stale
                } else {
                    BTreeMap::new()
                };
                if !stale.is_empty() {
                    repo.report.warnings.push(format!(
                        "{}: stale cached registry lookup after rate limiting ({})",
                        dep.name,
                        stale.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                let catalog = catalogs
                    .get(&self.catalog_key(dep, repo.config.as_ref()))
                    .and_then(|c| c.as_ref().ok());
                if dep.tagged_action() && catalog.is_some_and(|c| c.versions.is_empty()) {
                    repo.report.warnings.push(format!(
                        "{}: registry returned an empty tag catalog; version lookup is unavailable",
                        dep.name
                    ));
                }
                let resolved =
                    catalog.and_then(|c| patchula_lookup::resolved_current_version(dep, c));
                let resolution = resolved.as_ref().or(dep.resolved.as_ref()).map(|version| {
                    let kind = if dep.tagged_action() {
                        if dep.digest.is_some() {
                            "pinned-commit"
                        } else {
                            "floating-ref"
                        }
                    } else if matches!(dep.manager, Manager::Npm | Manager::Cargo)
                        && !matches!(
                            dep.kind,
                            patchula_config::DependencyKind::Toolchain
                                | patchula_config::DependencyKind::Tool
                        )
                        && dep.resolved.is_some()
                    {
                        "lockfile"
                    } else if resolved.is_some()
                        && matches!(dep.kind, patchula_config::DependencyKind::Tool)
                    {
                        "release-line"
                    } else if resolved.is_some()
                        && dep.kind == patchula_config::DependencyKind::Toolchain
                    {
                        "supported-range"
                    } else {
                        "declared"
                    };
                    patchula_report::Resolution {
                        kind: kind.into(),
                        version: version.clone(),
                    }
                });
                let proposals_refs: BTreeSet<_> = proposals
                    .iter()
                    .filter_map(|p| p.new_value.clone())
                    .collect();
                let value = ReportDependency {
                    manager: word(&dep.manager),
                    name: dep.name.clone(),
                    occurrence: None,
                    package: dep.package.clone(),
                    current: Current {
                        value: Some(dep.current.clone()),
                        version: catalogs
                            .get(&self.catalog_key(dep, repo.config.as_ref()))
                            .and_then(|c| c.as_ref().ok())
                            .and_then(|c| patchula_lookup::resolved_current_version(dep, c))
                            .or_else(|| dep.resolved.clone())
                            .or_else(|| {
                                patchula_lookup::scheme(dep)
                                    .ok()
                                    .filter(|_| dep.kind != patchula_config::DependencyKind::Runner)
                                    .and_then(|s| {
                                        patchula_versioning::Versioning::parse(&s, &dep.current)
                                            .ok()
                                    })
                                    .map(|v| v.0.to_string())
                            }),
                        digest: dep.digest.clone(),
                    },
                    updates: proposals,
                    metadata: Some(patchula_report::DependencyMetadata {
                        tag_digests: if dep.tagged_action() {
                            catalog
                                .into_iter()
                                .flat_map(|c| &c.versions)
                                .filter(|v| proposals_refs.contains(&v.version))
                                .filter_map(|v| {
                                    v.digest
                                        .as_ref()
                                        .map(|digest| (v.version.clone(), digest.clone()))
                                })
                                .collect()
                        } else {
                            BTreeMap::new()
                        },
                        source: word(&dep.source),
                        kinds: dep.kinds().iter().map(word).collect(),
                        locations: vec![patchula_report::Location {
                            start: dep.location.start,
                            end: dep.location.end,
                            line: dep.location.line,
                            column: dep.location.column,
                            pointer: dep.location.pointer.clone(),
                        }],
                        held_back: evaluation.held_back,
                        stale,
                        lookup: eval.as_ref().map(|eval| {
                            let mut evidence = eval.lookup_evidence(dep, catalog, now);
                            evidence.catalog = catalog_evidence
                                .get(&self.catalog_key(dep, repo.config.as_ref()))
                                .cloned();
                            evidence
                        }),
                        resolution,
                        advisories: advisory
                            .map(|(query, found)| {
                                patchula_lookup::report_advisories(dep, &query, found)
                            })
                            .unwrap_or_default(),
                    }),
                };
                files.entry(dep.file.clone()).or_default().push(value);
            }
            repo.report.package_files = files
                .into_iter()
                .map(|(path, dependencies)| PackageFile {
                    path,
                    dependencies: patchula_report::normalize_dependencies(dependencies),
                })
                .collect();
            repo.report.warnings.sort();
            repo.report.warnings.dedup();
        }
        self.save_catalog_controls(&work, now).await?;
        let policy_time = proposal_cpu_time + micros(started);
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
                    + catalog_evidence.values().filter(|e| e.reused).count() as u64,
                extraction_hits: self.extraction_hits.load(Ordering::Relaxed) - before_hits,
                lookups: catalogs.len() as u64,
                phase_micros: BTreeMap::from([
                    ("discovery".into(), discovery),
                    ("extraction".into(), extraction_time),
                    ("lookup".into(), lookup_time),
                    ("policy".into(), policy_time),
                    ("digest".into(), digest_time),
                    ("total".into(), micros(total)),
                ]),
            }),
        };
        report.validate()?;
        Ok((report, work))
    }
    async fn digests(
        &self,
        keys: BTreeSet<(String, String)>,
    ) -> BTreeMap<(String, String), std::result::Result<String, String>> {
        stream::iter(keys)
            .map(|(package, reference)| async move {
                let result = self
                    .sources
                    .digest(&package, &reference)
                    .await
                    .map_err(|e| e.to_string());
                ((package, reference), result)
            })
            .buffer_unordered(100)
            .collect()
            .await
    }

    async fn catalogs(&self, keys: BTreeSet<LookupKey>, sources: &Sources) -> Catalogs {
        let (releases, others): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|(source, _)| source.ends_with(":github-releases"));
        let mut groups = Vec::new();
        for enabled in [true, false] {
            let packages: Vec<_> = releases
                .iter()
                .filter(|(source, _)| source.starts_with(&enabled.to_string()))
                .map(|(_, package)| package.clone())
                .collect();
            groups.extend(packages.chunks(16).map(|chunk| (enabled, chunk.to_vec())));
        }
        let batched = stream::iter(groups)
            .map(|(enabled, packages)| async move {
                let client = sources.clone().with_versions_host(enabled);
                match client.release_catalogs(&packages).await {
                    Ok(results) => results
                        .into_iter()
                        .map(|(package, result)| {
                            (
                                (format!("{enabled}:github-releases"), package),
                                result.map_err(|e| e.to_string()),
                            )
                        })
                        .collect::<Catalogs>(),
                    Err(_) => {
                        stream::iter(packages)
                            .map(|package| async move {
                                let key = (format!("{enabled}:github-releases"), package);
                                let result = Self::lookup_key(sources, &key).await;
                                (key, result)
                            })
                            .buffer_unordered(16)
                            .collect()
                            .await
                    }
                }
            })
            .buffer_unordered(16)
            .collect::<Vec<_>>();
        let others = stream::iter(others)
            .map(|key| async move {
                let result = Self::lookup_key(sources, &key).await;
                (key, result)
            })
            .buffer_unordered(100)
            .collect::<Catalogs>();
        let (groups, mut results) = tokio::join!(batched, others);
        results.extend(groups.into_iter().flatten());
        results
    }
    async fn repositories(&self) -> Result<Vec<String>> {
        match self
            .github
            .rest_pages::<Value>("user/repos?per_page=100", |_| false)
            .await
        {
            Ok(repos) => repos
                .iter()
                .map(|v| {
                    v["full_name"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Error::Invalid("invalid repository listing".into()))
                })
                .collect(),
            Err(patchula_github::Error::Status(401 | 403)) => {
                let mut repos = Vec::new();
                for page in 1..=1000 {
                    let value = self
                        .json(&format!(
                            "installation/repositories?per_page=100&page={page}"
                        ))
                        .await?;
                    let entries = value["repositories"].as_array().ok_or_else(|| {
                        Error::Invalid("invalid installation repository listing".into())
                    })?;
                    for entry in entries {
                        repos.push(
                            entry["full_name"]
                                .as_str()
                                .ok_or_else(|| {
                                    Error::Invalid("invalid installation repository".into())
                                })?
                                .into(),
                        );
                    }
                    if entries.len() < 100 {
                        return Ok(repos);
                    }
                }
                Err(Error::Invalid(
                    "installation repository page budget exceeded".into(),
                ))
            }
            Err(e) => Err(e.into()),
        }
    }
    async fn json(&self, path: &str) -> Result<Value> {
        Ok(serde_json::from_slice(&self.github.get(path).await?.body)?)
    }
    async fn tree(&self, repo: &str, sha: &str) -> Result<Vec<Entry>> {
        let value = self
            .json(&format!("repos/{repo}/git/trees/{sha}?recursive=1"))
            .await?;
        if value["truncated"].as_bool() != Some(false) {
            return Err(Error::Invalid(
                "repository tree is truncated; cannot guarantee complete extraction".into(),
            ));
        }
        Ok(serde_json::from_value(value["tree"].clone())?)
    }
    async fn file(&self, repo: &str, sha: &str) -> Result<String> {
        use base64::Engine;
        let value = self.json(&format!("repos/{repo}/git/blobs/{sha}")).await?;
        if value["encoding"] != "base64" {
            return Err(Error::Invalid("unsupported blob encoding".into()));
        }
        let content = value["content"]
            .as_str()
            .ok_or_else(|| Error::Invalid("blob has no content".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content.replace(['\n', '\r'], ""))
            .map_err(|_| Error::Invalid("invalid base64 blob".into()))?;
        String::from_utf8(bytes).map_err(|_| Error::Invalid("selected file is not UTF-8".into()))
    }
    async fn scan(&self, repo: &str, override_config: Option<&Config>) -> Result<Work> {
        let hint_key = ExtractionKey {
            repo: format!("branch-hint:{repo}"),
            default_branch_commit: self.github.credential_scope().into(),
            config_hash: "branch-v2".into(),
        };
        let hint = self
            .cache
            .extraction_get(&hint_key)
            .await?
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let (branch, commit, tree_sha) = if let Some(hint) = hint {
            // Conditional REST heads are faster than GraphQL snapshots on the
            // measured warm path. Revalidate metadata and the hinted branch in
            // parallel; a rename retries only the newly authoritative branch.
            let metadata_path = format!("repos/{repo}");
            let (metadata, head) = tokio::join!(self.json(&metadata_path), self.head(repo, &hint));
            let metadata = metadata?;
            let branch = metadata["default_branch"]
                .as_str()
                .ok_or_else(|| Error::Invalid("repository has no default branch".into()))?
                .to_owned();
            let head = if branch == hint {
                head?
            } else {
                self.head(repo, &branch).await?
            };
            let commit = head["commit"]["sha"]
                .as_str()
                .ok_or_else(|| Error::Invalid("branch has no commit".into()))?
                .to_owned();
            let tree = head["commit"]["commit"]["tree"]["sha"]
                .as_str()
                .ok_or_else(|| Error::Invalid("commit has no tree".into()))?
                .to_owned();
            (branch, commit, tree)
        } else {
            let (owner, name) = repo.split_once('/').expect("validated repository");
            let data = self
                .github
                .graphql(
                    REPOSITORY_HEAD_QUERY,
                    serde_json::json!({"owner":owner,"name":name}),
                )
                .await?;
            let head = &data["repository"]["defaultBranchRef"];
            let branch = head["name"]
                .as_str()
                .ok_or_else(|| Error::Invalid("repository has no default branch".into()))?
                .to_owned();
            let commit = head["target"]["oid"]
                .as_str()
                .ok_or_else(|| Error::Invalid("branch has no commit".into()))?
                .to_owned();
            let tree = head["target"]["tree"]["oid"]
                .as_str()
                .ok_or_else(|| Error::Invalid("commit has no tree".into()))?
                .to_owned();
            (branch, commit, tree)
        };
        self.cache
            .extraction_put(&hint_key, branch.as_bytes())
            .await?;
        let mut report = repository(repo, Status::Complete, None);
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
                        return Ok(Work {
                            report: repository(
                                repo,
                                Status::Skipped,
                                Some("the Count was not invited: no patchula.pkl".into()),
                            ),
                            config: None,
                            dependencies: Vec::new(),
                            locked: Vec::new(),
                            snapshot: patchula_update::Snapshot::default(),
                            inventory: None,
                            controls: patchula_update::dashboard::Controls::default(),
                        });
                    }
                }
            }
        };
        let scanner = Scanner::new(&config).map_err(Error::Invalid)?;
        let dir = dir.to_owned();
        let (paths, blobs, extraction) = tokio::task::spawn_blocking(move || {
            let mut paths = BTreeSet::new();
            let mut blobs = BTreeMap::new();
            let mut pending = vec![dir.clone()];
            while let Some(next) = pending.pop() {
                for entry in std::fs::read_dir(&next)? {
                    let entry = entry?;
                    let kind = entry.file_type()?;
                    let path = entry.path();
                    if kind.is_dir() {
                        if entry.file_name() != ".git" {
                            pending.push(path);
                        }
                        continue;
                    }
                    if !kind.is_file() {
                        continue;
                    }
                    let relative = path
                        .strip_prefix(&dir)
                        .expect("walked below the root")
                        .to_string_lossy()
                        .replace('\\', "/");
                    if scanner.needs(&relative) {
                        // Non-UTF-8 files cannot hold an editable declaration.
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            blobs.insert(
                                relative.clone(),
                                patchula_update::Blob {
                                    sha: patchula_update::blob_sha(&content),
                                    mode: "100644".into(),
                                    content,
                                },
                            );
                        }
                    }
                    paths.insert(relative);
                }
            }
            let files: Files = blobs
                .iter()
                .map(|(p, b): (&String, &patchula_update::Blob)| (p.clone(), b.content.clone()))
                .collect();
            let extraction = scanner.extract(&files).map_err(std::io::Error::other)?;
            std::io::Result::Ok((paths, blobs, extraction))
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?
        .map_err(|e| Error::Invalid(e.to_string()))?;
        let mut report = repository(repo, Status::Complete, None);
        report.warnings = extraction.warnings;
        report.skipped = skipped(&extraction.skipped);
        // Plans are based on a commit; a directory's stands in as a hash of
        // its selected files, stable until they change.
        report.commit = Some(hash(&serde_json::to_vec(
            &blobs
                .iter()
                .map(|(path, blob)| (path, &blob.sha))
                .collect::<Vec<_>>(),
        )?));
        report.default_branch = Some("local".into());
        Ok(Work {
            report,
            config: Some(config),
            dependencies: extraction.dependencies,
            locked: extraction.locked,
            snapshot: patchula_update::Snapshot {
                paths,
                files: blobs,
            },
            inventory: None,
            controls: patchula_update::dashboard::Controls::default(),
        })
    }
    async fn head(&self, repo: &str, branch: &str) -> Result<Value> {
        let mut url =
            reqwest::Url::parse(&format!("https://api.github.com/repos/{repo}/branches/"))
                .expect("validated repository");
        url.path_segments_mut()
            .expect("https URL")
            .pop_if_empty()
            .push(branch);
        self.json(url.as_str()).await
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
            let key = ExtractionKey {
                repo: format!("config:{repo}"),
                default_branch_commit: commit.clone(),
                // v2: only patchula.pkl at the root is discovered.
                config_hash: format!("v2:{}", hash(patchula_config::SCHEMA.as_bytes())),
            };
            if let Some(bytes) = self.cache.extraction_get(&key).await? {
                serde_json::from_slice(&bytes)?
            } else {
                let entries = self.tree(repo, tree_sha).await?;
                let path = Some(patchula_config::CONFIG_PATH).filter(|p| {
                    entries
                        .iter()
                        .any(|entry| entry.path == *p && entry.kind == "blob")
                });
                let config = if let Some(path) = path {
                    Some(self.config(repo, path, &entries).await?)
                } else {
                    None
                };
                self.cache
                    .extraction_put(&key, &serde_json::to_vec(&config)?)
                    .await?;
                tree = Some(entries);
                config
            }
        };
        let Some(config) = config else {
            report.status = Status::Skipped;
            report.reason = Some("the Count was not invited: no patchula.pkl".into());
            return Ok(Work {
                report,
                config: None,
                dependencies: Vec::new(),
                locked: Vec::new(),
                snapshot: patchula_update::Snapshot::default(),
                inventory: None,
                controls: patchula_update::dashboard::Controls::default(),
            });
        };
        let key = ExtractionKey {
            repo: repo.into(),
            default_branch_commit: commit,
            config_hash: format!("extract-v10:{}", hash(&serde_json::to_vec(&config)?)),
        };
        let cached: CachedExtraction = if let Some(bytes) = self.cache.extraction_get(&key).await? {
            self.extraction_hits.fetch_add(1, Ordering::Relaxed);
            serde_json::from_slice(&bytes)?
        } else {
            let scanner = Scanner::new(&config).map_err(Error::Invalid)?;
            let entries = if let Some(tree) = tree {
                tree
            } else {
                self.tree(repo, tree_sha).await?
            };
            let paths = entries
                .iter()
                .filter(|e| e.kind == "blob")
                .map(|e| e.path.clone())
                .collect();
            let selected: Vec<_> = entries
                .into_iter()
                .filter(|entry| entry.kind == "blob" && scanner.needs(&entry.path))
                .collect();
            let blobs: BTreeMap<String, patchula_update::Blob> = stream::iter(selected)
                .map(|entry| async move {
                    let content = self.file(repo, &entry.sha).await?;
                    Ok((
                        entry.path,
                        patchula_update::Blob {
                            sha: entry.sha,
                            mode: entry.mode,
                            content,
                        },
                    ))
                })
                .buffer_unordered(self.github.concurrency())
                .collect::<Vec<Result<_>>>()
                .await
                .into_iter()
                .collect::<Result<_>>()?;
            let files: Files = blobs
                .iter()
                .map(|(p, b)| (p.clone(), b.content.clone()))
                .collect();
            let extraction = tokio::task::spawn_blocking(move || scanner.extract(&files))
                .await
                .map_err(|e| Error::Invalid(e.to_string()))?
                .map_err(Error::Invalid)?;
            let cached = CachedExtraction {
                extraction,
                snapshot: patchula_update::Snapshot {
                    paths,
                    files: blobs,
                },
            };
            self.cache
                .extraction_put(&key, &serde_json::to_vec(&cached)?)
                .await?;
            cached
        };
        report.warnings = cached.extraction.warnings;
        report.skipped = skipped(&cached.extraction.skipped);
        Ok(Work {
            report,
            config: Some(config),
            dependencies: cached.extraction.dependencies,
            locked: cached.extraction.locked,
            snapshot: cached.snapshot,
            inventory: None,
            controls: patchula_update::dashboard::Controls::default(),
        })
    }
    async fn config(&self, repo: &str, path: &str, entries: &[Entry]) -> Result<Config> {
        let imports = regex::Regex::new(r#"(?:amends|extends|import\*?)\s*\(?\s*"([^"]+)""#)
            .expect("import regex");
        let mut files = BTreeMap::new();
        let mut pending = vec![path.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(path) = pending.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            if visited.len() > 100 {
                return Err(Error::Invalid("config import budget exceeded".into()));
            }
            let text =
                if let Some(entry) = entries.iter().find(|e| e.path == path && e.kind == "blob") {
                    self.file(repo, &entry.sha).await?
                } else if path.rsplit('/').next() == Some(patchula_config::SCHEMA_NAME) {
                    patchula_config::SCHEMA.into()
                } else {
                    return Err(Error::Invalid(format!(
                        "missing local config module {path}"
                    )));
                };
            for captures in imports.captures_iter(&text) {
                let relative = &captures[1];
                if relative.contains(':') || relative.starts_with('/') {
                    return Err(Error::Invalid(
                        "repository config imports must be relative local modules".into(),
                    ));
                }
                let parent = std::path::Path::new(&path)
                    .parent()
                    .unwrap_or(std::path::Path::new(""));
                let mut normalized = std::path::PathBuf::new();
                for component in parent.join(relative).components() {
                    match component {
                        std::path::Component::ParentDir => {
                            if !normalized.pop() {
                                return Err(Error::Invalid(
                                    "config import escapes repository".into(),
                                ));
                            }
                        }
                        std::path::Component::CurDir => {}
                        part => normalized.push(part.as_os_str()),
                    }
                }
                pending.push(normalized.to_string_lossy().into_owned());
            }
            files.insert(format!("/repo/{path}"), text);
        }
        let path = format!("/repo/{path}");
        tokio::task::spawn_blocking(move || {
            patchula_config::load_files(&path, files).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?
        .map_err(Error::Invalid)
    }
}
fn last_run_key(repo: &str) -> ExtractionKey {
    ExtractionKey {
        repo: format!("last-run:{repo}"),
        default_branch_commit: String::default(),
        config_hash: "last-run-v1".into(),
    }
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros().min(u64::MAX as u128) as u64
}
fn validate_repo(repo: &str) -> Result<()> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(Error::Invalid(format!("expected owner/repo, got {repo}")));
    };
    if owner.is_empty()
        || name.is_empty()
        || [owner, name].iter().any(|s| {
            !s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                || matches!(*s, "." | "..")
        })
    {
        return Err(Error::Invalid("invalid repository identity".into()));
    }
    Ok(())
}
/// A local directory reports as `local/<directory name>`.
fn local_name(dir: &std::path::Path) -> String {
    let name = dir
        .canonicalize()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "directory".into());
    format!("local/{name}")
}
fn repository(repo: &str, status: Status, reason: Option<String>) -> Repository {
    Repository {
        repository: repo.into(),
        status,
        reason,
        package_files: Vec::new(),
        warnings: Vec::new(),
        skipped: Vec::new(),
        default_branch: None,
        commit: None,
    }
}
fn skipped(skipped: &[patchula_manager::Skipped]) -> Vec<patchula_report::Skipped> {
    skipped
        .iter()
        .map(|s| patchula_report::Skipped {
            manager: patchula_manager::word(&s.manager),
            path: s.file.clone(),
            line: s.line,
            value: s.value.clone(),
            reason: s.reason.clone(),
        })
        .collect()
}
