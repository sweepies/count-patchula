//! Release notes for changed dependencies: upstream changelogs and releases,
//! parsed and rendered for pull request bodies.
#![forbid(unsafe_code)]
mod adapters;
mod changelog;
mod chart;
#[cfg(test)]
mod derive_parity;
mod forge;
mod markdown;
mod mirror;
mod releases;
mod source;
pub use adapters::{parse_go_history, parse_release_sgml};
pub use changelog::{Section, parse_changelog};
pub use chart::render_changes;
use forge::{compare_link, releases_url};
use markdown::demote_headings;
pub use markdown::{Links, fit, inert, sanitise};
use patchula_cache::State;
use patchula_config::{PackageId, split_package};
use patchula_datasource::upstream::{ChartNotes, MirrorList, directory, mapped, mirror_url};
pub use patchula_datasource::upstream::{Forge, ForgeKind, Repository};
use patchula_datasource::{Error, Result, truncate};
use patchula_versioning::{Ecosystem, Parsed, Versioning};
use patchula_words::Source;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
type SharedSections = Arc<tokio::sync::Mutex<Option<Arc<Vec<Section>>>>>;
/// Parsed upstream blobs memoised per batch. Changelog files and adapter
/// sections share the map; the key type keeps their formats distinct.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemoKey {
    Changelog {
        repo: String,
        path: String,
        sha: String,
    },
    Adapter {
        repo: String,
        path: String,
        sha: String,
    },
}
type ChangelogCache = Arc<tokio::sync::Mutex<BTreeMap<MemoKey, SharedSections>>>;
type SharedRead = Arc<tokio::sync::Mutex<Option<std::result::Result<Vec<u8>, String>>>>;
type ReadCache = Arc<tokio::sync::Mutex<BTreeMap<String, SharedRead>>>;

/// The parsed sections of one immutable changelog blob.
#[derive(Serialize, Deserialize)]
struct ParsedChangelog(Vec<Section>);
#[derive(Serialize)]
struct ChangelogScope {
    repository: String,
    path: String,
    blob_sha: String,
}
impl State for ParsedChangelog {
    const NAMESPACE: &'static str = "changelog";
    const VERSION: u32 = 2;
    const CODE_VERSIONED: bool = true;
    type Scope = ChangelogScope;
}

#[derive(Clone, Debug, Default)]
pub struct Notes {
    /// The version the project is on, and the one it moves to.
    pub from: String,
    pub to: String,
    pub source: Option<Repository>,
    pub sections: Vec<Section>,
    pub compare: Option<String>,
    pub compare_inferred: bool,
    pub incomplete: bool,
    /// The sections are the chart's own list of changes, not its upstream's.
    pub from_chart: bool,
}

