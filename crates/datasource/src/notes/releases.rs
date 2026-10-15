//! Batched, timestamp-validated GitHub release bodies.
use super::*;
use serde_json::json;

type BodyResult = std::result::Result<Option<ReleaseBody>, String>;
type RepoBodies = Arc<tokio::sync::Mutex<BTreeMap<String, BodyResult>>>;
pub(crate) type BodyCache = Arc<tokio::sync::Mutex<BTreeMap<String, RepoBodies>>>;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseBody {
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) draft: bool,
    #[serde(default)]
    pub(super) prerelease: bool,
}
#[derive(Serialize, Deserialize)]
struct SavedRelease {
    updated_at: String,
    release: ReleaseBody,
}
fn release_key(repo: &str, tag: &str) -> ExtractionKey {
    ExtractionKey {
        repo: format!("release-notes:{}", repo.to_ascii_lowercase()),
        default_branch_commit: tag.into(),
        config_hash: "release-body-updated-at-v1".into(),
    }
}
impl Client {
    /// Union selected tags before rendering so overlapping PR ranges cost one
    /// query per upstream (bounded to 100 aliases per request).
    pub async fn prepare_release_notes(&self, ranges: &[(Source, String, String, String)]) {
        let mut repos: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (source, package, current, target) in ranges {
            let Ok(Some(repository)) = self.notes_source_repository(*source, package, target).await
            else {
                continue;
            };
            let Some(repo) = repository.github.as_deref() else {
                continue;
            };
            let Some((old, new)) = note_bounds(*source, package, current, target, &repository)
            else {
                continue;
            };
            let Ok(catalog) = self.notes_list(repo, "releases").await else {
                continue;
            };
            let mut candidates: Vec<_> = catalog
                .releases
                .into_iter()
                .filter_map(|release| {
                    let version =
                        tag_version(&release.tag, package, &repository).and_then(parsed)?;
                    (in_range(&version, &old, &new)
                        && (!release.prerelease || !new.0.pre.is_empty()))
                    .then_some((version, release.tag))
                })
                .collect();
            candidates.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
            repos
                .entry(repo.to_ascii_lowercase())
                .or_default()
                .extend(candidates.into_iter().take(32).map(|(_, tag)| tag));
        }
        use futures_util::{StreamExt, stream};
        stream::iter(repos.into_iter().map(|(repo, tags)| async move {
            let _ = self
                .release_bodies(&repo, &tags.into_iter().collect::<Vec<_>>())
                .await;
        }))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    }

    pub(super) async fn release_bodies(
        &self,
        repo: &str,
        tags: &[String],
    ) -> Result<BTreeMap<String, BodyResult>> {
        let repo = repo.to_ascii_lowercase();
        let shared: RepoBodies = if self.note_reads.is_some() {
            self.release_bodies
                .lock()
                .await
                .entry(repo.clone())
                .or_default()
                .clone()
        } else {
            Default::default()
        };
        let mut bodies = shared.lock().await;
        let missing: Vec<_> = tags
            .iter()
            .filter(|tag| !bodies.contains_key(*tag))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for chunk in missing.chunks(100) {
            let result = self.fetch_release_bodies(&repo, chunk).await;
            match result {
                Ok(fetched) => bodies.extend(fetched),
                Err(error) => {
                    let error = error.to_string();
                    bodies.extend(chunk.iter().map(|tag| (tag.clone(), Err(error.clone()))));
                }
            }
        }
        Ok(tags
            .iter()
            .map(|tag| (tag.clone(), bodies[tag].clone()))
            .collect())
    }

    async fn fetch_release_bodies(
        &self,
        repo: &str,
        tags: &[String],
    ) -> Result<BTreeMap<String, BodyResult>> {
        let mut cached = BTreeMap::new();
        for tag in tags {
            if let Some(bytes) = self.cache.extraction_get(&release_key(repo, tag)).await?
                && let Ok(saved) = serde_json::from_slice::<SavedRelease>(&bytes)
            {
                cached.insert(tag.clone(), saved);
            }
        }
        let mut remaining = tags.to_vec();
        let mut result = BTreeMap::new();
        // Cold reads include bodies. Warm reads ask only for updatedAt; only
        // edited releases need a second, batched body read. Never trust a TTL.
        for _ in 0..2 {
            if remaining.is_empty() {
                break;
            }
            let (owner, name) = repo
                .split_once('/')
                .ok_or_else(|| Error::Notes("invalid upstream repository".into()))?;
            let mut variables = json!({"owner":owner,"name":name});
            let mut declarations = String::from("$owner:String!,$name:String!");
            let mut fields = String::new();
            for (i, tag) in remaining.iter().enumerate() {
                declarations.push_str(&format!(",$t{i}:String!"));
                variables[format!("t{i}")] = json!(tag);
                let body = if cached.contains_key(tag) {
                    ""
                } else {
                    " body:description draft:isDraft prerelease:isPrerelease"
                };
                fields.push_str(&format!("r{i}:release(tagName:$t{i}){{updatedAt{body}}}"));
            }
            let query = format!(
                "query ReleaseNotes({declarations}){{repository(owner:$owner,name:$name){{{fields}}}}}"
            );
            let data = self.github.graphql(&query, variables).await?;
            let repository = data["repository"]
                .as_object()
                .ok_or_else(|| Error::Notes("missing upstream repository".into()))?;
            let mut changed = Vec::new();
            for (i, tag) in remaining.into_iter().enumerate() {
                let value = repository
                    .get(&format!("r{i}"))
                    .ok_or_else(|| Error::Notes("missing release alias".into()))?;
                if value.is_null() {
                    result.insert(tag, Ok(None));
                    continue;
                }
                let updated_at = value["updatedAt"]
                    .as_str()
                    .ok_or_else(|| Error::Notes("missing release updatedAt".into()))?;
                if let Some(saved) = cached.remove(&tag) {
                    if saved.updated_at == updated_at {
                        result.insert(tag, Ok(Some(saved.release)));
                    } else {
                        changed.push(tag);
                    }
                    continue;
                }
                if !value
                    .get("body")
                    .is_some_and(|body| body.is_null() || body.is_string())
                    || !value["draft"].is_boolean()
                    || !value["prerelease"].is_boolean()
                {
                    return Err(Error::Notes("incomplete release body".into()));
                }
                let release: ReleaseBody = serde_json::from_value(value.clone())?;
                let saved = SavedRelease {
                    updated_at: updated_at.into(),
                    release: release.clone(),
                };
                self.cache
                    .extraction_put(&release_key(repo, &tag), &serde_json::to_vec(&saved)?)
                    .await?;
                result.insert(tag, Ok(Some(release)));
            }
            remaining = changed;
        }
        Ok(result)
    }
}
