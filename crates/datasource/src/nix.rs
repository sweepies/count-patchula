//! Flake inputs as nix resolves them: `nix flake prefetch` reports the
//! revision a reference points at now and when that commit was made.
use super::*;
use patchula_cache::ExtractionKey;
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

/// A stored resolution is reused for this long, like a mise listing.
const FRESH_MS: i64 = 3_600_000;

#[derive(Serialize, Deserialize)]
struct Resolved {
    fetched_at: i64,
    revision: String,
    last_modified: Option<i64>,
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
        let key = ExtractionKey {
            repo: format!("nix-prefetch:{reference}"),
            default_branch_commit: self.github.credential_scope().into(),
            config_hash: "nix-prefetch-v1".into(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let saved = match self.cache.extraction_get(&key).await? {
            Some(bytes) if !self.http.is_revalidating() => {
                serde_json::from_slice::<Resolved>(&bytes)
                    .ok()
                    .filter(|saved| (0..FRESH_MS).contains(&now.saturating_sub(saved.fetched_at)))
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
                self.cache
                    .extraction_put(&key, &serde_json::to_vec(&resolved)?)
                    .await?;
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
