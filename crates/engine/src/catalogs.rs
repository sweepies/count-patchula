//! Policy-bounded catalog views. Evaluation and mutable digests stay fresh.
use super::*;
use chrono::{DateTime, SecondsFormat, Utc};
use patchula_datasource::Catalog;
use patchula_report::CatalogEvidence;

/// Saved catalogs read at once while deciding what to refetch.
const READ_FAN_OUT: usize = 4;

#[derive(Serialize, Deserialize)]
struct Saved {
    fetched_at: i64,
    catalog: Catalog,
    credentialed: bool,
}
#[derive(Serialize)]
struct CatalogScope {
    versions_host: bool,
    package: PackageId,
    credential_scope: String,
}
impl State for Saved {
    const NAMESPACE: &'static str = "catalog";
    const VERSION: u32 = 1;
    type Scope = CatalogScope;
}
pub(super) type Evidence = BTreeMap<LookupKey, CatalogEvidence>;

fn reuse_hours(wait_hours: u32) -> Option<u32> {
    (wait_hours != 0).then(|| wait_hours.saturating_sub(1).clamp(1, 24))
}
fn eligible(fetched: i64, now: i64, hours: u32) -> bool {
    now >= fetched && now.saturating_sub(fetched) < i64::from(hours) * 3_600_000
}
impl Engine {
    fn catalog_scope(&self, key: &LookupKey) -> CatalogScope {
        CatalogScope {
            versions_host: key.0,
            package: key.1.clone(),
            credential_scope: self.github.credential_scope().into(),
        }
    }
    pub(super) async fn age_catalogs(
        &self,
        keys: BTreeSet<LookupKey>,
        waits: &BTreeMap<LookupKey, u32>,
        force: &BTreeSet<LookupKey>,
        sources: &Sources,
        now: DateTime<Utc>,
    ) -> Result<(Catalogs, Evidence, BTreeSet<LookupKey>)> {
        let mut results = Catalogs::new();
        let mut evidence = Evidence::new();
        let mut credentialed = BTreeSet::new();
        let mut ordinary = BTreeSet::new();
        let mut forced = BTreeSet::new();
        let cached = stream::iter(keys)
            .map(|key| async move {
                let saved = if !force.contains(&key)
                    && reuse_hours(*waits.get(&key).unwrap_or(&0)).is_some()
                {
                    self.cache.state::<Saved>(&self.catalog_scope(&key)).await?
                } else {
                    None
                };
                Ok::<_, Error>((key, saved))
            })
            .buffer_unordered(READ_FAN_OUT)
            .collect::<Vec<_>>()
            .await;
        for cached in cached {
            let (key, saved) = cached?;
            if let Some(saved) = saved.filter(|saved| {
                reuse_hours(*waits.get(&key).unwrap_or(&0))
                    .is_some_and(|hours| eligible(saved.fetched_at, now.timestamp_millis(), hours))
            }) {
                let fetched_at = DateTime::from_timestamp_millis(saved.fetched_at)
                    .ok_or_else(|| Error::Invalid("invalid catalog timestamp".into()))?;
                self.progress.advance(1);
                if saved.credentialed {
                    credentialed.insert(key.clone());
                }
                results.insert(key.clone(), Ok(saved.catalog));
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
        let fresh = sources.revalidating();
        let ((mut fetched, sent), (forced, sent_forced)) = tokio::join!(
            self.catalogs(ordinary, sources),
            self.catalogs(forced, &fresh)
        );
        fetched.extend(forced);
        credentialed.extend(sent);
        credentialed.extend(sent_forced);
        for (key, result) in &fetched {
            if let Ok(catalog) = result {
                // Quota fallbacks may expose an older registry snapshot. Never
                // reset its age by promoting it into a newly fetched view.
                if matches!(key.1.source, Source::Docker | Source::Oci)
                    && !sources.registry_stale(&key.1.name).await.is_empty()
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
                self.cache
                    .set_state(
                        &self.catalog_scope(key),
                        &Saved {
                            fetched_at: now.timestamp_millis(),
                            catalog: catalog.clone(),
                            credentialed: credentialed.contains(key),
                        },
                    )
                    .await?;
            }
        }
        results.extend(fetched);
        Ok((results, evidence, credentialed))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Controls {
    branches: BTreeMap<String, BTreeSet<String>>,
    targets: BTreeMap<String, BTreeSet<String>>,
}
impl State for Controls {
    const NAMESPACE: &'static str = "catalog-controls";
    const VERSION: u32 = 1;
    type Scope = CredentialScope;
}
impl Engine {
    fn catalog_controls_scope(&self, repo: &str) -> CredentialScope {
        CredentialScope {
            repository: repo.into(),
            credential_scope: self.github.credential_scope().into(),
        }
    }
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
                .state::<Controls>(&self.catalog_controls_scope(&repo.report.repository))
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
            let mut controls = Controls::default();
            let since = self.last_run(&repo.report.repository).await?;
            let dependencies: Vec<Dependency> = repo
                .dependencies
                .iter()
                .map(|dep| dep.dependency.clone())
                .collect();
            if let Ok(plans) = patchula_update::plan(
                &repo.report,
                config,
                &dependencies,
                &repo.snapshot,
                false,
                since,
                now,
            ) {
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
                        for version in std::iter::once(&held.version).chain(&held.versions) {
                            controls.targets.insert(
                                patchula_update::identity::held_target(
                                    &key,
                                    version,
                                    held.update_type.as_str(),
                                ),
                                BTreeSet::from([key.clone()]),
                            );
                        }
                    }
                }
            }
            self.cache
                .set_state(
                    &self.catalog_controls_scope(&repo.report.repository),
                    &controls,
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
