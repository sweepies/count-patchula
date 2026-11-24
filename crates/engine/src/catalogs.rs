//! Policy-bounded catalog views. Evaluation and mutable digests stay fresh.
use super::*;
use chrono::{DateTime, SecondsFormat, Utc};
use patchula_datasource::Catalog;
use patchula_report::CatalogEvidence;

/// Saved catalogs read at once while deciding what to refetch.
const READ_FAN_OUT: usize = 4;

/// A stored catalog view. Read back owned; written borrowing the catalog
/// the run fetched, so saving it copies nothing.
#[derive(Serialize, Deserialize)]
pub(crate) struct Saved<'a> {
    fetched_at: i64,
    catalog: std::borrow::Cow<'a, Catalog>,
    credentialed: bool,
    /// The versions the lookup described: a view serves only a lookup
    /// whose candidates these cover, since the rest are listed unchecked.
    candidates: patchula_datasource::Candidates,
}
#[derive(Serialize)]
pub(crate) struct CatalogScope {
    versions_host: bool,
    package: PackageId,
    credential_scope: String,
    /// The operator registry entries the view may have been read with,
    /// as `Engine::registry_scope` digests them.
    registries: String,
    /// Whom a mise catalog was listed for; `Run` for every other source.
    access: CatalogAccess,
}
/// A key's `MiseAccess` with a repository named by its numeric id, so a
/// repository that later takes a renamed one's name never reads its views.
#[derive(Serialize)]
enum CatalogAccess {
    Run,
    Public,
    Repository(u64),
}
impl State for Saved<'_> {
    const NAMESPACE: &'static str = "catalog";
    const VERSION: u32 = 30;
    // Code-versioned: the view is datasource output decoded into `Catalog`;
    // a stale-but-decodable view from a release whose transforms moved is
    // worse than a miss.
    const CODE_VERSIONED: bool = true;
    type Scope = CatalogScope;
}
pub(super) type Evidence = BTreeMap<LookupKey, CatalogEvidence>;

