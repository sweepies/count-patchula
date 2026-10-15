//! Read-only batch engine shared by CLI and future server callers.
#![forbid(unsafe_code)]
mod overrides;

use futures_util::{StreamExt, stream};
use rustovate_cache::{Cache, ExtractionKey};
use rustovate_config::{Config, Manager, Source};
use rustovate_datasource::Client as Sources;
use rustovate_github::{Client as Github, Options as GithubOptions};
use rustovate_http::{Cached, PhaseTrace, Transport};
use rustovate_lookup::{Evaluation, Evaluator};
use rustovate_manager::{Dependency, Extraction, Files, Scanner, word};
use rustovate_report::{
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
    Github(#[from] rustovate_github::Error),
    #[error(transparent)]
    Cache(#[from] rustovate_cache::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Report(#[from] rustovate_report::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Default)]
pub struct Options {
    pub repositories: Vec<String>,
    pub config: Option<Config>,
    pub config_dir: Option<std::path::PathBuf>,
}
pub struct Engine {
    github: Github,
    http: Cached,
    sources: Sources,
    cache: Cache,
    concurrency: usize,
    extraction_hits: AtomicU64,
}
struct Work {
    report: Repository,
    config: Option<Config>,
    dependencies: Vec<Dependency>,
}
#[derive(Clone, Deserialize)]
struct Entry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}

type LookupKey = (String, String);
type Catalogs = BTreeMap<LookupKey, std::result::Result<rustovate_datasource::Catalog, String>>;
pub const REPOSITORY_HEAD_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){isPrivate defaultBranchRef{name target{... on Commit{oid tree{oid}}}}}}";
#[derive(Default, Serialize, Deserialize)]
struct LookupHint {
    catalogs: BTreeSet<LookupKey>,
    digests: BTreeSet<LookupKey>,
}

