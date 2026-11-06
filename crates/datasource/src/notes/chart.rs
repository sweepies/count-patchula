//! Helm charts: the repository a chart's `sources` names, and the changes the
//! chart lists for each version in Artifact Hub's `artifacthub.io/changes`
//! annotation (<https://artifacthub.io/docs/topics/annotations/helm/>).
use super::*;

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
    pub(crate) fn repository(&self) -> Option<Repository> {
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

#[derive(Deserialize)]
#[serde(untagged)]
enum Change {
    Text(String),
    Entry {
        kind: Option<String>,
        description: String,
        #[serde(default)]
        links: Vec<ChangeLink>,
    },
}
#[derive(Deserialize)]
struct ChangeLink {
    name: Option<String>,
    url: String,
}

const KINDS: [(&str, &str); 6] = [
    ("added", "Added"),
    ("changed", "Changed"),
    ("deprecated", "Deprecated"),
    ("removed", "Removed"),
    ("fixed", "Fixed"),
    ("security", "Security"),
];

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn link(link: &ChangeLink) -> Option<String> {
    let url = reqwest::Url::parse(link.url.trim()).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let name = link
        .name
        .as_deref()
        .map(one_line)
        .filter(|n| !n.is_empty())
        .or_else(|| url.host_str().map(str::to_owned))?;
    let name: String = name
        .chars()
        .flat_map(|c| {
            let escape = matches!(c, '\\' | '[' | ']').then_some('\\');
            escape.into_iter().chain([c])
        })
        .collect();
    let url = url.as_str().replace('(', "%28").replace(')', "%29");
    Some(format!("[{name}]({url})"))
}

/// Markdown for one version's `artifacthub.io/changes`: plain entries as a
/// list, structured entries grouped under their kind. `None` when the
/// annotation is not such a list.
pub fn render_changes(raw: &str) -> Option<String> {
    let changes: Vec<Change> = serde_yaml_ng::from_str(raw).ok()?;
    // Index 0 holds entries without a known kind.
    let mut groups: Vec<Vec<String>> = vec![Vec::new(); KINDS.len() + 1];
    for change in changes {
        let (kind, description, links) = match change {
            Change::Text(text) => (None, text, Vec::new()),
            Change::Entry {
                kind,
                description,
                links,
            } => (kind, description, links),
        };
        let description = one_line(&description);
        if description.is_empty() {
            continue;
        }
        let links: Vec<_> = links.iter().filter_map(link).collect();
        let line = if links.is_empty() {
            format!("- {description}")
        } else {
            format!("- {description} ({})", links.join(", "))
        };
        let group = kind
            .and_then(|kind| {
                KINDS
                    .iter()
                    .position(|(name, _)| kind.trim().eq_ignore_ascii_case(name))
            })
            .map_or(0, |i| i + 1);
        groups[group].push(line);
    }
    let mut text = String::new();
    for (index, lines) in groups.iter().enumerate() {
        if lines.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        if index > 0 {
            text.push_str(&format!("### {}\n\n", KINDS[index - 1].1));
        }
        text.push_str(&lines.join("\n"));
        text.push('\n');
    }
    (!text.is_empty()).then_some(text)
}

/// One version's changes as the chart lists them.
#[derive(Serialize, Deserialize)]
struct ChartRange {
    sections: Vec<Section>,
    incomplete: bool,
}

/// Versions of a chart read for its changes, newest first.
const RANGE: usize = 32;

impl Client {
    /// The changes a chart lists for each version in (current, target]. `None`
    /// when the package is not a chart or no version in the range lists any,
    /// so the chart's source repository is read instead.
    pub(super) async fn chart_sections(
        &self,
        id: &PackageId,
        current: &str,
        target: &str,
    ) -> Result<Option<(Vec<Section>, bool)>> {
        if !matches!(id.source, Source::Helm | Source::Oci) || !id.is_public() {
            return Ok(None);
        }
        let key = serde_json::to_string(&("chart-changes", id, current, target))?;
        let bytes = self
            .note_read(&key, async {
                Ok(serde_json::to_vec(
                    &self.read_chart_range(id, current, target).await?,
                )?)
            })
            .await?;
        let range: Option<ChartRange> = serde_json::from_slice(&bytes)?;
        Ok(range.map(|r| (r.sections, r.incomplete)))
    }

    async fn read_chart_range(
        &self,
        id: &PackageId,
        current: &str,
        target: &str,
    ) -> Result<Option<ChartRange>> {
        let (Some(old), Some(new)) = (boundary(current, id.source), boundary(target, id.source))
        else {
            return Ok(None);
        };
        if !Ecosystem::Loose.order(&old, &new).is_lt() {
            return Ok(None);
        }
        let mut index = match id.source {
            Source::Helm => {
                let chart = self.helm_chart(&id.name).await?;
                Some((chart.catalog.versions, chart.notes))
            }
            // Only a chart's config carries chart metadata; an image costs one read.
            _ => match self.oci_chart(&id.name, target).await? {
                Some(_) => None,
                None => return Ok(None),
            },
        };
        let versions = match &mut index {
            Some((versions, _)) => std::mem::take(versions),
            None => self.lookup(id).await?.versions,
        };
        let mut range: Vec<_> = versions
            .into_iter()
            .filter(|v| !v.yanked)
            .filter_map(|v| Some((parsed(&v.version)?, v.version)))
            .filter(|(version, _)| in_range(version, &old, &new))
            .collect();
        range.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
        let mut incomplete = range.len() > RANGE;
        range.truncate(RANGE);
        // Per version: its listed changes, `None` without the annotation, or `Err`
        // when its chart could not be read.
        let changes: Vec<std::result::Result<Option<String>, ()>> = match index {
            Some((_, notes)) => range
                .iter()
                .map(|(_, version)| Ok(notes.get(version).and_then(|n| n.changes.clone())))
                .collect(),
            None => {
                use futures_util::{StreamExt, stream};
                // Owned versions keep the future free of higher-ranked
                // borrows, so the server can spawn it.
                let versions: Vec<String> = range.iter().map(|(_, v)| v.clone()).collect();
                let read = stream::iter(versions)
                    .map(|version| async move { self.oci_chart(&id.name, &version).await })
                    .buffered(4)
                    .collect::<Vec<_>>()
                    .await;
                read.into_iter()
                    .map(|chart| match chart {
                        Ok(chart) => Ok(chart.and_then(|c| c.changes)),
                        Err(_) => Err(()),
                    })
                    .collect()
            }
        };
        if !changes.iter().any(|c| matches!(c, Ok(Some(_)))) {
            return Ok(None);
        }
        let sections = range
            .iter()
            .zip(changes)
            .map(|((version, _), changes)| {
                let body = match changes.as_ref().map(|c| c.as_deref().map(render_changes)) {
                    Ok(Some(Some(body))) => body,
                    Ok(Some(None)) => {
                        incomplete = true;
                        "The chart's list of changes for this version could not be read.\n".into()
                    }
                    Ok(None) => "The chart lists no changes for this version.\n".into(),
                    Err(()) => {
                        incomplete = true;
                        "The Count could not read the chart for this version.\n".into()
                    }
                };
                Section {
                    version: version_id(version),
                    link: String::new(),
                    body,
                }
            })
            .collect();
        Ok(Some(ChartRange {
            sections,
            incomplete,
        }))
    }
}