/// Whether `path` sits inside `directory`, the way the changelog sort and
/// filter mean it.
fn under_directory(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn parsed(text: &str) -> Option<Parsed> {
    let text = text.replace('_', ".");
    Ecosystem::Loose
        .parse(&text)
        .or_else(|_| Ecosystem::Pep440.parse(&text))
        .ok()
}
fn version_id(version: &Parsed) -> String {
    version
        .1
        .as_ref()
        .map_or_else(|| version.0.to_string(), ToString::to_string)
}
fn boundary(text: &str, source: Source) -> Option<Parsed> {
    let text = text.split("@sha256:").next()?.split("@sha512:").next()?;
    if matches!(source, Source::Docker | Source::Oci) {
        return Ecosystem::Docker
            .parse(text)
            .ok()
            .and_then(|p| parsed(&p.0.to_string()))
            // An OCI catalog reads a chart's `_` build metadata back as `+`,
            // which no image tag holds.
            .or_else(|| (source == Source::Oci).then(|| parsed(text)).flatten());
    }
    parsed(text.trim_start_matches(['^', '~', '=', '>', ' ']))
}
fn in_range(version: &Parsed, old: &Parsed, new: &Parsed) -> bool {
    Ecosystem::Loose.order(version, old).is_gt()
        && !Ecosystem::Loose.order(version, new).is_gt()
        && (version.0.pre.is_empty() || !new.0.pre.is_empty())
}
/// Upstream prereleases stay out of a stable target's notes; a prerelease
/// target sees them.
fn hidden_prerelease(upstream_prerelease: bool, target: &Parsed) -> bool {
    upstream_prerelease && target.0.pre.is_empty()
}
/// A PR body renders at most this many upstream-text bytes per dependency.
const NOTES_BYTES: usize = 12_000;
/// ... and at most this many version sections.
const NOTES_SECTIONS: usize = 32;
/// The name a package's tags are namespaced by: a Helm chart's name rather
/// than its repository's URL.
fn tag_name(id: &PackageId) -> &str {
    match id.source {
        Source::Helm => split_package(&id.name).map_or(id.name.as_str(), |(_, chart)| chart),
        _ => id.name.as_str(),
    }
}
fn tag_version<'a>(tag: &'a str, package: &str, repository: &Repository) -> Option<&'a str> {
    if parsed(tag).is_some() {
        return Some(tag);
    }
    // Upstream-specific prefixes exist only for these GitHub repositories;
    // namespaced monorepo tags apply to every forge.
    let special = match repository.github.as_deref() {
        Some("golang/go") => Some("go"),
        Some("jqlang/jq") => Some("jq-"),
        Some("oven-sh/bun") => Some("bun-v"),
        Some("nginx/nginx") => Some("release-"),
        Some("postgres/postgres") => Some("REL_"),
        _ => None,
    };
    if let Some(version) = special
        .and_then(|p| tag.strip_prefix(p))
        .filter(|v| parsed(v).is_some())
    {
        return Some(version);
    }
    // Match only this package's namespace, never another monorepo package.
    let short = package.rsplit('/').next().unwrap_or(package);
    let directory = repository.directory.as_deref().unwrap_or("");
    for name in [
        package,
        short,
        directory,
        directory.rsplit('/').next().unwrap_or(""),
    ] {
        if name.is_empty() {
            continue;
        }
        // chart-releaser tags `<chart>-<version>`; elsewhere a bare `-` would
        // also take tags like `foo-2` for a version of `foo`.
        let chart = repository.chart_sources.then_some("-");
        for separator in ["@", "-v", "/v"].into_iter().chain(chart) {
            if let Some(version) = tag
                .strip_prefix(&format!("{name}{separator}"))
                .filter(|v| parsed(v).is_some())
            {
                return Some(version);
            }
        }
    }
    None
}
fn segment_url(base: &str, segment: &str) -> String {
    let mut url = reqwest::Url::parse(base).expect("validated upstream URL");
    url.path_segments_mut()
        .expect("HTTP URL")
        .pop_if_empty()
        .push(segment);
    url.into()
}
fn compare(repo: &str, old: &str, new: &str) -> String {
    segment_url(
        &format!("https://github.com/{repo}/compare/"),
        &format!("{old}...{new}"),
    )
}
/// The compare page between two tags of a repository looked up through
/// \`github-tags\` or \`forge-tags\`; \`None\` for other sources.
pub fn compare_url(source: Source, package: &str, old: &str, new: &str) -> Option<String> {
    match source {
        Source::GithubTags => Some(compare(package, old, new)),
        Source::ForgeTags => Repository::from_url(&format!("https://{package}"))?
            .forge
            .map(|forge| compare_link(&forge, old, new)),
        _ => None,
    }
}
fn inferred_tag(
    template: &str,
    package: &str,
    repository: &Repository,
    version: &Parsed,
) -> Option<String> {
    let raw = tag_version(template, package, repository)?;
    let prefix = &template[..template.len() - raw.len()];
    let v = if raw.starts_with('v') { "v" } else { "" };
    let components = raw
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?
        .split(['.', '_'])
        .count();
    let mut number = version_id(version);
    if components == 2 && version.0.patch == 0 {
        number = format!("{}.{}", version.0.major, version.0.minor);
    }
    if raw.contains('_') {
        number = number.replace('.', "_");
    }
    Some(format!("{prefix}{v}{number}"))
}

fn note_bounds(
    source: Source,
    package: &str,
    current: &str,
    target: &str,
    repository: &Repository,
) -> Option<(Parsed, Parsed)> {
    let old = boundary(current, source)?;
    let new = matches!(source, Source::Docker | Source::Oci)
        .then(|| {
            repository
                .image_version
                .as_deref()
                .and_then(|v| tag_version(v, package, repository))
                .and_then(parsed)
        })
        .flatten()
        .or_else(|| boundary(target, source))?;
    Ecosystem::Loose
        .order(&old, &new)
        .is_lt()
        .then_some((old, new))
}

