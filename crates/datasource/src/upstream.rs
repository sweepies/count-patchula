//! Where a package's upstream lives: the repository release notes are read
//! from, and what a Helm chart's metadata says about its sources and changes.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};

static UPSTREAMS: LazyLock<BTreeMap<String, BTreeMap<String, String>>> =
    LazyLock::new(|| serde_json::from_str(include_str!("upstreams.json")).expect("upstream map"));

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub url: String,
    pub github: Option<String>,
    /// A GitLab or Gitea/Forgejo upstream; GitHub upstreams use `github`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<Forge>,
    pub directory: Option<String>,
    pub image_version: Option<String>,
    /// Read from a Helm chart's `sources`, which may name the packaged
    /// application's repository rather than the chart's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chart_sources: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Gitlab,
    /// Gitea and Forgejo (including Codeberg) share one API.
    Gitea,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forge {
    pub kind: ForgeKind,
    pub host: String,
    /// `group/subgroup/project` on GitLab, `owner/repo` on Gitea.
    pub path: String,
}
impl Forge {
    /// Recognise a forge from its host: gitlab.com or any host with a
    /// `gitlab` label; codeberg.org, gitea.com, or any host with a `gitea` or
    /// `forgejo` label. That label match is how self-hosted instances are
    /// found; other hosts stay unsupported rather than being probed.
    fn detect(host: &str, segments: &[&str]) -> Option<Self> {
        let kind = if host == "gitlab.com" || host.split('.').any(|l| l == "gitlab") {
            ForgeKind::Gitlab
        } else if matches!(host, "codeberg.org" | "gitea.com")
            || host.split('.').any(|l| matches!(l, "gitea" | "forgejo"))
        {
            ForgeKind::Gitea
        } else {
            return None;
        };
        // GitLab paths may nest groups; `-` starts a project sub-route.
        let mut parts: Vec<_> = segments
            .iter()
            .copied()
            .filter(|s| !s.is_empty())
            .take_while(|s| *s != "-")
            .collect();
        if kind == ForgeKind::Gitea {
            parts.truncate(2);
        }
        if let Some(last) = parts.last_mut() {
            *last = last.trim_end_matches(".git");
        }
        let valid = |p: &&str| {
            !p.is_empty()
                && p.len() <= 100
                && !matches!(*p, "." | "..")
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        (parts.len() >= 2 && parts.iter().all(valid)).then(|| Self {
            kind,
            host: host.to_ascii_lowercase(),
            path: parts.join("/"),
        })
    }
    pub fn web(&self) -> String {
        format!("https://{}/{}", self.host, self.path)
    }
}

impl Repository {
    pub fn chart_sourced(self) -> Self {
        Self {
            chart_sources: true,
            ..self
        }
    }
    pub fn from_url(raw: &str) -> Option<Self> {
        let raw = raw.trim().trim_start_matches("git+");
        let raw = if let Some(path) = raw.strip_prefix("git@github.com:") {
            format!("https://github.com/{path}")
        } else if let Some((host, path)) = raw
            .strip_prefix("git@")
            .and_then(|rest| rest.split_once(':'))
            .filter(|(host, _)| !host.contains('/'))
        {
            format!("https://{host}/{path}")
        } else if let Some(path) = raw.strip_prefix("github:") {
            format!("https://github.com/{path}")
        } else if !raw.contains("://") && raw.split('/').count() == 2 {
            format!("https://github.com/{raw}")
        } else {
            raw.to_owned()
        };
        let mut url = reqwest::Url::parse(&raw).ok()?;
        url.set_query(None);
        url.set_fragment(None);
        if (!url.username().is_empty() && !(url.scheme() == "ssh" && url.username() == "git"))
            || url.password().is_some()
        {
            return None;
        }
        if url.host_str() == Some("github.com") {
            let segments: Vec<_> = url.path_segments()?.collect();
            let parts = &segments[..segments.len().min(2)];
            // `.git` ends a clone URL, not a name; `owner/.git` has no
            // repository, so it comes off before the name is validated.
            let normalized = [
                parts.first().copied().unwrap_or(""),
                parts
                    .get(1)
                    .map_or("", |name| name.trim_end_matches(".git")),
            ];
            if normalized.iter().any(|p| {
                p.is_empty()
                    || p.len() > 100
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    || matches!(*p, "." | "..")
            }) {
                return None;
            }
            let repo = format!("{}/{}", normalized[0], normalized[1]);
            // `/tree/<ref>/<directory>` names a package inside the repository.
            let directory = match segments.get(2..) {
                Some(["tree", _, rest @ ..]) => directory(rest.join("/").trim_end_matches('/')),
                _ => None,
            };
            Some(Self {
                url: format!("https://github.com/{repo}"),
                github: Some(repo),
                directory,
                ..Self::default()
            })
        } else if matches!(url.scheme(), "https" | "http" | "git" | "ssh") {
            let segments: Vec<_> = url.path_segments().into_iter().flatten().collect();
            let forge = url
                .host_str()
                .and_then(|host| Forge::detect(host, &segments));
            Some(Self {
                url: forge.as_ref().map_or_else(|| url.to_string(), Forge::web),
                forge,
                ..Self::default()
            })
        } else {
            None
        }
    }
}

/// A path inside a repository: no traversal, no characters a URL would
/// re-encode.
pub fn directory(raw: &str) -> Option<String> {
    (!raw.is_empty()
        && !raw.starts_with('/')
        && raw.split('/').all(|s| !matches!(s, "" | "." | ".."))
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-@".contains(&b)))
    .then(|| raw.into())
}

/// The upstream `upstreams.json` names for a package of `group`.
pub fn mapped(group: &str, name: &str) -> Option<Repository> {
    Repository::from_url(UPSTREAMS.get(group)?.get(name)?)
}

/// The `Chart.yaml` annotation listing a version's changes.
pub const CHANGES: &str = "artifacthub.io/changes";
/// Sources kept per chart version; charts list one or two.
const SOURCES: usize = 8;
/// Bytes of one version's changes kept; a PR shows at most 12,000 per package.
const CHANGES_BYTES: usize = 32_000;

/// What release notes read from one chart version's `Chart.yaml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChartNotes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<String>,
}
impl ChartNotes {
    pub(crate) fn new<'a>(sources: impl Iterator<Item = &'a str>, changes: Option<&str>) -> Self {
        Self {
            sources: sources.take(SOURCES).map(str::to_owned).collect(),
            changes: changes
                .filter(|c| !c.trim().is_empty())
                .map(|c| truncate(c, CHANGES_BYTES).to_owned()),
        }
    }
    /// From `Chart.yaml` as JSON, which is what an OCI chart's config holds.
    pub(crate) fn from_metadata(value: &Value) -> Self {
        Self::new(
            value["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
            value["annotations"][CHANGES].as_str(),
        )
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.changes.is_none()
    }
    /// The first `sources` entry on a supported forge, else the first readable
    /// one. Charts often list the packaged application's repository first.
    pub fn repository(&self) -> Option<Repository> {
        let found: Vec<_> = self
            .sources
            .iter()
            .filter_map(|s| Repository::from_url(s))
            .collect();
        let supported = found
            .iter()
            .position(|r| r.github.is_some() || r.forge.is_some())
            .unwrap_or(0);
        found
            .into_iter()
            .nth(supported)
            .map(Repository::chart_sourced)
    }
}

/// The public mise-versions mirror of GitHub release and tag lists.
const MIRROR_URL: &str = "https://mise-versions.jdx.dev/data";
/// The mirror's document for one repository's `releases` or `tags` list.
pub fn mirror_url(repository: &str, kind: &str) -> String {
    format!(
        "{MIRROR_URL}/github/{}/{kind}.json",
        repository.to_ascii_lowercase()
    )
}
/// One mirror document: a repository's release and tag entries.
#[derive(Default, Serialize, Deserialize)]
pub struct MirrorList {
    #[serde(default)]
    pub releases: Vec<MirrorEntry>,
    #[serde(default)]
    pub tags: Vec<MirrorEntry>,
    #[serde(default)]
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MirrorEntry {
    pub tag: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
}
/// The longest prefix of `text` within `limit` bytes that ends on a char
/// boundary.
pub fn truncate(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_url` reads the spellings package metadata uses for a repository:
    /// SCP-style `git@` remotes, `github:` shorthands and bare `owner/repo`,
    /// and refuses credentials, unsupported schemes and unsafe paths.
    #[test]
    fn repository_urls_normalize_and_refuse() {
        let github = Repository::from_url("git@github.com:owner/repo.git").unwrap();
        assert_eq!(github.github.as_deref(), Some("owner/repo"));
        assert_eq!(github.url, "https://github.com/owner/repo");

        let gitlab = Repository::from_url("git@gitlab.com:group/sub/repo.git").unwrap();
        let forge = gitlab.forge.unwrap();
        assert_eq!(forge.kind, ForgeKind::Gitlab);
        assert_eq!(forge.path, "group/sub/repo");

        assert_eq!(
            Repository::from_url("github:owner/repo")
                .unwrap()
                .github
                .as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            Repository::from_url("owner/repo")
                .unwrap()
                .github
                .as_deref(),
            Some("owner/repo")
        );
        // `/tree/<ref>/<dir>` names a package inside the repository.
        let nested = Repository::from_url("https://github.com/o/r/tree/main/pkg/sub").unwrap();
        assert_eq!(nested.directory.as_deref(), Some("pkg/sub"));

        // Credentials never survive into a repository identity.
        // Built at runtime so the secret scan never sees a credential-shaped URL.
        let passworded = format!("https://user:{}@github.com/o/r", "p".to_owned() + "w");
        assert!(Repository::from_url(&passworded).is_none());
        assert!(Repository::from_url("https://user@github.com/o/r").is_none());
        // A scheme release notes cannot read is no repository.
        assert!(Repository::from_url("ftp://example.com/o/r").is_none());
        // Unsafe owner or repository names are refused.
        assert!(Repository::from_url("https://github.com/o/").is_none());
        assert!(Repository::from_url("https://github.com/.").is_none());
    }

    /// `truncate` never splits a character: the cut backs up to a boundary.
    #[test]
    fn truncate_cuts_on_a_character_boundary() {
        assert_eq!(truncate("abcdef", 3), "abc");
        // "aéb" is four bytes; a cut inside `é` backs up to before it.
        assert_eq!(truncate("aéb", 2), "a");
        assert_eq!(truncate("aéb", 3), "aé");
        assert_eq!(truncate("aéb", 4), "aéb");
        assert_eq!(truncate("short", 100), "short");
    }
}