fn reuse_hours(wait_hours: u32) -> Option<u32> {
    (wait_hours != 0).then(|| wait_hours.saturating_sub(1).clamp(1, 24))
}
/// Milliseconds in an hour, the unit catalog timestamps are kept in.
const MS_PER_HOUR: i64 = 3_600_000;
fn eligible(fetched: i64, now: i64, hours: u32) -> bool {
    now >= fetched && now.saturating_sub(fetched) < i64::from(hours) * MS_PER_HOUR
}
impl Engine {
    /// `key`'s stored view's scope; `None` when its repository's id cannot
    /// be read, and then nothing is reused or stored for it.
    async fn catalog_scope(&self, key: &LookupKey) -> Option<CatalogScope> {
        let access = match &key.2 {
            MiseAccess::Run => CatalogAccess::Run,
            MiseAccess::Public => CatalogAccess::Public,
            MiseAccess::Repository(repository) => {
                CatalogAccess::Repository(self.github.scoped_repository_id(repository)?.await.ok()?)
            }
        };
        Some(CatalogScope {
            versions_host: key.0,
            package: key.1.clone(),
            credential_scope: self.github.credential_scope().into(),
            registries: self.registry_scope.clone(),
            access,
        })
    }
    pub(super) async fn age_catalogs(
        &self,
        keys: BTreeSet<LookupKey>,
        waits: &BTreeMap<LookupKey, u32>,
        force: &BTreeSet<LookupKey>,
        vias: &MiseVias,
        candidates: &CatalogCandidates,
        now: DateTime<Utc>,
    ) -> Result<(Catalogs, Evidence, BTreeSet<LookupKey>)> {
        let mut results = Catalogs::new();
        let mut evidence = Evidence::new();
        let mut credentialed = BTreeSet::new();
        let mut ordinary = BTreeSet::new();
        let mut forced = BTreeSet::new();
        let cached = stream::iter(keys)
            .map(|key| async move {
                let hours = reuse_hours(*waits.get(&key).unwrap_or(&0));
                let saved = match self.catalog_scope(&key).await {
                    Some(scope) if !force.contains(&key) && hours.is_some() => {
                        self.cache.state::<Saved<'static>>(&scope).await?
                    }
                    _ => None,
                };
                Ok::<_, Error>((key, hours, saved))
            })
            .buffer_unordered(READ_FAN_OUT)
            .collect::<Vec<_>>()
            .await;
        for cached in cached {
            let (key, hours, saved) = cached?;
            let wanted = candidates.get(&key).cloned().unwrap_or_default();
            if let Some(saved) = saved.filter(|saved| {
                hours.is_some_and(|hours| eligible(saved.fetched_at, now.timestamp_millis(), hours))
                    && saved.candidates.covers(&wanted)
            }) {
                let fetched_at = DateTime::from_timestamp_millis(saved.fetched_at)
                    .ok_or_else(|| Error::Invalid("invalid catalog timestamp".into()))?;
                self.progress.advance(1);
                if saved.credentialed {
                    credentialed.insert(key.clone());
                }
                results.insert(key.clone(), Ok(saved.catalog.into_owned()));
                evidence.insert(
                    key,
                    CatalogEvidence {
                        fetched_at: fetched_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                        reused: true,
                    },
                );
            } else if force.contains(&key) {
                forced.insert(key);
            } else {
                ordinary.insert(key);
            }
        }
        let fresh = self.sources.revalidating();
        let ((mut fetched, sent), (forced, sent_forced)) = tokio::join!(
            self.catalogs(ordinary, &self.sources, vias, candidates),
            self.catalogs(forced, &fresh, vias, candidates)
        );
        fetched.extend(forced);
        credentialed.extend(sent);
        credentialed.extend(sent_forced);
        let mut views = Vec::new();
        for (key, result) in &fetched {
            if let Ok(catalog) = result {
                // Quota fallbacks may expose an older registry snapshot. Never
                // reset its age by promoting it into a newly fetched view.
                if matches!(key.1.source, Source::Docker | Source::Oci)
                    && !self.sources.registry_stale(&key.1.name).await.is_empty()
                {
                    continue;
                }
                evidence.insert(
                    key.clone(),
                    CatalogEvidence {
                        fetched_at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
                        reused: false,
                    },
                );
                let Some(scope) = self.catalog_scope(key).await else {
                    continue;
                };
                views.push((
                    scope,
                    Saved {
                        fetched_at: now.timestamp_millis(),
                        catalog: std::borrow::Cow::Borrowed(catalog),
                        credentialed: credentialed.contains(key),
                        candidates: candidates.get(key).cloned().unwrap_or_default(),
                    },
                ));
            }
        }
        self.cache.set_states(&views).await?;
        drop(views);
        results.extend(fetched);
        Ok((results, evidence, credentialed))
    }
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Controls {
    branches: BTreeMap<String, BTreeSet<String>>,
    targets: BTreeMap<String, BTreeSet<String>>,
}
impl State for Controls {
    const NAMESPACE: &'static str = "catalog-controls";
    const VERSION: u32 = 30;
    // Code-versioned: the stored keys come from `identity` and `plan`
    // code, and a key written by older code that no longer matches its
    // dependency quietly un-retries a ticked box.
    const CODE_VERSIONED: bool = true;
    type Scope = CredentialScope;
}
impl Engine {
    /// Whether each dependency is under a manual retry this run: the
    /// dashboard controls' retry box, a recheck, a ticked branch box, or a
    /// create/recreate whose saved controls name it. Parallel to
    /// `repo.dependencies`.
    pub(super) async fn retried(&self, repo: &Work) -> Result<Vec<bool>> {
        let controls = &repo.controls;
        let mut dependencies = BTreeSet::new();
        let mut all = controls.recheck;
        if !controls.branches.is_empty()
            || !controls.create.is_empty()
            || !controls.recreate.is_empty()
        {
            let saved = self
                .cache
                .state::<Controls>(&self.credential_scope(&repo.report.repository))
                .await?;
            if let Some(saved) = saved {
                for (name, entries) in controls
                    .branches
                    .iter()
                    .map(|name| (name, &saved.branches))
                    .chain(
                        controls
                            .create
                            .iter()
                            .chain(&controls.recreate)
                            .map(|name| (name, &saved.targets)),
                    )
                {
                    if let Some(keys) = entries.get(name) {
                        if keys.iter().any(|key| {
                            !repo
                                .dependencies
                                .iter()
                                .any(|dep| patchula_update::identity::extracted_key(dep) == *key)
                        }) {
                            all = true;
                        }
                        dependencies.extend(keys.iter().cloned());
                    } else {
                        all = true;
                    }
                }
            } else {
                all = true;
            }
        }
        Ok(repo
            .dependencies
            .iter()
            .map(|dep| {
                all || dependencies.contains(&patchula_update::identity::extracted_key(dep))
                    || controls
                        .retry
                        .contains(&patchula_update::identity::extracted_retry_key(dep))
            })
            .collect())
    }
    pub(super) async fn save_catalog_controls(
        &self,
        work: &[Work],
        now: DateTime<Utc>,
    ) -> Result<()> {
        for repo in work {
            let Some(config) = &repo.config else { continue };
            if repo.report.status != Status::Complete {
                continue;
            }
            let since = self.last_run(&repo.report.repository).await?;
            let dependencies: Vec<Dependency> = repo
                .dependencies
                .iter()
                .map(|dep| dep.dependency.clone())
                .collect();
            // A plan error keeps the plan-derived controls the last run saved:
            // an empty set would quietly un-retry every ticked box on the
            // next run. This run's held-back targets are saved either way.
            let (mut controls, plans) = match patchula_update::plan(
                &repo.report,
                config,
                &dependencies,
                &repo.snapshot,
                false,
                since,
                now,
            ) {
                Ok(plans) => (Controls::default(), plans),
                Err(error) => {
                    tracing::warn!(target: "patchula_engine", repository = %repo.report.repository,
                        %error, "the plan failed; the saved branch controls stand");
                    let saved = self
                        .cache
                        .state::<Controls>(&self.credential_scope(&repo.report.repository))
                        .await?
                        .unwrap_or_default();
                    (saved, Vec::new())
                }
            };
            for plan in plans {
                let dependencies: BTreeSet<_> = plan
                    .changes()
                    .iter()
                    .flat_map(|change| {
                        repo.dependencies
                            .iter()
                            .filter(move |dep| {
                                change.touches(
                                    &dep.file,
                                    dep.source,
                                    &dep.package,
                                    dep.registry.as_ref().map(|r| r.recorded()),
                                )
                            })
                            .map(|dep| patchula_update::identity::extracted_key(dep))
                    })
                    .collect();
                controls
                    .branches
                    .insert(plan.action().branch.clone(), dependencies.clone());
                controls.targets.insert(plan.update_key(), dependencies);
            }
            for file in &repo.report.package_files {
                for dep in &file.dependencies {
                    for held in dep
                        .metadata
                        .as_ref()
                        .into_iter()
                        .flat_map(|meta| &meta.held_back)
                    {
                        let key = patchula_update::identity::dependency_key(&file.path, dep);
                        for (.., target) in crate::held_targets(&key, held) {
                            controls
                                .targets
                                .insert(target, BTreeSet::from([key.clone()]));
                        }
                    }
                }
            }
            self.cache
                .set_state(&self.credential_scope(&repo.report.repository), &controls)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
