//! Per-version release dates for sources whose listing carries none (Go's
//! `.info`, a Maven POM's `Last-Modified`). Only the versions the lookup's
//! `Candidates` name are asked about — every release newer than the
//! baseline, wherever it sorts: a patch release on an older line is dated
//! like a new major. Each answer is kept, so a later lookup with an older
//! baseline asks only about the versions it adds. A version whose request
//! failed, or that was not asked about, is marked unchecked; a failed one is
//! asked about again by the next lookup. The requests bypass the HTTP cache,
//! since the answers are kept here.
use super::*;
use futures_util::{StreamExt, stream};

/// Per-version date requests a lookup has in flight at once.
const FAN_OUT: usize = 8;

/// What the source said about one version.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Dated {
    /// The version exists; its publication time, when the source gives one.
    At(Option<String>),
    /// The source lists the version but cannot serve it: not installable.
    Missing,
}
/// The answers kept for one package, by version.
#[derive(Default, Serialize, Deserialize)]
struct Dates(BTreeMap<String, Dated>);
#[derive(Serialize)]
pub(crate) struct DatesScope {
    pub(crate) source: Source,
    pub(crate) package: String,
    /// The operator registry entry's scope when one answered, so a date read
    /// with a credential is only reused under the same one.
    pub(crate) credential: Option<String>,
}
impl State for Dates {
    const NAMESPACE: &'static str = "release-dates";
    const VERSION: u32 = 1;
    type Scope = DatesScope;
}

impl Client {
    /// Date the versions in `versions` the lookup's candidates name that
    /// have no date yet, asking `ask` only about versions the store has no
    /// answer for. `ask` returns `None` when the request failed: that
    /// version is marked unchecked and nothing is stored for it. A version
    /// the store has no answer for and that was not asked about is unchecked
    /// too. Missing versions are dropped.
    pub(crate) async fn date_versions<F, Fut>(
        &self,
        scope: &DatesScope,
        versions: &mut Vec<Version>,
        ask: F,
    ) -> Result<()>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Option<Dated>>,
    {
        let candidates = self.candidates.wanted();
        let mut dates = self.cache.state::<Dates>(scope).await?.unwrap_or_default();
        let wanted: Vec<String> = versions
            .iter()
            .filter(|v| {
                v.published_at.is_none()
                    && !dates.0.contains_key(&v.version)
                    && candidates.contains(&v.version)
            })
            .map(|v| v.version.clone())
            .collect();
        let answers: Vec<(String, Option<Dated>)> = stream::iter(wanted)
            .map(|version| {
                let answer = ask(version.clone());
                async move { (version, answer.await) }
            })
            .buffer_unordered(FAN_OUT)
            .collect()
            .await;
        let mut failed = std::collections::HashSet::new();
        let mut learned = false;
        for (version, answer) in answers {
            match answer {
                Some(dated) => {
                    dates.0.insert(version, dated);
                    learned = true;
                }
                None => {
                    failed.insert(version);
                }
            }
        }
        versions.retain_mut(|version| {
            if failed.contains(&version.version) {
                version.unchecked = true;
                return true;
            }
            match dates.0.get(&version.version) {
                Some(Dated::Missing) => false,
                Some(Dated::At(time)) => {
                    if version.published_at.is_none() {
                        version.published_at.clone_from(time);
                    }
                    true
                }
                // Not asked about: the source said nothing of it.
                None => {
                    version.unchecked |= version.published_at.is_none();
                    true
                }
            }
        });
        if learned {
            self.cache.set_state(scope, &dates).await?;
        }
        Ok(())
    }
}
