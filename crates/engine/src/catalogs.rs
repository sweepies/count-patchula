//! Policy-bounded catalog views. Evaluation and mutable digests stay fresh.
use super::*;
use chrono::{DateTime, SecondsFormat, Utc};
use patchula_datasource::Catalog;
use patchula_report::CatalogEvidence;

#[derive(Serialize, Deserialize)]
struct Saved {
    fetched_at: i64,
    catalog: Catalog,
}
pub(super) type Evidence = BTreeMap<LookupKey, CatalogEvidence>;

fn reuse_hours(wait_hours: u32) -> Option<u32> {
    (wait_hours != 0).then(|| wait_hours.saturating_sub(1).clamp(1, 24))
}
fn eligible(fetched: i64, now: i64, hours: u32) -> bool {
    now >= fetched && now.saturating_sub(fetched) < i64::from(hours) * 3_600_000
}
impl Engine {
    fn catalog_cache_key(&self, key: &LookupKey) -> ExtractionKey {
        ExtractionKey {
            repo: format!("catalog:{}:{}", key.0, key.1),
            default_branch_commit: self.github.credential_scope().into(),
            config_hash: format!(
                "age-catalog-v1:{}",
                hash(patchula_manager::MISE_REGISTRY.as_bytes())
            ),
        }
    }
    pub(super) async fn age_catalogs(
        &self,
        keys: BTreeSet<LookupKey>,
        waits: &BTreeMap<LookupKey, u32>,
        force: &BTreeSet<LookupKey>,
        sources: &Sources,
        now: DateTime<Utc>,
    ) -> Result<(Catalogs, Evidence)> {
        let mut results = Catalogs::new();
        let mut evidence = Evidence::new();
        let mut ordinary = BTreeSet::new();
        let mut forced = BTreeSet::new();
        let cached = stream::iter(keys)
            .map(|key| async move {
                let saved = if !force.contains(&key)
                    && reuse_hours(*waits.get(&key).unwrap_or(&0)).is_some()
                    && let Some(bytes) = self
                        .cache
                        .extraction_get(&self.catalog_cache_key(&key))
                        .await?
                {
                    tokio::task::spawn_blocking(move || {
                        serde_json::from_slice::<Saved>(&bytes).ok()
                    })
                    .await
                    .map_err(|error| Error::Invalid(error.to_string()))?
                } else {
                    None
                };
                Ok::<_, Error>((key, saved))
            })
            .buffer_unordered(4)
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
        let fresh = sources.revalidating().catalog_batch();
        let (mut fetched, forced) = tokio::join!(
            self.catalogs(ordinary, sources),
            self.catalogs(forced, &fresh)
        );
        fetched.extend(forced);
        for (key, result) in &fetched {
            if let Ok(catalog) = result {
                // Quota fallbacks may expose an older registry snapshot. Never
                // reset its age by promoting it into a newly fetched view.
                if (key.0.ends_with(":docker") || key.0.ends_with(":oci"))
                    && !sources.registry_stale(&key.1).await.is_empty()
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
                    .extraction_put(
                        &self.catalog_cache_key(key),
                        &serde_json::to_vec(&Saved {
                            fetched_at: now.timestamp_millis(),
                            catalog: catalog.clone(),
                        })?,
                    )
                    .await?;
            }
        }
        results.extend(fetched);
        Ok((results, evidence))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Controls {
    branches: BTreeMap<String, BTreeSet<String>>,
    targets: BTreeMap<String, BTreeSet<String>>,
}
impl Engine {
    fn catalog_controls_key(&self, repo: &str) -> ExtractionKey {
        ExtractionKey {
            repo: format!("catalog-controls:{repo}"),
            default_branch_commit: self.github.credential_scope().into(),
            config_hash: "dependency-controls-v1".into(),
        }
    }
    pub(super) async fn forced_catalog_keys(&self, work: &[Work]) -> Result<BTreeSet<LookupKey>> {
        let mut forced = BTreeSet::new();
        for repo in work {
            let controls = &repo.controls;
            let mut dependencies = controls.retry.clone();
            let mut all = controls.recheck;
            if !controls.branches.is_empty()
                || !controls.create.is_empty()
                || !controls.recreate.is_empty()
            {
                let saved = self
                    .cache
                    .extraction_get(&self.catalog_controls_key(&repo.report.repository))
                    .await?
                    .and_then(|bytes| serde_json::from_slice::<Controls>(&bytes).ok());
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
                                !repo.dependencies.iter().any(|dep| {
                                    patchula_update::dashboard::extracted_key(dep) == *key
                                })
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
            for dep in &repo.dependencies {
                if all || dependencies.contains(&patchula_update::dashboard::extracted_key(dep)) {
                    forced.insert(self.catalog_key(dep, repo.config.as_ref()));
                }
            }
        }
        Ok(forced)
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
            let since = self.last_run(&repo.report.repository).await;
            if let Ok(plans) = patchula_update::plan(
                &repo.report,
                config,
                &repo.dependencies,
                &repo.snapshot,
                false,
                since,
                now,
            ) {
                for plan in plans {
                    let dependencies: BTreeSet<_> = plan
                        .changes
                        .iter()
                        .flat_map(|change| {
                            repo.dependencies
                                .iter()
                                .filter(move |dep| {
                                    dep.file == change.file
                                        && dep.package == change.package
                                        && word(&dep.source) == change.source
                                })
                                .map(patchula_update::dashboard::extracted_key)
                        })
                        .collect();
                    controls
                        .branches
                        .insert(plan.action.branch.clone(), dependencies.clone());
                    controls
                        .targets
                        .insert(patchula_update::update_key(&plan), dependencies);
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
                        let key = patchula_update::dashboard::dependency_key(&file.path, dep);
                        for version in std::iter::once(&held.version).chain(&held.versions) {
                            controls.targets.insert(
                                patchula_update::dashboard::held_target(
                                    &key,
                                    version,
                                    &held.update_type,
                                ),
                                BTreeSet::from([key.clone()]),
                            );
                        }
                    }
                }
            }
            self.cache
                .extraction_put(
                    &self.catalog_controls_key(&repo.report.repository),
                    &serde_json::to_vec(&controls)?,
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
