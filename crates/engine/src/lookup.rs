//! The lookup batch as stages: plan what each dependency needs, fetch it
//! once for the whole batch, evaluate policy, resolve digests and read
//! images. Report assembly lives in `report`.
use super::write::failed;
use super::*;
use chrono::{DateTime, Utc};
use std::sync::Mutex;

/// Independent ref or image digest reads run this many at once; the per-host
/// governors do the real limiting.
const DIGEST_FAN_OUT: usize = 100;
/// SBOM reads are heavier than a digest's; fewer run at once.
const SBOM_FAN_OUT: usize = 16;

/// What discovery produced before any repository is read.
pub(super) struct Discovery {
    repos: BTreeSet<String>,
    overrides: BTreeMap<String, Config>,
    /// Hash of the lookup inputs the hints are scoped by.
    inputs_hash: String,
    /// Hash of the config schema the hints are scoped by.
    pub(super) schema_hash: String,
    /// The credential scope the hints are scoped by.
    credential_scope: String,
    hint: LookupHint,
}

/// What the scan stage produced.
pub(super) struct Scanned {
    pub work: Vec<Work>,
    pub extraction: u64,
    pub prefetch: u64,
    pub digests: BTreeMap<DigestKey, std::result::Result<String, Failure>>,
}

/// What the post-scan stages produced: the finished work, the lookups they
/// made and each stage's microseconds.
pub(super) struct LookupTail {
    pub lookups: Lookups,
    pub work: Vec<Work>,
    pub lookup: u64,
    pub digest: u64,
    pub image: u64,
    pub policy: u64,
}

/// What the plan stage decided the batch must fetch.
#[derive(Default)]
pub(super) struct Plan {
    keys: BTreeSet<LookupKey>,
    waits: BTreeMap<LookupKey, u32>,
    forced: BTreeSet<LookupKey>,
    /// Mutable action refs whose current commit is read up front, and
    /// whether a manual retry revalidates each.
    current_refs: BTreeMap<DigestKey, bool>,
    /// Each scanned repository's replacement lookup hint and the packages
    /// it may name, written once the run's mutable tags are known.
    hints: Vec<(LookupHintScope, LookupHint, BTreeSet<String>)>,
    /// OSV queries to ask, and whether a manual retry revalidates each.
    advisories: BTreeMap<patchula_datasource::AdvisoryQuery, bool>,
}

/// The digest keys policy evaluation wants resolved, and whether a manual
/// retry revalidates each.
#[derive(Default)]
pub(super) struct EvalKeys {
    /// Action and image references, keyed by package and reference.
    digests: BTreeMap<DigestKey, bool>,
    images: BTreeMap<DigestKey, bool>,
}

/// Record `key` flagged `revalidate`, keeping the flag once any contributor
/// asked for it.
fn flagged<V: Ord>(map: &mut BTreeMap<V, bool>, key: V, revalidate: bool) {
    map.entry(key)
        .and_modify(|flag| *flag |= revalidate)
        .or_insert(revalidate);
}

/// The advisory IDs one digest's SBOMs list, per the batch OSV listing.
fn listed_ids(
    sboms: &ImageSboms,
    listing: &patchula_datasource::AdvisoryListing,
    package: &str,
    digest: &str,
    platform: &str,
) -> BTreeSet<String> {
    let key = (package.to_owned(), digest.to_owned(), platform.to_owned());
    match sboms.get(&key) {
        Some(Ok(patchula_datasource::ImageSbom::Found(sbom))) => sbom
            .queries()
            .iter()
            .filter_map(|q| listing.get(q))
            .flatten()
            .map(|(id, _)| id.clone())
            .collect(),
        _ => BTreeSet::new(),
    }
}

impl Engine {
    /// Stage: discovery — the repositories to scan, their config overrides,
    /// and the hashes the lookup hints are scoped by.
    pub(super) async fn discover(&self, options: &Options) -> Result<Discovery> {
        let repos: BTreeSet<String> = if let Some(dir) = &options.local {
            BTreeSet::from([local_name(dir)])
        } else if options.repositories.is_empty() {
            self.progress.stage(Stage::Discovery, 0);
            self.repositories().await?.into_iter().collect()
        } else {
            options.repositories.iter().cloned().collect()
        };
        for repo in &repos {
            validate_repo(repo)?;
        }
        let overrides = overrides::load(options.config_dir.as_deref(), &repos)?;
        // Hints overlap mutable digest reads with the repository head read.
        // Catalogs wait for current policy, age-cache and checkbox decisions.
        let inputs_hash = patchula_manager::hash(&serde_json::to_vec(&(
            &options.config,
            &overrides,
            self.versions_host_override,
        ))?);
        let schema_hash = patchula_manager::hash(patchula_config::SCHEMA.as_bytes());
        let credential_scope = self.github.credential_scope().to_owned();
        let mut hint = LookupHint::default();
        for repo in &repos {
            let scope = LookupHintScope {
                repository: repo.clone(),
                inputs_hash: inputs_hash.clone(),
                schema_hash: schema_hash.clone(),
                credential_scope: credential_scope.clone(),
            };
            if let Some(known) = self.cache.state::<LookupHint>(&scope).await? {
                hint.catalogs.extend(known.catalogs);
                hint.digests.extend(known.digests);
                hint.tags.extend(known.tags);
            }
        }
        Ok(Discovery {
            repos,
            overrides,
            inputs_hash,
            schema_hash,
            credential_scope,
            hint,
        })
    }

