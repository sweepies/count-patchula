//! Resolve non-App webhooks without persisting JWTs or installation tokens.
use crate::webhook::Payload;
use patchula_cache::{Cache, installations::Mapping};
use patchula_config::settings::valid_repo;
use patchula_github::app::App;
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
    pub(crate) async fn resolve(&self, payload: &Payload) -> Result<Option<u64>, &'static str> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.resolve_at(payload, chrono::Utc::now().timestamp()),
        )
        .await
        .map_err(|_| "installation-resolution-timeout")?
    }
    pub(crate) async fn resolve_at(
        &self,
        payload: &Payload,
        now: i64,
    ) -> Result<Option<u64>, &'static str> {
        // Never fall back to an org installation for an inaccessible repository:
        // a selected-repositories App may not have access to that repository.
        let target = if payload.repository.is_some() {
            let Some(repo) = payload
                .repository
                .as_ref()
                .and_then(|repo| repo.full_name.as_deref())
                .filter(|repo| valid_repo(repo))
            else {
                return Ok(None);
            };
            format!("repos/{}", repo.to_ascii_lowercase())
        } else {
            let Some(org) = payload
                .organization
                .as_ref()
                .and_then(|org| org.login.as_deref())
                .filter(|org| valid_login(org))
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

/// A GitHub login — user or organization — as `organization.login` reports
/// it: 1–39 ASCII alphanumerics or hyphens, never starting or ending with a
/// hyphen. Repo-name rules do not apply: `_` and `.` are not login
/// characters, and a login has a length bound a repository name does not.
fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::valid_login;

    #[test]
    fn org_logins_follow_github_login_rules() {
        for login in [
            "octocat",
            "github",
            "a",
            "a-b-c",
            "Org-With-Hyphens",
            &"x".repeat(39),
        ] {
            assert!(valid_login(login), "{login}");
        }
        for login in [
            "",
            "-octocat",
            "octocat-",
            "octo.cat",
            "octo_cat",
            "octo/cat",
            "octo cat",
            &"x".repeat(40),
            "öctocat",
        ] {
            assert!(!valid_login(login), "{login}");
        }
    }
}