impl Engine {
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
        http_options: rustovate_http::Options,
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
        Ok(Self {
            github,
            http,
            sources,
            cache,
            concurrency,
            extraction_hits: AtomicU64::new(0),
        })
    }
    pub async fn lookup(&self, options: Options) -> Result<Report> {
        self.lookup_at(options, chrono::Utc::now()).await
    }
    pub async fn lookup_at(
        &self,
        options: Options,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Report> {
        if options.config.is_some() && options.config_dir.is_some() {
            return Err(Error::Configuration(
                "--config and --config-dir are mutually exclusive".into(),
            ));
        }
        let total = Instant::now();
        let _total_span = PhaseTrace::new("total");
        let before_g = self.github.stats().await;
        let before_h = self.http.stats();
        let before_hits = self.extraction_hits.load(Ordering::Relaxed);
        let started = Instant::now();
        let phase = PhaseTrace::new("discovery");
        let repos = if options.repositories.is_empty() {
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
        // A hint only schedules reads. Freshly validated extraction/config is
        // always authoritative for evaluation; unused speculative results are
        // discarded. Overlap source revalidation with the repository head read.
        let hint_hash = format!(
            "lookup-hint-v1:{}:{}:{}:{}",
            hash(&serde_json::to_vec(&(&options.config, &overrides))?),
            hash(rustovate_config::SCHEMA.as_bytes()),
            hash(rustovate_manager::MISE_REGISTRY.as_bytes()),
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
            let work: Vec<_> = stream::iter(repos)
                .map(|repo| async move {
                    match self
                        .scan(&repo, overrides.get(&repo).or(override_config))
                        .await
                    {
                        Ok(work) => work,
                        Err(error) => Work {
                            report: repository(&repo, Status::Error, Some(error.to_string())),
                            config: None,
                            dependencies: Vec::new(),
                        },
                    }
                })
                .buffer_unordered(self.concurrency)
                .collect()
                .await;
            (work, micros(started))
        };
        let prefetch = async {
            let started = Instant::now();
            let _phase = PhaseTrace::new("lookup.prefetch");
            let result = tokio::join!(self.catalogs(hint.catalogs), self.digests(hint.digests));
            (result, micros(started))
        };
        let ((mut work, extraction_time), ((mut catalogs, mut digests), prefetch_time)) =
            tokio::join!(scans, prefetch);
        work.sort_by(|a, b| a.report.repository.cmp(&b.report.repository));
        let phase = PhaseTrace::new("lookup");
        let started = Instant::now();
        let mut keys = BTreeSet::new();
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
                    if eval.needs_lookup(dep) {
                        keys.insert((word(&dep.source), dep.package.clone()));
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
                    .filter(|dep| eval.needs_lookup(dep))
                    .map(|dep| (word(&dep.source), dep.package.clone()))
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
        let (remaining, refs) = tokio::join!(self.catalogs(keys), self.digests(current_refs));
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
                    match catalogs.get(&(word(&dep.source), dep.package.clone())) {
                        Some(Ok(catalog)) => match eval.evaluate_at(dep, catalog, now) {
                            Ok(p) => evaluation = p,
                            Err(e) => repo.report.warnings.push(format!("{}: {e}", dep.name)),
                        },
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
                    if dep.manager == Manager::GithubActions && dep.source == Source::GithubTags {
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
                        } else {
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
        drop(phase);
        // The revalidated complete tag namespace already carries peeled target
        // commits. Reuse those for both current and proposed action tags instead
        // of adding a serial /commits/{tag} round trip after policy evaluation.
        for (package, reference) in &digest_keys {
            if let Some(Ok(catalog)) = catalogs.get(&(word(&Source::GithubTags), package.clone()))
                && let Some(digest) = catalog
                    .versions
                    .iter()
                    .find(|version| &version.version == reference)
                    .and_then(|version| version.digest.as_ref())
            {
                digests.insert((package.clone(), reference.clone()), Ok(digest.clone()));
            }
        }
        digest_keys.retain(|key| !digests.contains_key(key));
        let started = Instant::now();
        let phase = PhaseTrace::new("digest");
        digests.extend(self.digests(digest_keys).await);
        let image_digests: BTreeMap<_, _> = stream::iter(image_keys)
            .map(|(package, tag)| async move {
                let result = self
                    .sources
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
                    && dep.manager == Manager::GithubActions
                    && dep.source == Source::GithubTags
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
                                        .get(&(word(&dep.source), dep.package.clone()))
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
                    .get(&(word(&dep.source), dep.package.clone()))
                    .and_then(|c| c.as_ref().ok());
                if dep.manager == Manager::GithubActions
                    && dep.source == Source::GithubTags
                    && catalog.is_some_and(|c| c.versions.is_empty())
                {
                    repo.report.warnings.push(format!(
                        "{}: registry returned an empty tag catalog; version lookup is unavailable",
                        dep.name
                    ));
                }
                let resolved =
                    catalog.and_then(|c| rustovate_lookup::resolved_current_version(dep, c));
                let resolution = resolved.as_ref().or(dep.resolved.as_ref()).map(|version| {
                    let kind = if dep.manager == Manager::GithubActions
                        && dep.source == Source::GithubTags
                    {
                        if dep.digest.is_some() {
                            "pinned-commit"
                        } else {
                            "floating-ref"
                        }
                    } else if matches!(dep.manager, Manager::Npm | Manager::Cargo)
                        && !matches!(
                            dep.kind,
                            rustovate_config::DependencyKind::Toolchain
                                | rustovate_config::DependencyKind::Tool
                        )
                        && dep.resolved.is_some()
                    {
                        "lockfile"
                    } else if resolved.is_some()
                        && matches!(dep.kind, rustovate_config::DependencyKind::Tool)
                    {
                        "release-line"
                    } else if resolved.is_some()
                        && dep.kind == rustovate_config::DependencyKind::Toolchain
                    {
                        "supported-range"
                    } else {
                        "declared"
                    };
                    rustovate_report::Resolution {
                        kind: kind.into(),
                        version: version.clone(),
                    }
                });
                let value = ReportDependency {
                    manager: word(&dep.manager),
                    name: dep.name.clone(),
                    occurrence: None,
                    package: dep.package.clone(),
                    current: Current {
                        value: Some(dep.current.clone()),
                        version: catalogs
                            .get(&(word(&dep.source), dep.package.clone()))
                            .and_then(|c| c.as_ref().ok())
                            .and_then(|c| rustovate_lookup::resolved_current_version(dep, c))
                            .or_else(|| dep.resolved.clone())
                            .or_else(|| {
                                rustovate_lookup::scheme(dep)
                                    .ok()
                                    .filter(|_| {
                                        dep.kind != rustovate_config::DependencyKind::Runner
                                    })
                                    .and_then(|s| {
                                        rustovate_versioning::Versioning::parse(&s, &dep.current)
                                            .ok()
                                    })
                                    .map(|v| v.0.to_string())
                            }),
                        digest: dep.digest.clone(),
                    },
                    updates: proposals,
                    metadata: Some(rustovate_report::DependencyMetadata {
                        source: word(&dep.source),
                        kinds: vec![word(&dep.kind)],
                        locations: vec![rustovate_report::Location {
                            start: dep.location.start,
                            end: dep.location.end,
                            line: dep.location.line,
                            column: dep.location.column,
                            pointer: dep.location.pointer.clone(),
                        }],
                        held_back: evaluation.held_back,
                        stale,
                        lookup: eval
                            .as_ref()
                            .map(|eval| eval.lookup_evidence(dep, catalog, now)),
                        resolution,
                    }),
                };
                files.entry(dep.file.clone()).or_default().push(value);
            }
            repo.report.package_files = files
                .into_iter()
                .map(|(path, dependencies)| PackageFile {
                    path,
                    dependencies: rustovate_report::normalize_dependencies(dependencies),
                })
                .collect();
            repo.report.warnings.sort();
            repo.report.warnings.dedup();
        }
        let policy_time = proposal_cpu_time + micros(started);
        let after_g = self.github.stats().await;
        let after_h = self.http.stats();
        let report = Report {
            schema_version: SCHEMA_VERSION,
            repositories: work.into_iter().map(|w| w.report).collect(),
            stats: Some(rustovate_report::Stats {
                requests: after_g.requests + after_h.requests
                    - before_g.requests
                    - before_h.requests,
                not_modified: after_g.not_modified + after_h.not_modified
                    - before_g.not_modified
                    - before_h.not_modified,
                cache_hits: after_h.cache_hits - before_h.cache_hits + after_g.not_modified
                    - before_g.not_modified,
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
        Ok(report)
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

    async fn catalogs(&self, keys: BTreeSet<LookupKey>) -> Catalogs {
        // The HTTP governors cap requests. A catalog walking several pages
        // must not occupy a slot that prevents unrelated sources from starting.
        let (releases, others): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|(source, _)| source == "github-releases");
        let packages: Vec<_> = releases.into_iter().map(|(_, package)| package).collect();
        let batched = async {
            let groups = stream::iter(packages.chunks(16))
                .map(|group| async move {
                    match self.sources.release_catalogs(group).await {
                        Ok(catalogs) => catalogs
                            .into_iter()
                            .map(|(package, catalog)| {
                                (
                                    ("github-releases".into(), package),
                                    catalog.map_err(|e| e.to_string()),
                                )
                            })
                            .collect(),
                        Err(_) => {
                            // One inaccessible repository must not poison a group.
                            stream::iter(group)
                                .map(|package| async move {
                                    (
                                        ("github-releases".into(), package.clone()),
                                        self.sources
                                            .lookup(Source::GithubReleases, package)
                                            .await
                                            .map_err(|e| e.to_string()),
                                    )
                                })
                                .buffer_unordered(100)
                                .collect::<Catalogs>()
                                .await
                        }
                    }
                })
                .buffer_unordered(100)
                .collect::<Vec<Catalogs>>()
                .await;
            groups.into_iter().flatten().collect::<Catalogs>()
        };
        let pending = others.len().max(1);
        let other_catalogs = stream::iter(others)
            .map(|(source, package)| async move {
                let parsed: Source =
                    serde_json::from_value(Value::String(source.clone())).expect("typed source");
                let result = self
                    .sources
                    .lookup(parsed, &package)
                    .await
                    .map_err(|e| e.to_string());
                ((source, package), result)
            })
            .buffer_unordered(pending)
            .collect::<Catalogs>();
        let (mut catalogs, other_catalogs) = tokio::join!(batched, other_catalogs);
        catalogs.extend(other_catalogs);
        catalogs
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
            Err(rustovate_github::Error::Status(401 | 403)) => {
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
                config_hash: hash(rustovate_config::SCHEMA.as_bytes()),
            };
            if let Some(bytes) = self.cache.extraction_get(&key).await? {
                serde_json::from_slice(&bytes)?
            } else {
                let entries = self.tree(repo, tree_sha).await?;
                let path = [".github/rustovate.pkl", "rustovate.pkl"]
                    .into_iter()
                    .find(|p| {
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
            report.reason = Some("no .github/rustovate.pkl or rustovate.pkl".into());
            return Ok(Work {
                report,
                config: None,
                dependencies: Vec::new(),
            });
        };
        let key = ExtractionKey {
            repo: repo.into(),
            default_branch_commit: commit,
            config_hash: format!(
                "extract-v6:{}:{}",
                hash(&serde_json::to_vec(&config)?),
                hash(rustovate_manager::MISE_REGISTRY.as_bytes())
            ),
        };
        let extraction: Extraction = if let Some(bytes) = self.cache.extraction_get(&key).await? {
            self.extraction_hits.fetch_add(1, Ordering::Relaxed);
            serde_json::from_slice(&bytes)?
        } else {
            let scanner = Scanner::new(&config).map_err(Error::Invalid)?;
            let entries = if let Some(tree) = tree {
                tree
            } else {
                self.tree(repo, tree_sha).await?
            };
            let selected: Vec<_> = entries
                .into_iter()
                .filter(|entry| entry.kind == "blob" && scanner.needs(&entry.path))
                .collect();
            let files: Files = stream::iter(selected)
                .map(|entry| async move { Ok((entry.path, self.file(repo, &entry.sha).await?)) })
                .buffer_unordered(self.github.concurrency())
                .collect::<Vec<Result<_>>>()
                .await
                .into_iter()
                .collect::<Result<_>>()?;
            let extraction = tokio::task::spawn_blocking(move || scanner.extract(&files))
                .await
                .map_err(|e| Error::Invalid(e.to_string()))?
                .map_err(Error::Invalid)?;
            self.cache
                .extraction_put(&key, &serde_json::to_vec(&extraction)?)
                .await?;
            extraction
        };
        report.warnings = extraction.warnings;
        Ok(Work {
            report,
            config: Some(config),
            dependencies: extraction.dependencies,
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
                } else if path.rsplit('/').next() == Some("Rustovate.pkl") {
                    rustovate_config::SCHEMA.into()
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
            rustovate_config::load_files(&path, files).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?
        .map_err(Error::Invalid)
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
fn repository(repo: &str, status: Status, reason: Option<String>) -> Repository {
    Repository {
        repository: repo.into(),
        status,
        reason,
        package_files: Vec::new(),
        warnings: Vec::new(),
        default_branch: None,
        commit: None,
    }
}
