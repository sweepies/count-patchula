//! Flake inputs as nix resolves them: `nix flake prefetch` reports the
//! revision a reference points at now and when that commit was made.
use super::*;
use std::{future::Future, pin::Pin, sync::Arc};

/// Runs `nix flake prefetch --json` for one flake reference and returns its
/// output. The lockfile runner implements it with the Count's pinned nix and
/// private store; `token` is for github.com only.
pub trait Flakes: Send + Sync {
    fn prefetch<'a>(
        &'a self,
        reference: &'a str,
        token: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<Vec<u8>, String>> + Send + 'a>>;
}

#[derive(Serialize)]
struct PrefetchScope {
    reference: String,
    credential_scope: String,
}
#[derive(Serialize, Deserialize)]
struct Resolved {
    fetched_at: i64,
    revision: String,
    last_modified: Option<i64>,
}
impl State for Resolved {
    const NAMESPACE: &'static str = "nix-prefetch";
    const VERSION: u32 = 1;
    type Scope = PrefetchScope;
}

impl Client {
    /// Where nix runs. Without it (an in-memory cache has no tool store) a
    /// flake input cannot be looked up.
    pub fn with_nix(mut self, flakes: Arc<dyn Flakes>) -> Self {
        self.nix = Some(flakes);
        self
    }
    /// One version: the revision the reference resolves to, or the NAR hash
    /// of an input without one, dated by its `lastModified`.
    pub(super) async fn nix(&self, reference: &str) -> Result<Catalog> {
        let flakes = self.nix.as_ref().ok_or(Error::NoNix)?;
        let scope = PrefetchScope {
            reference: reference.into(),
            credential_scope: self.github.credential_scope().into(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let saved = match self.cache.state::<Resolved>(&scope).await? {
            Some(saved)
                if !self.http.is_revalidating()
                    && (0..FRESH_MS).contains(&now.saturating_sub(saved.fetched_at)) =>
            {
                Some(saved)
            }
            _ => None,
        };
        let resolved = match saved {
            Some(saved) => saved,
            None => {
                let token = self.mise_token().await;
                let output = flakes
                    .prefetch(reference, token.as_deref())
                    .await
                    .map_err(Error::Nix)?;
                let value: Value = serde_json::from_slice(&output)?;
                let locked = &value["locked"];
                let resolved = Resolved {
                    fetched_at: now,
                    revision: locked["rev"]
                        .as_str()
                        .or_else(|| value["hash"].as_str())
                        .ok_or(Error::Invalid)?
                        .to_owned(),
                    last_modified: locked["lastModified"].as_i64(),
                };
                self.cache.set_state(&scope, &resolved).await?;
                resolved
            }
        };
        Ok(Catalog {
            versions: vec![Version {
                version: resolved.revision.clone(),
                published_at: resolved
                    .last_modified
                    .and_then(|t| chrono::DateTime::<chrono::Utc>::from_timestamp(t, 0))
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                digest: Some(resolved.revision),
                deprecated: false,
                yanked: false,
            }],
            dist_tags: BTreeMap::new(),
        })
    }
}
