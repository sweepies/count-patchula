//! Resolve non-App webhooks without persisting JWTs or installation tokens.
use crate::settings::valid_repo;
use rustovate_cache::{Cache, installations::Mapping};
use rustovate_github::app::App;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

pub(crate) const TTL_SECONDS: i64 = 300;
pub(crate) struct Resolver {
    app: Arc<App>,
    cache: Cache,
    entries: Mutex<BTreeMap<String, Mapping>>,
}
impl Resolver {
    pub(crate) fn new(app: Arc<App>, cache: Cache) -> Self {
        Self {
            app,
            cache,
            entries: Mutex::new(BTreeMap::new()),
        }
    }
    pub(crate) async fn resolve(&self, payload: &Value) -> Result<Option<u64>, &'static str> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.resolve_at(payload, chrono::Utc::now().timestamp()),
        )
        .await
        .map_err(|_| "installation-resolution-timeout")?
    }
    pub(crate) async fn resolve_at(
        &self,
        payload: &Value,
        now: i64,
    ) -> Result<Option<u64>, &'static str> {
        // Never fall back to an org installation for an inaccessible repository:
        // a selected-repositories App may not have access to that repository.
        let target = if !payload["repository"].is_null() {
            let Some(repo) = payload["repository"]["full_name"]
                .as_str()
                .filter(|repo| valid_repo(repo))
            else {
                return Ok(None);
            };
            format!("repos/{}", repo.to_ascii_lowercase())
        } else {
            let Some(org) = payload["organization"]["login"]
                .as_str()
                .filter(|org| valid_repo(&format!("{org}/placeholder")) && !org.contains('/'))
            else {
                return Ok(None);
            };
            format!("orgs/{}", org.to_ascii_lowercase())
        };
        // Single flight prevents concurrent cache misses from flooding App endpoints.
        let mut entries = self.entries.lock().await;
        entries.retain(|_, mapping| mapping.expires_at > now);
        if let Some(mapping) = entries.get(&target) {
            return Ok(Some(mapping.installation));
        }
        if entries.len() >= 4096 {
            entries.clear();
        }
        if let Some(mapping) = self
            .cache
            .installation_get(self.app.id(), target.clone(), now)
            .await
            .map_err(|error| error.kind())?
        {
            entries.insert(target, mapping);
            return Ok(Some(mapping.installation));
        }
        let installation = if let Some(repo) = target.strip_prefix("repos/") {
            self.app.repository_installation(repo).await
        } else {
            self.app
                .organization_installation(target.strip_prefix("orgs/").expect("target prefix"))
                .await
        }
        .map_err(|_| "installation-lookup-failed")?;
        if let Some(installation) = installation {
            let mapping = Mapping {
                installation,
                expires_at: now + TTL_SECONDS,
            };
            self.cache
                .installation_put(self.app.id(), target.clone(), mapping, now)
                .await
                .map_err(|error| error.kind())?;
            entries.insert(target, mapping);
        } else {
            entries.remove(&target);
            self.cache
                .installation_remove(self.app.id(), target)
                .await
                .map_err(|error| error.kind())?;
        }
        Ok(installation)
    }
}