    /// The hinted digest reads a scan overlaps: the refs the last runs
    /// recorded, and the mutable tags they tracked. A quiet sweep's lookup
    /// prefetches exactly these before its stages.
    pub(super) async fn prefetch_digests(
        &self,
        discovery: &Discovery,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> BTreeMap<DigestKey, std::result::Result<String, Failure>> {
        let hint_digests = discovery.hint.digests.clone();
        // A pinned tag is also a hinted ref; the tracked ref read answers it.
        let hint_tags: BTreeSet<DigestKey> = discovery
            .hint
            .tags
            .difference(&hint_digests)
            .cloned()
            .collect();
        let (digests, tags) = tokio::join!(
            self.digests(
                hint_digests.into_iter().map(|key| (key, false)).collect(),
                &self.sources,
                false,
                sent,
            ),
            self.sources.fresh_tag_digests(&hint_tags),
        );
        let mut digests = digests;
        digests.extend(tags);
        digests
    }

    /// Stage: scan — read every repository's config and files; hinted digests
    /// prefetch alongside.
    pub(super) async fn scan_all(
        &self,
        discovery: &Discovery,
        options: &Options,
        read_controls: bool,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> Result<Scanned> {
        self.progress
            .stage(Stage::Extraction, discovery.repos.len());
        let override_config = options.config.as_ref();
        let repos = &discovery.repos;
        let overrides = &discovery.overrides;
        let scans = async {
            let started = Instant::now();
            let _phase = PhaseTrace::new("extraction");
            let local = options.local.as_deref();
            let work: Vec<_> = stream::iter(repos.iter().cloned())
                .map(|repo| async move {
                    let (scanned, early) = match local {
                        Some(dir) => (self.scan_local(dir, &repo, override_config).await, None),
                        None => {
                            // The inventory reads beside the head check,
                            // under the branch prefix an override declares,
                            // else the last receipted run's, else the
                            // bundled default. A scan that disagrees throws
                            // the read away; it is the same read either way.
                            let early = read_controls.then_some(async {
                                let prefix = match overrides.get(&repo).or(override_config) {
                                    Some(config) => config.prs.branch.clone(),
                                    None => {
                                        patchula_update::Inventory::assumed_prefix(
                                            &self.cache,
                                            &repo,
                                        )
                                        .await?
                                    }
                                };
                                Ok::<_, patchula_update::Error>((
                                    patchula_update::Inventory::read_beside(
                                        &self.github,
                                        &self.cache,
                                        &repo,
                                        &prefix,
                                    )
                                    .await?,
                                    prefix,
                                ))
                            });
                            let scan = self.scan(&repo, overrides.get(&repo).or(override_config));
                            match early {
                                Some(early) => {
                                    tokio::pin!(scan);
                                    tokio::pin!(early);
                                    // Whichever answers first: a finished
                                    // early read leaves the scan to end
                                    // alone, and a scan that cannot use an
                                    // inventory — refused, failed or
                                    // config-free — drops the in-flight
                                    // walk rather than waiting out its
                                    // pages. A walk already done when the
                                    // scan wins its poll is still kept.
                                    let mut scanned_answer = None;
                                    let early_answer = tokio::select! {
                                        biased;
                                        scanned = scan.as_mut() => {
                                            scanned_answer = Some(scanned);
                                            futures_util::FutureExt::now_or_never(
                                                early.as_mut(),
                                            )
                                        }
                                        read = early.as_mut() => Some(read),
                                    };
                                    let scanned = match scanned_answer {
                                        Some(scanned) => scanned,
                                        None => scan.await,
                                    };
                                    let early = match early_answer {
                                        Some(read) => Some(read),
                                        None if matches!(
                                            &scanned,
                                            Ok(work)
                                                if work.report.status
                                                    == Status::Complete
                                                    && work.config.is_some()
                                        ) =>
                                        {
                                            Some(early.await)
                                        }
                                        None => None,
                                    };
                                    (scanned, early)
                                }
                                None => (scan.await, None),
                            }
                        }
                    };
                    let mut work = match scanned {
                        Ok(work) => work,
                        Err(error) => Work::empty(
                            repo_report(&repo, Status::Error, Some(error.to_string())),
                            None,
                        ),
                    };
                    if read_controls
                        && work.report.status == Status::Complete
                        && let Some(config) = &work.config
                    {
                        // The early inventory is usable only when it saw
                        // the scan's head and was read for the scan's
                        // prefix; anything else reads it again as before.
                        let read = match early {
                            Some(Ok((inventory, prefix)))
                                if inventory.default_branch.name == work.report.default_branch
                                    && inventory.default_branch.oid == work.report.commit
                                    && prefix == config.prs.branch =>
                            {
                                Ok(inventory)
                            }
                            _ => {
                                patchula_update::Inventory::read(
                                    &self.github,
                                    &self.cache,
                                    &work.report,
                                    config,
                                )
                                .await
                            }
                        };
                        match read {
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
                                            .await?,
                                        );
                                        work.controls = controls;
                                        work.inventory = Some(inventory);
                                    }
                                    Err(error) => {
                                        failed(&mut work.report, error.into());
                                    }
                                }
                            }
                            Err(error) => {
                                failed(&mut work.report, error.into());
                            }
                        }
                    }
                    self.progress.advance(1);
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
            (
                self.prefetch_digests(discovery, sent).await,
                micros(started),
            )
        };
        let ((work, extraction), (digests, prefetch)) = tokio::join!(scans, prefetch);
        Ok(Scanned {
            work: work?,
            extraction,
            prefetch,
            digests,
        })
    }

