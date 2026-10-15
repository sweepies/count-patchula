//! Read-only batch engine shared by CLI and future server callers.
#![forbid(unsafe_code)]
use futures_util::{StreamExt, stream};
use rustovate_cache::{Cache, ExtractionKey};
use rustovate_config::{Config, Manager, Source};
use rustovate_datasource::Client as Sources;
use rustovate_github::{Client as Github, Options as GithubOptions};
use rustovate_http::{Cached, Transport};
use rustovate_lookup::Evaluator;
use rustovate_manager::{Dependency, Extraction, Files, Scanner, word};
use rustovate_report::{
    Current, Dependency as ReportDependency, PackageFile, Report, Repository, SCHEMA_VERSION,
    Status, Update,
};
use serde::Deserialize;
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

impl Engine {
    pub fn with_transport(
        token: &str,
        cache: Cache,
        concurrency: usize,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        if concurrency == 0 || concurrency > 100 {
            return Err(Error::Invalid("concurrency must be 1..100".into()));
        }
        let github = Github::with_transport(
            token,
            cache.clone(),
            GithubOptions {
                concurrency,
                ..Default::default()
            },
            transport.clone(),
        )?;
        let http = Cached::new(cache.clone(), transport, concurrency)
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
        let total = Instant::now();
        let before_g = self.github.stats().await;
        let before_h = self.http.stats();
        let before_hits = self.extraction_hits.load(Ordering::Relaxed);
        let started = Instant::now();
        let repos = if options.repositories.is_empty() {
            self.repositories().await?
        } else {
            options.repositories
        };
        let repos: BTreeSet<_> = repos.into_iter().collect();
        for repo in &repos {
            validate_repo(repo)?;
        }
        let discovery = micros(started);
        let started = Instant::now();
        let override_config = options.config.as_ref();
        let mut work: Vec<_> = stream::iter(repos)
            .map(|repo| async move {
                match self.scan(&repo, override_config).await {
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
        work.sort_by(|a, b| a.report.repository.cmp(&b.report.repository));
        let extraction_time = micros(started);
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
        let catalogs: BTreeMap<_, _> = stream::iter(keys)
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
            .buffer_unordered(self.concurrency)
            .collect()
            .await;
        let lookup_time = micros(started);
        let started = Instant::now();
        let mut proposals: Vec<Vec<Vec<Update>>> = Vec::with_capacity(work.len());
        let mut digest_keys = BTreeSet::new();
        for (repo, eval) in work.iter_mut().zip(&evaluators) {
            let mut deps = Vec::with_capacity(repo.dependencies.len());
            for dep in &repo.dependencies {
                let mut proposed = Vec::new();
                if let Some(eval) = eval
                    && !eval.ignored(dep)
                {
                    match catalogs.get(&(word(&dep.source), dep.package.clone())) {
                        Some(Ok(catalog)) => match eval.propose(dep, catalog) {
                            Ok(p) => proposed = p,
                            Err(e) => repo.report.warnings.push(format!("{}: {e}", dep.name)),
                        },
                        Some(Err(error)) => repo
                            .report
                            .warnings
                            .push(format!("{} ({}): {error}", dep.name, dep.package)),
                        None => {}
                    }
                    if dep.manager == Manager::GithubActions && dep.source == Source::GithubTags {
                        for proposal in &proposed {
                            if eval.needs_digest(dep, proposal)
                                && let Some(reference) = proposal.new_value.as_deref()
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
                deps.push(proposed);
            }
            proposals.push(deps);
        }
        let digests: BTreeMap<_, _> = stream::iter(digest_keys)
            .map(|(package, reference)| async move {
                let result = self
                    .sources
                    .digest(&package, &reference)
                    .await
                    .map_err(|e| e.to_string());
                ((package, reference), result)
            })
            .buffer_unordered(self.concurrency)
            .collect()
            .await;
        for ((repo, eval), dep_proposals) in work.iter_mut().zip(&evaluators).zip(proposals) {
            let mut files: BTreeMap<String, Vec<ReportDependency>> = BTreeMap::new();
            for (dep, mut proposals) in repo.dependencies.iter().zip(dep_proposals) {
                if let Some(eval) = eval
                    && dep.manager == Manager::GithubActions
                    && dep.source == Source::GithubTags
                    && !eval.ignored(dep)
                {
                    for proposal in &mut proposals {
                        if eval.needs_digest(dep, proposal)
                            && let Some(reference) = proposal.new_value.as_deref()
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
                                if let Some(proposal) = eval.digest_proposal(dep, sha) {
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
                let value = ReportDependency {
                    manager: word(&dep.manager),
                    name: dep.name.clone(),
                    occurrence: None,
                    package: dep.package.clone(),
                    current: Current {
                        value: Some(dep.current.clone()),
                        version: dep.resolved.clone().or_else(|| {
                            rustovate_lookup::scheme(dep)
                                .ok()
                                .filter(|s| {
                                    matches!(
                                        s,
                                        rustovate_versioning::Ecosystem::Npm
                                            | rustovate_versioning::Ecosystem::Go
                                    )
                                })
                                .and_then(|s| {
                                    rustovate_versioning::Versioning::parse(&s, &dep.current).ok()
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
        let policy_time = micros(started);
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
                    ("total".into(), micros(total)),
                ]),
            }),
        };
        report.validate()?;
        Ok(report)
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
        let metadata = self.json(&format!("repos/{repo}")).await?;
        let branch = metadata["default_branch"]
            .as_str()
            .ok_or_else(|| Error::Invalid("repository has no default branch".into()))?
            .to_owned();
        let mut branch_url =
            reqwest::Url::parse(&format!("https://api.github.com/repos/{repo}/branches/"))
                .expect("validated repository URL");
        branch_url
            .path_segments_mut()
            .expect("https URL")
            .pop_if_empty()
            .push(&branch);
        let head = self.json(branch_url.as_str()).await?;
        let commit = head["commit"]["sha"]
            .as_str()
            .ok_or_else(|| Error::Invalid("branch has no commit".into()))?
            .to_owned();
        let tree_sha = head["commit"]["commit"]["tree"]["sha"]
            .as_str()
            .ok_or_else(|| Error::Invalid("commit has no tree".into()))?;
        let mut report = repository(repo, Status::Complete, None);
        report.default_branch = Some(branch);
        report.commit = Some(commit.clone());
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
            config_hash: format!("extract-v1:{}", hash(&serde_json::to_vec(&config)?)),
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
                .buffer_unordered(self.concurrency)
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