/// Release notes read through a datasource client. Constructing one starts
/// a batch: every memoised read lives and dies with it, so callers cannot
/// forget to scope a publication batch. New batches still revalidate mutable
/// upstream resources, including on explicit re-checks.
#[derive(Clone)]
pub struct Client {
    sources: patchula_datasource::Client,
    changelogs: ChangelogCache,
    note_reads: ReadCache,
    release_bodies: releases::BodyCache,
}
impl Client {
    pub fn new(sources: patchula_datasource::Client) -> Self {
        Self {
            sources,
            changelogs: Default::default(),
            note_reads: Default::default(),
            release_bodies: Default::default(),
        }
    }
    async fn note_read(
        &self,
        key: &str,
        fetch: impl std::future::Future<Output = Result<Vec<u8>>>,
    ) -> Result<Vec<u8>> {
        let read = self
            .note_reads
            .lock()
            .await
            .entry(key.into())
            .or_default()
            .clone();
        let mut saved = read.lock().await;
        if let Some(result) = &*saved {
            return result.clone().map_err(Error::Notes);
        }
        let result = fetch.await.map_err(|e| e.to_string());
        *saved = Some(result.clone());
        result.map_err(Error::Notes)
    }
    async fn notes_source_repository(
        &self,
        id: &PackageId,
        target: &str,
    ) -> Result<Option<Repository>> {
        let key = serde_json::to_string(&("note-source", id, target))?;
        let bytes = self
            .note_read(&key, async {
                Ok(serde_json::to_vec(
                    &self.source_repository(id, target).await?,
                )?)
            })
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    async fn notes_directory(&self, repo: &str, directory: &str) -> Result<changelog::Tree> {
        let mut url = self
            .sources
            .github
            .api_url()
            .join(&format!("repos/{repo}/contents"))
            .expect("validated API URL");
        if !directory.is_empty() {
            url.path_segments_mut()
                .expect("HTTP URL")
                .extend(directory.split('/'));
        }
        let bytes = self
            .note_read(url.as_str(), async {
                Ok(self
                    .sources
                    .github
                    .get_derived(url.as_str(), &changelog::NOTES_CHANGELOG_DIRECTORY)
                    .await?
                    .body)
            })
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    async fn changelog_files(
        &self,
        repo: &str,
        directory: Option<&str>,
    ) -> Result<changelog::Tree> {
        // Small directory listings avoid a multi-megabyte recursive source tree.
        let mut root = self.notes_directory(repo, "").await?;
        if let Some(directory) = directory {
            match self.notes_directory(repo, directory).await {
                Ok(package) => {
                    root.truncated |= package.truncated;
                    root.tree.extend(package.tree);
                }
                Err(_) => root.truncated = true,
            }
        }
        let directories: Vec<_> = root
            .tree
            .iter()
            .filter(|file| {
                matches!(file.kind.as_str(), "dir" | "tree")
                    && [
                        "doc",
                        "docs",
                        "changelog",
                        "changelogs",
                        "changes",
                        "news",
                        "releases",
                    ]
                    .contains(
                        &file
                            .path
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .as_str(),
                    )
            })
            .map(|file| file.path.clone())
            .collect();
        // One more level handles doc/changelogs and Node's major files.
        for directory in directories {
            match self.notes_directory(repo, &directory).await {
                Ok(tree) => {
                    root.truncated |= tree.truncated;
                    for file in tree.tree {
                        if matches!(file.kind.as_str(), "dir" | "tree")
                            && file
                                .path
                                .rsplit('/')
                                .next()
                                .is_some_and(|name| name.eq_ignore_ascii_case("changelogs"))
                        {
                            match self.notes_directory(repo, &file.path).await {
                                Ok(nested) => {
                                    root.truncated |= nested.truncated;
                                    root.tree.extend(nested.tree);
                                }
                                Err(_) => root.truncated = true,
                            }
                        } else {
                            root.tree.push(file);
                        }
                    }
                }
                Err(_) => root.truncated = true,
            }
        }
        root.tree
            .retain(|file| matches!(file.kind.as_str(), "blob" | "file"));
        Ok(root)
    }
    async fn changelog_sections(
        &self,
        repo: &str,
        file: &changelog::File,
    ) -> Result<Arc<Vec<Section>>> {
        let scope = ChangelogScope {
            repository: repo.into(),
            path: file.path.clone(),
            blob_sha: file.sha.clone(),
        };
        let shared = self
            .changelogs
            .lock()
            .await
            .entry(MemoKey::Changelog {
                repo: repo.into(),
                path: file.path.clone(),
                sha: file.sha.clone(),
            })
            .or_default()
            .clone();
        let mut saved = shared.lock().await;
        if let Some(sections) = &*saved {
            return Ok(sections.clone());
        }
        let sections =
            if let Some(entry) = self.sources.cache.state::<ParsedChangelog>(&scope).await? {
                entry.0
            } else {
                let response = self
                    .sources
                    .github
                    .get_derived(
                        &format!("repos/{repo}/git/blobs/{}", file.sha),
                        &changelog::PARSED_CHANGELOG,
                    )
                    .await?;
                let entry = ParsedChangelog(serde_json::from_slice(&response.body)?);
                self.sources.cache.set_state(&scope, &entry).await?;
                entry.0
            };
        let sections = Arc::new(sections);
        *saved = Some(sections.clone());
        Ok(sections)
    }
    async fn notes_list(&self, repo: &str, kind: &str) -> Result<MirrorList> {
        let url = mirror_url(repo, kind);
        let bytes = self
            .note_read(&url, async {
                Ok(self
                    .sources
                    .http
                    .get_derived(&url, "application/json", &mirror::NOTES_MISE_VERSIONS)
                    .await?
                    .body)
            })
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub async fn release_notes(
        &self,
        id: &PackageId,
        current: &str,
        target: &str,
    ) -> Result<Notes> {
        let (source, package) = (id.source, tag_name(id));
        let mut notes = Notes {
            from: current.into(),
            to: target.into(),
            ..Notes::default()
        };
        // A chart's own list of changes describes the chart; its `sources`
        // may name the packaged application instead. Read first, so that a
        // source that cannot be read only costs the Source link.
        match self.chart_sections(id, current, target).await {
            Ok(Some((sections, incomplete))) => {
                notes.source = self
                    .notes_source_repository(id, target)
                    .await
                    .ok()
                    .flatten();
                notes.sections = sections;
                notes.incomplete = incomplete;
                notes.from_chart = true;
                return Ok(notes);
            }
            Ok(None) => {}
            // Unread chart changes leave the source's notes, marked incomplete.
            Err(_) => notes.incomplete = true,
        }
        notes.source = self.notes_source_repository(id, target).await?;
        let Some(repository) = notes.source.as_ref() else {
            return Ok(notes);
        };
        let Some(repo) = repository.github.as_deref() else {
            let forge = repository.forge.clone();
            let bounds = note_bounds(source, package, current, target, repository);
            return match (forge, bounds) {
                (Some(forge), Some((old, new))) => {
                    self.forge_notes(notes, &forge, &old, &new, package).await
                }
                _ => Ok(notes),
            };
        };
        let Some((old, new)) = note_bounds(source, package, current, target, repository) else {
            return Ok(notes);
        };
        let mut bounds = (None, None);
        let mut template = None;
        // The mirror contract does not permit API list fallback, including on
        // errors, empty responses, 404s or truncated lists. Bodies are fetched
        // from GitHub only for releases explicitly present in this listing.
        let catalog = self.notes_list(repo, "releases").await;
        let mut versions = BTreeSet::new();
        match catalog {
            Ok(catalog) => {
                notes.incomplete |= catalog.truncated;
                let mut candidates = Vec::new();
                for release in catalog.releases {
                    let Some(version) =
                        tag_version(&release.tag, package, repository).and_then(parsed)
                    else {
                        continue;
                    };
                    template.get_or_insert_with(|| release.tag.clone());
                    forge::observe(&release.tag, &version, &old, &new, &mut bounds);
                    if hidden_prerelease(release.prerelease, &new) {
                        continue;
                    }
                    if !in_range(&version, &old, &new) {
                        continue;
                    }
                    candidates.push((version, release));
                }
                candidates.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
                let tags: Vec<_> = candidates
                    .iter()
                    .take(NOTES_SECTIONS)
                    .map(|(_, r)| r.tag.clone())
                    .collect();
                let bodies = self.release_bodies(repo, &tags).await?;
                let mut bytes = 0;
                for (index, (version, release)) in candidates.into_iter().enumerate() {
                    // A PR displays at most 12 KB per dependency. Avoid reading
                    // hundreds of older bodies that the renderer cannot include.
                    if bytes >= NOTES_BYTES || index >= NOTES_SECTIONS {
                        notes.incomplete = true;
                        break;
                    }
                    match &bodies[&release.tag] {
                        Ok(Some(body)) => {
                            if body.draft || hidden_prerelease(body.prerelease, &new) {
                                continue;
                            }
                            if let Some(body) = body.body.clone().filter(|b| !b.trim().is_empty()) {
                                bytes += body.len();
                                versions.insert(version_id(&version));
                                notes.sections.push(Section {
                                    version: version_id(&version),
                                    link: segment_url(
                                        &format!("https://github.com/{repo}/releases/tag/"),
                                        &release.tag,
                                    ),
                                    body,
                                });
                            }
                        }
                        Ok(None) | Err(_) => notes.incomplete = true,
                    }
                }
            }
            Err(_) => notes.incomplete = true,
        }
        if (bounds.0.is_none() || bounds.1.is_none())
            && let Ok(tags) = self.notes_list(repo, "tags").await
        {
            notes.incomplete |= tags.truncated;
            for tag in tags.tags {
                let Some(version) = tag_version(&tag.tag, package, repository).and_then(parsed)
                else {
                    continue;
                };
                template.get_or_insert_with(|| tag.tag.clone());
                forge::observe(&tag.tag, &version, &old, &new, &mut bounds);
            }
        }
        // Changelogs fill versions with missing/empty releases. Discover once;
        // parse each immutable blob once, irrespective of version/heading count.
        let tree = if notes.sections.iter().map(|s| s.body.len()).sum::<usize>() >= NOTES_BYTES {
            None
        } else {
            Some(
                self.changelog_files(repo, repository.directory.as_deref())
                    .await,
            )
        };
        if let Some(Ok(tree)) = tree {
            notes.incomplete |= tree.truncated;
            let mut files: Vec<_> = tree
                .tree
                .into_iter()
                .filter(|f| {
                    changelog::relevant(
                        f,
                        repository.directory.as_deref(),
                        old.0.major,
                        new.0.major,
                    )
                })
                .collect();
            // Package-local notes win over repository-wide notes for monorepos.
            files.sort_by_key(|file| {
                (
                    repository
                        .directory
                        .as_ref()
                        .is_none_or(|dir| !under_directory(&file.path, dir)),
                    file.path.clone(),
                )
            });
            if repository.directory.is_some()
                && files.iter().any(|f| {
                    repository
                        .directory
                        .as_ref()
                        .is_some_and(|d| under_directory(&f.path, d))
                })
            {
                files.retain(|f| {
                    repository
                        .directory
                        .as_ref()
                        .is_some_and(|d| under_directory(&f.path, d))
                });
            }
            for file in files {
                let Ok(sections) = self.changelog_sections(repo, &file).await else {
                    notes.incomplete = true;
                    continue;
                };
                for section in sections.iter() {
                    let Some(version) = parsed(&section.version) else {
                        continue;
                    };
                    if !in_range(&version, &old, &new) || !versions.insert(version_id(&version)) {
                        continue;
                    }
                    let mut section = section.clone();
                    let mut link =
                        reqwest::Url::parse(&format!("https://github.com/{repo}/blob/HEAD/"))
                            .expect("upstream URL");
                    link.path_segments_mut()
                        .expect("HTTP URL")
                        .pop_if_empty()
                        .extend(file.path.split('/'));
                    link.set_fragment(Some(&section.link));
                    section.link = link.into();
                    section.version = version_id(&version);
                    notes.sections.push(section);
                }
            }
        } else if notes.sections.is_empty() {
            notes.incomplete = true;
        }
        // Upstreams without GitHub releases publish their notes elsewhere.
        if notes.sections.is_empty()
            && let Some(adapter) = adapters::adapter(repo)
        {
            match self.adapter_notes(adapter, &old, &new).await {
                Ok((sections, complete)) => {
                    notes.incomplete |= !complete;
                    if sections.is_empty() {
                        notes.incomplete = true;
                    }
                    notes.sections.extend(sections);
                }
                Err(_) => notes.incomplete = true,
            }
        }
        // Sort parsed versions once, rather than reparsing inside the comparator.
        let mut sections: Vec<_> = notes
            .sections
            .drain(..)
            .filter_map(|s| Some((parsed(&s.version)?, s)))
            .collect();
        sections.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
        notes.sections.extend(sections.into_iter().map(|(_, s)| s));
        if bounds.0.is_none() || bounds.1.is_none() {
            notes.compare_inferred = true;
            notes.incomplete = true;
            if let Some(template) = template {
                bounds.0 = bounds
                    .0
                    .take()
                    .or_else(|| inferred_tag(&template, package, repository, &old));
                bounds.1 = bounds
                    .1
                    .take()
                    .or_else(|| inferred_tag(&template, package, repository, &new));
            }
        }
        if let (Some(old), Some(new)) = (bounds.0, bounds.1) {
            notes.compare = Some(compare(repo, &old, &new));
        } else {
            // No observed tag namespace: do not invent a compare URL.
            notes.incomplete = true;
        }
        Ok(notes)
    }
}

/// Upstream's top heading level inside a version's notes: the package is
/// `###` and each version `####`.
const NOTE_HEADINGS: usize = 5;
/// A single release this short stays expanded: at most this many lines and bytes.
const SHORT_LINES: usize = 8;
const SHORT_BYTES: usize = 600;

/// A version inside a summary line: plain text, whatever the project calls it.
fn summary_text(version: &str) -> String {
    version
        .chars()
        .filter(|c| c.is_alphanumeric() || ".+-_~^:/!".contains(*c))
        .take(40)
        .collect()
}

impl Notes {
    /// Includes the source/compare links even when no upstream text is published.
    /// Budgets count UTF-8 bytes, a conservative bound on GitHub's character cap.
    pub fn render(&self, package: &str, limit: usize) -> String {
        if limit < 512 {
            return String::new();
        }
        let package = truncate(package, 160)
            .replace(['\n', '\r'], " ")
            .replace("<!--", "&lt;!--")
            .replace("-->", "--&gt;");
        let package = truncate(&package, 160);
        // A chart's own list of changes needs no source; anything else does.
        let chart = self.from_chart && !self.sections.is_empty();
        let fits = |source: &&Repository| {
            source.url.len() <= 512 && source.url.len() + package.len() + 160 <= limit
        };
        let source = match &self.source {
            None if !chart => {
                return format!(
                    "### {package}\n\nRelease notes unavailable: source repository could not be resolved.\n"
                );
            }
            Some(source) if !chart && !fits(&source) => {
                return format!(
                    "### {package}\n\nSource found; upstream URL exceeds the release-note budget.\n"
                );
            }
            source => source.as_ref().filter(fits),
        };
        let cut = || match source {
            Some(source) => format!(
                "### {package}\n\nRelease notes truncated; [view upstream]({}).\n",
                source.url
            ),
            None => format!("### {package}\n\nRelease notes truncated.\n"),
        };
        let mut text = format!("### {package}\n\n");
        if let Some(source) = source {
            text.push_str(&format!("[Source]({})", source.url));
            if let Some(compare) = &self.compare {
                let label = if self.compare_inferred {
                    "Compare changes (inferred tags)"
                } else {
                    "Compare changes"
                };
                text.push_str(&format!(" · [{label}]({compare})"));
            }
            text.push_str("\n\n");
            if source.chart_sources && !chart {
                text.push_str(
                    "The Count found these notes in the repository the chart lists under `sources`, so they may describe the packaged application rather than the chart itself.\n\n",
                );
            }
        }
        if text.len() + 200 > limit {
            return cut();
        }
        let links = match source {
            _ if chart => Links::None,
            Some(Repository {
                github: Some(repo), ..
            }) => Links::Github(repo),
            Some(Repository {
                forge: Some(forge), ..
            }) => Links::Forge(forge),
            _ => {
                text.push_str(
                    "Source found, unsupported host. Release notes support GitHub, GitLab and Gitea/Forgejo.\n",
                );
                return text;
            }
        };
        if self.sections.is_empty() {
            text.push_str("No upstream release text or changelog sections found for this range.\n");
        }
        // Room for the summary, the closing tag and the notice.
        let reserve = 700;
        let mut releases = String::new();
        let mut shown = 0;
        let mut trimmed = false;
        let mut unreadable = false;
        for section in &self.sections {
            if section.link.len() > 512 {
                unreadable = true;
                continue;
            }
            let heading = if section.link.is_empty() {
                format!("#### {}\n\n", section.version)
            } else {
                format!("#### [{}]({})\n\n", section.version, section.link)
            };
            let room = limit.saturating_sub(text.len() + releases.len() + reserve);
            if heading.len() + 40 > room {
                trimmed = true;
                break;
            }
            // Upstream headings sit under the version heading.
            let body = demote_headings(&sanitise(&section.body, links), NOTE_HEADINGS);
            let (body, whole) = fit(&body, room - heading.len());
            if body.trim().is_empty() && !section.body.trim().is_empty() {
                // Not even its first block fits; a bare heading says nothing.
                trimmed = true;
                break;
            }
            releases.push_str(&heading);
            releases.push_str(&body);
            releases.push('\n');
            shown += 1;
            if !whole {
                trimmed = true;
                break;
            }
        }
        if shown > 0 {
            let total = self.sections.len();
            // One short release is shorter than its own fold.
            let short = shown == total
                && total == 1
                && releases.lines().count() <= SHORT_LINES
                && releases.len() <= SHORT_BYTES;
            if short {
                text.push_str(&releases);
            } else {
                let range = if self.from.is_empty() || self.to.is_empty() {
                    String::new()
                } else {
                    format!(
                        ": {} → {}",
                        summary_text(&self.from),
                        summary_text(&self.to)
                    )
                };
                let count = if shown == total {
                    format!(
                        "{shown} {}",
                        if shown == 1 { "release" } else { "releases" }
                    )
                } else {
                    format!("{shown} of {total} releases")
                };
                text.push_str(&format!(
                    "<details>\n<summary>Release notes{range} ({count})</summary>\n\n{releases}</details>\n"
                ));
            }
        }
        // One notice: either the Count left text out, or upstream's own
        // history couldn't be read in full. A chart's own changes have no
        // upstream history to point to.
        let history = source.filter(|_| !chart).map(|source| {
            source
                .forge
                .as_ref()
                .map_or_else(|| format!("{}/releases", source.url), releases_url)
        });
        if trimmed {
            text.push_str(&match &history {
                Some(history) => format!(
                    "\nThe Count left some release notes out to keep this description short; [read them all]({history}).\n"
                ),
                None => "\nThe Count left some release notes out to keep this description short.\n".into(),
            });
        } else if self.incomplete || unreadable {
            text.push_str(&match &history {
                Some(history) => format!(
                    "\nThese release notes may be incomplete; [see upstream's history]({history}).\n"
                ),
                None => "\nThese release notes may be incomplete.\n".into(),
            });
        }
        if text.len() > limit {
            return cut();
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_releaser_tags_name_the_chart_then_its_version() {
        let id = PackageId::public(Source::Helm, "https://charts.example.com|app");
        let repository = Repository::from_url("https://github.com/example/charts")
            .unwrap()
            .chart_sourced();
        let package = tag_name(&id);
        assert_eq!(package, "app");
        assert_eq!(
            tag_version("app-1.2.3", package, &repository),
            Some("1.2.3")
        );
        assert_eq!(
            tag_version("app-v1.2.3", package, &repository),
            Some("1.2.3")
        );
        // Another chart's tags in the same repository are not this chart's.
        assert_eq!(tag_version("app-extra-1.2.3", package, &repository), None);
        assert_eq!(tag_version("other-1.2.3", package, &repository), None);
        // Only a chart's tags take a bare `-`.
        let plain = Repository::from_url("https://github.com/example/foo").unwrap();
        assert_eq!(tag_version("foo-2", "foo", &plain), None);
        assert_eq!(tag_version("foo-v2", "foo", &plain), Some("2"));
    }
}