    /// The stages after a scan — plan, fetch, evaluate, resolve digests,
    /// read images and assemble the report — shared by a run's scanned work
    /// and a quiet sweep's cached work.
    pub(super) async fn lookup_tail(
        &self,
        discovery: &Discovery,
        scanned: Scanned,
        now: DateTime<Utc>,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> Result<LookupTail> {
        let mut work = scanned.work;
        work.sort_by(|a, b| a.report.repository.cmp(&b.report.repository));
        let mut lookups = Lookups {
            digests: scanned.digests,
            ..Default::default()
        };
        let phase = PhaseTrace::new("lookup");
        let started = Instant::now();
        let plan = self.plan_lookups(&mut work, discovery).await?;
        self.fetch_lookups(&plan, &mut work, &mut lookups, now, sent)
            .await?;
        let lookup = scanned.prefetch + micros(started);
        drop(phase);
        let phase = PhaseTrace::new("policy.evaluate");
        let (eval_keys, proposal_cpu_time) =
            self.evaluate_lookups(&mut work, &lookups, now).await?;
        drop(phase);
        let started = Instant::now();
        let phase = PhaseTrace::new("digest");
        let hinted_tags = self.resolve_digests(eval_keys, &mut lookups, sent).await?;
        let digest = micros(started);
        drop(phase);
        self.save_lookup_hints(&plan, &hinted_tags).await?;
        let started = Instant::now();
        let phase = PhaseTrace::new("image");
        self.read_images(&work, &mut lookups, sent).await?;
        let image = micros(started);
        drop(phase);
        let started = Instant::now();
        let _phase = PhaseTrace::new("policy.report");
        self.assemble_reports(&mut work, &lookups, sent, now).await;
        self.save_catalog_controls(&work, now).await?;
        let policy = proposal_cpu_time + micros(started);
        // The sweep replays this stage against its stored answer: hash each
        // row now, before the write path adds its tooling and warnings.
        for work in &mut work {
            work.lookup_hash = crate::sweep::hash(&work.report)?;
        }
        Ok(LookupTail {
            lookups,
            work,
            lookup,
            digest,
            image,
            policy,
        })
    }

    /// What the batch fetches and checks for `dep`, decided once its
    /// evaluator exists. `retried` marks a manual retry this run.
    fn needs(&self, repo: &Work, dep: &Dependency, retried: bool) -> Needs {
        let config = repo.config.as_ref();
        // The key and query are kept even without an evaluator: another
        // repository's dependency may have fetched what they share.
        let catalog_key = Some(self.catalog_key(dep, config));
        let advisory = patchula_lookup::advisory_query(dep);
        let Some(eval) = &repo.evaluator else {
            return Needs {
                catalog_key,
                advisory,
                revalidate: retried,
                ..Needs::default()
            };
        };
        let ignored = eval.ignored(dep);
        let versions = eval.needs_lookup(dep);
        let forge = eval.needs_forge_catalog(dep);
        let digest_policy = eval.digest_policy_applies(dep);
        let current_ref = (dep.proposing().pin.commit
            && dep.source == Source::GithubTags
            && !(dep.digest.is_some() && dep.resolved.is_none())
            && digest_policy
            && !versions)
            .then(|| {
                (
                    dep.package.clone(),
                    dep.resolved.as_deref().unwrap_or(&dep.current).to_owned(),
                )
            });
        let image = matches!(dep.source, Source::Docker | Source::Oci);
        Needs {
            wait_hours: if versions || forge {
                eval.catalog_wait_hours(dep)
            } else {
                0
            },
            catalog_key,
            versions,
            forge,
            advisory,
            current_ref,
            digest_policy,
            ignored,
            image,
            sboms: image
                && !ignored
                && dep.digest.is_some()
                && self.advisories
                && config
                    .is_some_and(|config| config.security.advisories && config.security.images),
            revalidate: retried,
        }
    }

    /// The OSV query `needs`' advisories are fetched under this run: the
    /// repository's settings must allow advisory lookups and policy must not
    /// ignore the dependency. The raw query stays on `needs` either way,
    /// since a batch-shared result may still be read for it.
    fn advisory_fetch<'a>(
        &self,
        repo: &Work,
        needs: &'a Needs,
    ) -> Option<&'a patchula_datasource::AdvisoryQuery> {
        if self.advisories
            && repo.evaluator.is_some()
            && repo
                .config
                .as_ref()
                .is_some_and(|config| config.security.advisories)
            && !needs.ignored
        {
            needs.advisory.as_ref()
        } else {
            None
        }
    }

    /// Stage: plan — each repository's evaluator, each dependency's needs,
    /// the fetch key sets, and this run's lookup hints.
    pub(super) async fn plan_lookups(
        &self,
        work: &mut [Work],
        discovery: &Discovery,
    ) -> Result<Plan> {
        let mut plan = Plan::default();
        for repo in work.iter_mut() {
            let retried = self.retried(repo).await?;
            repo.evaluator = repo
                .config
                .as_ref()
                .map(Evaluator::new)
                .transpose()
                .map_err(Error::Invalid)?;
            let needs: Vec<Needs> = repo
                .dependencies
                .iter()
                .enumerate()
                .map(|(i, dep)| self.needs(repo, &dep.dependency, retried[i]))
                .collect();
            for (dep, needs) in repo.dependencies.iter_mut().zip(needs) {
                dep.needs = needs;
            }
        }
        // Catalog reads wait for authoritative policy and checkbox state. Only
        // digest hints are speculative, so a warm cache hit sends no list read.
        for repo in work.iter() {
            for dep in &repo.dependencies {
                // A retry re-reads the catalog even when this dependency
                // shares the key without initiating the fetch itself.
                if dep.needs.revalidate
                    && let Some(key) = &dep.needs.catalog_key
                {
                    plan.forced.insert(key.clone());
                }
                if (dep.needs.versions || dep.needs.forge)
                    && let Some(key) = &dep.needs.catalog_key
                {
                    plan.keys.insert(key.clone());
                    let hours = dep.needs.wait_hours;
                    plan.waits
                        .entry(key.clone())
                        .and_modify(|wait| *wait = (*wait).min(hours))
                        .or_insert(hours);
                }
                if let Some(query) = self.advisory_fetch(repo, &dep.needs) {
                    flagged(&mut plan.advisories, query.clone(), dep.needs.revalidate);
                }
                if let Some(reference) = &dep.needs.current_ref {
                    flagged(
                        &mut plan.current_refs,
                        reference.clone(),
                        dep.needs.revalidate,
                    );
                }
            }
            if repo.evaluator.is_none() {
                continue;
            }
            for locked in &repo.locked {
                let dep = locked.dependency();
                let needs = self.needs(repo, &dep, false);
                if let Some(query) = self.advisory_fetch(repo, &needs) {
                    flagged(&mut plan.advisories, query.clone(), false);
                }
            }
        }
        // Replace per-repository hints only after successful extraction; the
        // mutable tags join them after digest resolution names the set.
        for repo in work.iter() {
            if repo.evaluator.is_some() && repo.report.status == Status::Complete {
                let catalogs = repo
                    .dependencies
                    .iter()
                    .filter(|dep| dep.needs.versions || dep.needs.forge)
                    .filter_map(|dep| dep.needs.catalog_key.clone())
                    .collect();
                let digests = plan
                    .current_refs
                    .keys()
                    .filter(|(package, _)| {
                        repo.dependencies.iter().any(|dep| &dep.package == package)
                    })
                    .cloned()
                    .collect();
                let scope = LookupHintScope {
                    repository: repo.report.repository.clone(),
                    inputs_hash: discovery.inputs_hash.clone(),
                    schema_hash: discovery.schema_hash.clone(),
                    credential_scope: discovery.credential_scope.clone(),
                };
                let packages = repo
                    .dependencies
                    .iter()
                    .map(|dep| dep.package.clone())
                    .collect();
                plan.hints.push((
                    scope,
                    LookupHint {
                        catalogs,
                        digests,
                        tags: BTreeSet::new(),
                    },
                    packages,
                ));
            }
        }
        Ok(plan)
    }

    /// Write the hints `plan_lookups` computed, with the mutable tag keys
    /// `resolve_digests` returned. Hints are replaced after a successful
    /// run's resolution, so a hint that outlives its run names only what a
    /// later run still wants.
    pub(super) async fn save_lookup_hints(
        &self,
        plan: &Plan,
        tags: &BTreeSet<DigestKey>,
    ) -> Result<()> {
        for (scope, hint, packages) in &plan.hints {
            let mut hint = hint.clone();
            hint.tags = tags
                .iter()
                .filter(|(package, _)| packages.contains(package))
                .cloned()
                .collect();
            self.cache.set_state(scope, &hint).await?;
        }
        Ok(())
    }

    /// Stage: fetch — aged catalogs, current-ref digests, and OSV advisories;
    /// then the vulnerable catalog refetch and lockfile promotions a
    /// vulnerable package earns.
    pub(super) async fn fetch_lookups(
        &self,
        plan: &Plan,
        work: &mut [Work],
        lookups: &mut Lookups,
        now: DateTime<Utc>,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> Result<()> {
        // Hints may refer to dependencies removed by the new commit/config.
        // A ref under a manual retry is read again even when a hint has it.
        let current_refs: BTreeMap<DigestKey, bool> = plan
            .current_refs
            .iter()
            .filter(|(key, revalidate)| **revalidate || !lookups.digests.contains_key(*key))
            .map(|(key, revalidate)| (key.clone(), *revalidate))
            .collect();
        self.progress
            .stage(Stage::Lookup, plan.keys.len() + current_refs.len());
        let mut plain_queries = BTreeSet::new();
        let mut fresh_queries = BTreeSet::new();
        for (query, revalidate) in &plan.advisories {
            if *revalidate {
                fresh_queries.insert(query.clone());
            } else {
                plain_queries.insert(query.clone());
            }
        }
        let advisories = async {
            if plan.advisories.is_empty() {
                return self.sources.advisories(&BTreeSet::new()).await;
            }
            let fresh = self.sources.revalidating();
            let (plain, fresh) = tokio::join!(
                self.sources.advisories(&plain_queries),
                fresh.advisories(&fresh_queries),
            );
            let mut merged = plain?;
            merged.extend(fresh?);
            Ok(merged)
        };
        let fresh_sources = &self.sources.revalidating();
        let (remaining, refs, advisories) = tokio::join!(
            self.age_catalogs(
                plan.keys.clone(),
                &plan.waits,
                &plan.forced,
                &self.sources,
                now
            ),
            self.digests(current_refs, fresh_sources, true, sent),
            advisories
        );
        lookups.advisories = advisories.unwrap_or_else(|error| {
            for repo in work.iter_mut() {
                if repo.evaluator.is_some()
                    && repo.dependencies.iter().any(|dep| {
                        !dep.needs.ignored
                            && dep
                                .needs
                                .advisory
                                .as_ref()
                                .is_some_and(|q| plan.advisories.contains_key(q))
                    })
                {
                    repo.report
                        .warnings
                        .push(format!("advisory lookup on OSV.dev failed: {error}"));
                }
            }
            BTreeMap::new()
        });
        let (remaining, evidence, credentialed) = remaining?;
        lookups.catalogs.extend(remaining);
        lookups.evidence.extend(evidence);
        lookups.credentialed.extend(credentialed);
        lookups.digests.extend(refs);
        // A lockfile-only package joins the dependencies once it is vulnerable.
        for repo in work.iter_mut() {
            if repo.evaluator.is_none() {
                continue;
            }
            let vulnerable: Vec<Dependency> = repo
                .locked
                .iter()
                .map(patchula_manager::Locked::dependency)
                .filter(|dep| {
                    patchula_lookup::advisory_query(dep)
                        .and_then(|q| lookups.advisories.get(&q))
                        .is_some_and(|found| !found.is_empty())
                })
                .collect();
            for dependency in vulnerable {
                let needs = self.needs(repo, &dependency, false);
                repo.dependencies.push(Dep {
                    dependency,
                    needs,
                    evaluation: Evaluation::default(),
                });
            }
        }
        // A vulnerable dependency's fix may be newer than a catalog reused
        // under its release-age wait, so those catalogs are read again. So is a
        // forge action's: its tags may have moved, and their commits are its pins.
        let mut vulnerable = BTreeSet::new();
        for repo in work.iter() {
            for dep in &repo.dependencies {
                let Some(key) = &dep.needs.catalog_key else {
                    continue;
                };
                if !lookups.evidence.get(key).is_some_and(|e| e.reused) {
                    continue;
                }
                let advisory_hit = dep.needs.versions
                    && dep
                        .needs
                        .advisory
                        .as_ref()
                        .and_then(|q| lookups.advisories.get(q))
                        .is_some_and(|found| !found.is_empty());
                if dep.needs.forge || advisory_hit {
                    vulnerable.insert(key.clone());
                }
            }
        }
        if !vulnerable.is_empty() {
            self.progress.stage(Stage::Lookup, vulnerable.len());
            let (fresh, evidence, credentialed) = self
                .age_catalogs(
                    vulnerable,
                    &BTreeMap::new(),
                    &BTreeSet::new(),
                    &self.sources,
                    now,
                )
                .await?;
            // The fresh reads win over the aged ones they replace.
            lookups.catalogs.extend(fresh);
            lookups.evidence.extend(evidence);
            lookups.credentialed.extend(credentialed);
        }
        Ok(())
    }

    /// Stage: evaluate — policy proposals per dependency, held targets,
    /// permit pruning, and the digest and image keys to resolve. Returns the
    /// keys and the evaluation loop's CPU time in microseconds.
    pub(super) async fn evaluate_lookups(
        &self,
        work: &mut [Work],
        lookups: &Lookups,
        now: DateTime<Utc>,
    ) -> Result<(EvalKeys, u64)> {
        let started = Instant::now();
        let mut keys = EvalKeys::default();
        // Per repository, the targets held back this run, or `None` when a
        // lookup failed and the set may be missing some: saved age permits
        // are pruned to these, so a permit lasts while its version is held.
        let mut held_targets = Vec::with_capacity(work.len());
        for repo in work.iter_mut() {
            let mut held = Some(BTreeSet::new());
            let eval = repo.evaluator.as_ref();
            for dep in repo.dependencies.iter_mut() {
                let mut evaluation = Evaluation::default();
                if let Some(eval) = eval
                    && !dep.needs.ignored
                {
                    let found = dep.needs.advisory.as_ref().and_then(|query| {
                        lookups
                            .advisories
                            .get(query)
                            .map(|found| (query, found.as_slice()))
                    });
                    let catalog = dep
                        .needs
                        .catalog_key
                        .as_ref()
                        .and_then(|key| lookups.catalogs.get(key));
                    match catalog {
                        Some(Ok(catalog)) => {
                            match eval.evaluate_with(
                                &dep.dependency,
                                catalog,
                                now,
                                &BTreeSet::new(),
                                found,
                            ) {
                                Ok(p) => {
                                    let dep_key =
                                        patchula_update::identity::extracted_key(&dep.dependency);
                                    if let Some(held) = &mut held {
                                        held.extend(p.held_back.iter().flat_map(|h| {
                                            h.versions
                                                .iter()
                                                .chain(std::iter::once(&h.version))
                                                .map(|v| {
                                                    patchula_update::identity::held_target(
                                                        &dep_key,
                                                        v,
                                                        h.update_type.as_str(),
                                                    )
                                                })
                                        }));
                                    }
                                    let overrides: BTreeSet<_> = p
                                        .held_back
                                        .iter()
                                        .flat_map(|h| {
                                            h.versions
                                                .iter()
                                                .chain(std::iter::once(&h.version))
                                                .map(|v| (v.clone(), h.update_type.to_string()))
                                        })
                                        .filter(|(v, k)| {
                                            let target = patchula_update::identity::held_target(
                                                &dep_key, v, k,
                                            );
                                            repo.controls.create.contains(&target)
                                                || repo.controls.age_permits.contains(&target)
                                        })
                                        .collect();
                                    if overrides.is_empty() {
                                        evaluation = p;
                                    } else {
                                        match eval.evaluate_with(
                                            &dep.dependency,
                                            catalog,
                                            now,
                                            &overrides,
                                            found,
                                        ) {
                                            Ok(p) => {
                                                for (v, k) in overrides {
                                                    repo.controls.age_permits.insert(
                                                        patchula_update::identity::held_target(
                                                            &dep_key, &v, &k,
                                                        ),
                                                    );
                                                }
                                                evaluation = p;
                                            }
                                            Err(e) => {
                                                held = None;
                                                repo.report
                                                    .warnings
                                                    .push(format!("{}: {e}", dep.name));
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    held = None;
                                    repo.report.warnings.push(format!("{}: {e}", dep.name));
                                }
                            }
                        }
                        Some(Err(error)) => {
                            held = None;
                            repo.report.lookup_failures.push(lookup_failure(
                                &dep.dependency,
                                error,
                                error.message.clone(),
                            ));
                        }
                        // A dependency that needs no lookup holds nothing back.
                        None => {}
                    }
                    if dep.needs.image {
                        for proposal in &evaluation.updates {
                            if eval.needs_digest(&dep.dependency, proposal)
                                && let Some(tag) = &proposal.new_version
                            {
                                flagged(
                                    &mut keys.images,
                                    (dep.package.clone(), tag.clone()),
                                    dep.needs.revalidate,
                                );
                            }
                        }
                        if dep.needs.digest_policy {
                            flagged(
                                &mut keys.images,
                                (dep.package.clone(), dep.current.clone()),
                                dep.needs.revalidate,
                            );
                        }
                    }
                    if dep.proposing().pin.commit {
                        for proposal in &evaluation.updates {
                            if eval.needs_digest(&dep.dependency, proposal)
                                && let Some(reference) = proposal
                                    .new_version
                                    .as_deref()
                                    .or(proposal.new_value.as_deref())
                            {
                                flagged(
                                    &mut keys.digests,
                                    (dep.package.clone(), reference.to_owned()),
                                    dep.needs.revalidate,
                                );
                            }
                        }
                        if dep.digest.is_some() && dep.resolved.is_none() {
                            repo.report.warnings.push(format!(
                                "{}: SHA pin has no version comment; replacement tag is unknown",
                                dep.name
                            ));
                        } else if dep.source == Source::GithubTags || dep.needs.forge {
                            // A forge resolves only tags, which its catalog
                            // already carries; branch refs stay unpinned.
                            let reference = dep.resolved.as_deref().unwrap_or(&dep.current);
                            if dep.needs.digest_policy {
                                flagged(
                                    &mut keys.digests,
                                    (dep.package.clone(), reference.to_owned()),
                                    dep.needs.revalidate,
                                );
                            }
                        }
                    }
                }
                dep.evaluation = evaluation;
            }
            held_targets.push(held);
        }
        let proposal_cpu_time = micros(started);
        for (repo, held) in work.iter().zip(&held_targets) {
            if let Some(inventory) = &repo.inventory {
                let permits = match held {
                    Some(held) => repo
                        .controls
                        .age_permits
                        .intersection(held)
                        .cloned()
                        .collect(),
                    None => repo.controls.age_permits.clone(),
                };
                patchula_update::dashboard::save_permits(
                    &self.cache,
                    &repo.report.repository,
                    &inventory.actor,
                    permits,
                )
                .await?;
            }
        }
        Ok((keys, proposal_cpu_time))
    }

    /// Stage: resolve — catalog-carried digests, mutable-tag GraphQL, ref
    /// digests, and image tag digests. Returns the mutable tag keys this
    /// run wanted, for the next run's prefetch hint.
    pub(super) async fn resolve_digests(
        &self,
        mut keys: EvalKeys,
        lookups: &mut Lookups,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> Result<BTreeSet<DigestKey>> {
        // The revalidated complete tag namespace already carries peeled target
        // commits. Reuse those for both current and proposed action tags instead
        // of adding a serial /commits/{tag} round trip after policy evaluation.
        // Digests a catalog fetched this run carries: fresh, even on a retry.
        let mut carried = BTreeSet::new();
        for (package, reference) in keys.digests.keys() {
            if let Some(Ok(catalog)) = lookups.catalogs.iter().find_map(|(key, result)| {
                (key.1.name == *package
                    && (key.1.source == Source::ForgeTags
                        || (key.1.source == Source::GithubTags
                            && lookups.evidence.get(key).is_none_or(|e| !e.reused))))
                .then_some(result)
            }) && let Some(digest) = catalog
                .versions
                .iter()
                .find(|version| &version.version == reference)
                .and_then(|version| version.digest.as_ref())
            {
                lookups
                    .digests
                    .insert((package.clone(), reference.clone()), Ok(digest.clone()));
                carried.insert((package.clone(), reference.clone()));
            }
        }
        // Every key the reused-catalog rule sends to the batched tag read is
        // next run's prefetch hint, whether a prefetch answered it this run
        // or the read below did: hinting only sent keys would drop the very
        // keys the prefetch just resolved.
        let hinted_tags: BTreeSet<DigestKey> = keys
            .digests
            .keys()
            .filter(|(package, _)| {
                lookups.evidence.iter().any(|(key, e)| {
                    key.1.source == Source::GithubTags && key.1.name == *package && e.reused
                })
            })
            .cloned()
            .collect();
        // A ref under a manual retry is read again when only a prior run's
        // hint holds it; one a catalog fetched this run carries is fresh.
        keys.digests.retain(|key, revalidate| {
            !lookups.digests.contains_key(key) || (*revalidate && !carried.contains(key))
        });
        // Forge refs resolve only through their tag catalog. A ref missing
        // from it is not sent to GitHub.
        keys.digests.retain(|(package, reference), _| {
            let Some(result) = lookups.catalogs.iter().find_map(|(key, result)| {
                (key.1.source == Source::ForgeTags && key.1.name == *package).then_some(result)
            }) else {
                return true;
            };
            if result.is_ok() {
                lookups.digests.insert(
                    (package.clone(), reference.clone()),
                    Err(Failure {
                        kind: LookupErrorKind::NotFound,
                        message: format!("tag {reference} not found"),
                    }),
                );
            }
            false
        });
        self.progress
            .stage(Stage::Digests, keys.digests.len() + keys.images.len());
        // Mutable tags, other refs and image digests are independent, so they
        // resolve side by side rather than in three waves.
        let (mutable, rest): (Vec<_>, Vec<_>) =
            keys.digests.into_iter().partition(|((package, _), _)| {
                lookups.evidence.iter().any(|(key, e)| {
                    key.1.source == Source::GithubTags && key.1.name == *package && e.reused
                })
            });
        let mutable_tags: BTreeSet<DigestKey> = mutable.into_iter().map(|(key, _)| key).collect();
        let digest_keys: BTreeMap<DigestKey, bool> = rest.into_iter().collect();
        let tags = async {
            let found = self.sources.fresh_tag_digests(&mutable_tags).await;
            self.progress.advance(mutable_tags.len());
            found
        };
        let fresh = self.sources.revalidating();
        let fresh_sources = &fresh;
        let refs = self.digests(digest_keys, fresh_sources, true, sent);
        let images = stream::iter(keys.images)
            .map(|((package, tag), revalidate)| async move {
                let sources = if revalidate {
                    fresh_sources
                } else {
                    &self.sources
                };
                let (result, credentialed) =
                    patchula_http::tracked(Box::pin(sources.registry_digest(&package, &tag))).await;
                if credentialed {
                    sent.lock()
                        .expect("sent packages")
                        .insert(Sent::Image(package.clone()));
                }
                let result = result.map_err(Failure::from);
                self.progress.advance(1);
                ((package, tag), result)
            })
            .buffer_unordered(DIGEST_FAN_OUT)
            .collect::<BTreeMap<_, _>>();
        let (tags, refs, image_digests) = futures_util::future::join3(tags, refs, images).await;
        lookups.digests.extend(tags);
        lookups.digests.extend(refs);
        lookups.image_digests = image_digests;
        Ok(hinted_tags)
    }

    /// Stage: images — SBOM reads for pinned digests and the OSV
    /// image-advisory comparison for replacements.
    pub(super) async fn read_images(
        &self,
        work: &[Work],
        lookups: &mut Lookups,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> Result<()> {
        // Every pinned image's current digest is read, so the dashboard shows a
        // gap before the first rebuild; OSV is asked only about replacements.
        let mut image_reads = BTreeMap::new();
        let mut image_pairs = BTreeSet::new();
        let mut image_deps = Vec::new();
        for repo in work {
            let (Some(eval), Some(config)) = (&repo.evaluator, &repo.config) else {
                continue;
            };
            for dep in &repo.dependencies {
                if !dep.needs.sboms {
                    continue;
                }
                let Some(old) = dep.dependency.digest.as_ref() else {
                    continue;
                };
                let current = lookups
                    .image_digests
                    .get(&(dep.package.clone(), dep.current.clone()))
                    .and_then(|r| r.as_ref().ok())
                    .filter(|new| {
                        eval.digest_proposal(&dep.dependency, new)
                            .is_ok_and(|p| p.is_some())
                    });
                let tagged = dep
                    .evaluation
                    .updates
                    .iter()
                    .filter(|p| eval.needs_digest(&dep.dependency, p))
                    .filter_map(|p| p.new_version.clone())
                    .filter_map(|tag| lookups.image_digests.get(&(dep.package.clone(), tag)))
                    .filter_map(|r| r.as_ref().ok());
                let mut digests = vec![old.clone()];
                for new in current.into_iter().chain(tagged).filter(|new| *new != old) {
                    image_pairs.insert((dep.package.clone(), old.clone()));
                    image_pairs.insert((dep.package.clone(), new.clone()));
                    digests.push(new.clone());
                }
                for digest in &digests {
                    for platform in &config.security.image_platforms {
                        flagged(
                            &mut image_reads,
                            (dep.package.clone(), digest.clone(), platform.clone()),
                            dep.needs.revalidate,
                        );
                    }
                }
                image_deps.push((
                    dep.package.clone(),
                    digests,
                    &config.security.image_platforms,
                    BTreeSet::new(),
                    dep.needs.revalidate,
                ));
            }
        }
        let fresh = self.sources.revalidating();
        let fresh_sources = &fresh;
        let read_sboms = |reads: BTreeMap<(String, String, String), bool>| {
            stream::iter(reads)
                .map(|((package, digest, platform), revalidate)| async move {
                    let sources = if revalidate {
                        fresh_sources
                    } else {
                        &self.sources
                    };
                    let (result, credentialed) = patchula_http::tracked(Box::pin(
                        sources.image_sbom(&package, &digest, &platform),
                    ))
                    .await;
                    if credentialed {
                        sent.lock()
                            .expect("sent packages")
                            .insert(Sent::Image(package.clone()));
                    }
                    let result = result.map_err(|e| e.to_string());
                    ((package, digest, platform), result)
                })
                .buffer_unordered(SBOM_FAN_OUT)
                .collect::<BTreeMap<_, _>>()
        };
        let mut image_sboms = read_sboms(image_reads).await;
        // An image with none of the configured platforms is read for the first
        // platform its index lists, on every digest of the dependency.
        let mut fallback_reads = BTreeMap::new();
        for (package, digests, configured, listed, revalidate) in &mut image_deps {
            for digest in digests.iter() {
                let side = image_side(
                    &image_sboms,
                    package.as_str(),
                    digest,
                    configured.as_slice(),
                );
                let Some(platform) = patchula_lookup::images::fallback(&side) else {
                    continue;
                };
                for digest in digests.iter() {
                    let key = (package.clone(), digest.clone(), platform.clone());
                    if !image_sboms.contains_key(&key) {
                        flagged(&mut fallback_reads, key, *revalidate);
                    }
                }
                listed.insert(platform);
            }
        }
        image_sboms.extend(read_sboms(fallback_reads).await);
        // A retried image asks OSV for its listings again, as it re-reads
        // its SBOMs and digests.
        let retried_images: BTreeSet<&str> = image_deps
            .iter()
            .filter(|(.., revalidate)| *revalidate)
            .map(|(package, ..)| package.as_str())
            .collect();
        let (mut image_queries, mut retried_queries) = (BTreeSet::new(), BTreeSet::new());
        for ((package, digest, _), result) in &image_sboms {
            if !image_pairs.contains(&(package.clone(), digest.clone())) {
                continue;
            }
            let Ok(patchula_datasource::ImageSbom::Found(sbom)) = result else {
                continue;
            };
            let target = if retried_images.contains(package.as_str()) {
                &mut retried_queries
            } else {
                &mut image_queries
            };
            target.extend(sbom.queries());
        }
        image_queries.retain(|query| !retried_queries.contains(query));
        let image_queries_all: BTreeSet<_> =
            image_queries.union(&retried_queries).cloned().collect();
        // Only an advisory one side of a replacement has and the other lacks
        // can be fixed or brought in, so only those records are read.
        // Each replacement is judged on its own repository's platforms.
        let replacements: Vec<(String, String, String, BTreeSet<String>)> = image_deps
            .iter()
            .flat_map(|(package, digests, configured, listed, _)| {
                let (old, news) = digests.split_first().expect("a dependency's own digest");
                let platforms: BTreeSet<String> =
                    configured.iter().chain(listed).cloned().collect();
                news.iter()
                    .map(|new| (package.clone(), old.clone(), new.clone(), platforms.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        // Which advisories a rebuild leaves behind comes from the IDs alone, but
        // only a record read in full marks it a security fix. The most severe
        // few it fixes or brings in are read; when every fix read is
        // withdrawn, the next ones are read in rank order until one isn't.
        let ((image_listing, image_found), osv_failure) = if image_queries_all.is_empty() {
            (Default::default(), None)
        } else {
            let read = async {
                let (plain, retried) = tokio::join!(
                    self.sources.advisory_listing(&image_queries),
                    fresh_sources.advisory_listing(&retried_queries),
                );
                let mut listing = plain?;
                listing.extend(retried?);
                let mut fixes = Vec::new();
                let mut wanted = BTreeSet::new();
                for (package, old, new, platforms) in &replacements {
                    let (mut fixed, mut introduced) = (BTreeSet::new(), BTreeSet::new());
                    for platform in platforms {
                        let before = listed_ids(&image_sboms, &listing, package, old, platform);
                        let after = listed_ids(&image_sboms, &listing, package, new, platform);
                        fixed.extend(before.difference(&after).cloned());
                        introduced.extend(after.difference(&before).cloned());
                    }
                    let ranked = |ids: &BTreeSet<String>| -> Vec<String> {
                        patchula_lookup::images::most_severe(ids.iter().map(String::as_str))
                            .into_iter()
                            .map(str::to_owned)
                            .collect()
                    };
                    wanted.extend(
                        ranked(&introduced)
                            .into_iter()
                            .take(patchula_lookup::images::SHOWN),
                    );
                    fixes.push(ranked(&fixed));
                }
                let mut found = patchula_lookup::images::Found::new();
                let mut fetched = BTreeSet::<String>::new();
                let mut verified = BTreeSet::<String>::new();
                loop {
                    for ranked in &fixes {
                        if ranked.iter().any(|id| verified.contains(id)) {
                            continue;
                        }
                        wanted.extend(
                            ranked
                                .iter()
                                .filter(|id| !fetched.contains(*id))
                                .take(patchula_lookup::images::SHOWN)
                                .cloned(),
                        );
                    }
                    let batch: BTreeSet<String> = wanted.difference(&fetched).cloned().collect();
                    if batch.is_empty() {
                        break;
                    }
                    let records = self
                        .sources
                        .advisory_records(&listing, |id| batch.contains(id))
                        .await?;
                    for (query, advisories) in records {
                        verified.extend(advisories.iter().map(|a| a.id.clone()));
                        found.entry(query).or_default().extend(advisories);
                    }
                    fetched.extend(batch);
                }
                for ids in listing.values_mut() {
                    ids.retain(|(id, _)| !fetched.contains(id) || verified.contains(id));
                }
                Ok::<_, patchula_datasource::Error>((listing, found))
            };
            match read.await {
                Ok(read) => (read, None),
                Err(error) => (Default::default(), Some(format!("OSV.dev failed: {error}"))),
            }
        };
        lookups.image_sboms = image_sboms;
        lookups.image_listing = image_listing;
        lookups.image_found = image_found;
        lookups.osv_failure = osv_failure;
        Ok(())
    }

    /// The digests for `keys`, the `revalidate` ones through `fresh`; `counted`
    /// advances the current progress stage per key. Adds to `sent` each
    /// package whose read carried credentials.
    async fn digests(
        &self,
        keys: BTreeMap<DigestKey, bool>,
        fresh: &Sources,
        counted: bool,
        sent: &Mutex<BTreeSet<Sent>>,
    ) -> BTreeMap<DigestKey, std::result::Result<String, Failure>> {
        stream::iter(keys)
            .map(|((package, reference), revalidate)| async move {
                let sources = if revalidate { fresh } else { &self.sources };
                let (result, credentialed) =
                    patchula_http::tracked(Box::pin(sources.digest(&package, &reference))).await;
                if credentialed {
                    sent.lock()
                        .expect("sent packages")
                        .insert(Sent::Git(package.clone()));
                }
                let result = result.map_err(Failure::from);
                if counted {
                    self.progress.advance(1);
                }
                ((package, reference), result)
            })
            .buffer_unordered(DIGEST_FAN_OUT)
            .collect()
            .await
    }
}
