//! Source metadata and release ranges, fetched only for changed pull requests.
mod changelog;
mod source;
use super::*;
pub use changelog::{Section, parse_changelog};
use rustovate_cache::ExtractionKey;
use rustovate_versioning::Parsed;
pub use source::Repository;
pub(crate) use source::mapped;
use std::{
    collections::BTreeSet,
    sync::{Arc, LazyLock},
};
type SharedSections = Arc<tokio::sync::Mutex<Option<Arc<Vec<Section>>>>>;
pub(crate) type ChangelogCache = Arc<tokio::sync::Mutex<BTreeMap<String, SharedSections>>>;

#[derive(Default, Serialize, Deserialize)]
struct MirrorList {
    #[serde(default)]
    releases: Vec<MirrorVersion>,
    #[serde(default)]
    tags: Vec<MirrorVersion>,
    #[serde(default)]
    truncated: bool,
}
#[derive(Serialize, Deserialize)]
struct MirrorVersion {
    tag: String,
    #[serde(default)]
    prerelease: bool,
}
fn compact_mirror(bytes: &[u8]) -> std::result::Result<Vec<u8>, rustovate_http::CacheError> {
    let list: MirrorList =
        serde_json::from_slice(bytes).map_err(|_| rustovate_http::CacheError::Document)?;
    serde_json::to_vec(&list).map_err(|_| rustovate_http::CacheError::Document)
}

#[derive(Clone, Debug, Default)]
pub struct Notes {
    pub source: Option<Repository>,
    pub sections: Vec<Section>,
    pub compare: Option<String>,
    pub compare_inferred: bool,
    pub incomplete: bool,
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
            .and_then(|p| parsed(&p.0.to_string()));
    }
    parsed(text.trim_start_matches(['^', '~', '=', '>', ' ']))
}
fn in_range(version: &Parsed, old: &Parsed, new: &Parsed) -> bool {
    Ecosystem::Loose.order(version, old).is_gt()
        && !Ecosystem::Loose.order(version, new).is_gt()
        && (version.0.pre.is_empty() || !new.0.pre.is_empty())
}
fn tag_version<'a>(tag: &'a str, package: &str, repository: &Repository) -> Option<&'a str> {
    if parsed(tag).is_some() {
        return Some(tag);
    }
    let repo = repository.github.as_deref()?;
    let special = match repo {
        "golang/go" => Some("go"),
        "jqlang/jq" => Some("jq-"),
        "oven-sh/bun" => Some("bun-v"),
        "nginx/nginx" => Some("release-"),
        "postgres/postgres" => Some("REL_"),
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
        for separator in ["@", "-v", "/v"] {
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

#[derive(Serialize, Deserialize)]
struct ReleaseBody {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}
fn compact_release(bytes: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::from_slice::<ReleaseBody>(bytes)?)
}

impl Client {
    async fn notes_directory(&self, repo: &str, directory: &str) -> Result<changelog::Tree> {
        let mut url = reqwest::Url::parse(&format!("https://api.github.com/repos/{repo}/contents"))
            .expect("upstream URL");
        if !directory.is_empty() {
            url.path_segments_mut()
                .expect("HTTP URL")
                .extend(directory.split('/'));
        }
        let response = self
            .github
            .get_derived(
                url.as_str().trim_start_matches("https://api.github.com/"),
                "notes-changelog-directory-v1",
                changelog::compact_tree,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
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
        let key = ExtractionKey {
            repo: format!("changelog:{repo}:{}", file.path),
            default_branch_commit: file.sha.clone(),
            config_hash: "parsed-changelog-v1".into(),
        };
        let shared = self
            .changelogs
            .lock()
            .await
            .entry(format!("{repo}:{}:{}", file.path, file.sha))
            .or_default()
            .clone();
        let mut saved = shared.lock().await;
        if let Some(sections) = &*saved {
            return Ok(sections.clone());
        }
        let bytes = if let Some(bytes) = self.cache.extraction_get(&key).await? {
            bytes
        } else {
            let response = self
                .github
                .get_derived(
                    &format!("repos/{repo}/git/blobs/{}", file.sha),
                    "parsed-changelog-v1",
                    changelog::compact_blob,
                )
                .await?;
            self.cache.extraction_put(&key, &response.body).await?;
            response.body
        };
        let sections = Arc::new(serde_json::from_slice::<Vec<Section>>(&bytes)?);
        *saved = Some(sections.clone());
        Ok(sections)
    }
    async fn notes_list(&self, repo: &str, kind: &str) -> Result<MirrorList> {
        let response = self
            .http
            .get_derived(
                &format!(
                    "https://mise-versions.jdx.dev/data/github/{}/{kind}.json",
                    repo.to_ascii_lowercase()
                ),
                "application/json",
                "notes-mise-versions-v1",
                compact_mirror,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    pub async fn release_notes(
        &self,
        source: Source,
        package: &str,
        current: &str,
        target: &str,
    ) -> Result<Notes> {
        let mut notes = Notes {
            source: self.source_repository(source, package, target).await?,
            ..Notes::default()
        };
        let Some(repository) = notes.source.as_ref() else {
            return Ok(notes);
        };
        let Some(repo) = repository.github.as_deref() else {
            return Ok(notes);
        };
        let Some(old) = boundary(current, source) else {
            return Ok(notes);
        };
        let labelled_version = matches!(source, Source::Docker | Source::Oci)
            .then(|| {
                repository
                    .image_version
                    .as_deref()
                    .and_then(|v| tag_version(v, package, repository))
                    .and_then(parsed)
            })
            .flatten();
        let Some(new) = labelled_version.or_else(|| boundary(target, source)) else {
            return Ok(notes);
        };
        if !Ecosystem::Loose.order(&old, &new).is_lt() {
            return Ok(notes);
        }
        let mut old_tag = None;
        let mut new_tag = None;
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
                    if Ecosystem::Loose.order(&version, &old).is_eq() {
                        old_tag = Some(release.tag.clone());
                    }
                    if Ecosystem::Loose.order(&version, &new).is_eq() {
                        new_tag = Some(release.tag.clone());
                    }
                    if release.prerelease && new.0.pre.is_empty() {
                        continue;
                    }
                    if !in_range(&version, &old, &new) {
                        continue;
                    }
                    candidates.push((version, release));
                }
                candidates.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
                let mut bytes = 0;
                for (index, (version, release)) in candidates.into_iter().enumerate() {
                    // A PR displays at most 12 KB per dependency. Avoid reading
                    // hundreds of older bodies that the renderer cannot include.
                    if bytes >= 12_000 || index >= 32 {
                        notes.incomplete = true;
                        break;
                    }
                    let url = segment_url(
                        &format!("https://api.github.com/repos/{repo}/releases/tags/"),
                        &release.tag,
                    );
                    match self
                        .github
                        .get_derived(
                            url.trim_start_matches("https://api.github.com/"),
                            "release-notes-body-v2",
                            compact_release,
                        )
                        .await
                    {
                        Ok(response) => {
                            let body: ReleaseBody = serde_json::from_slice(&response.body)?;
                            if body.draft || body.prerelease && new.0.pre.is_empty() {
                                continue;
                            }
                            if let Some(body) = body.body.filter(|b| !b.trim().is_empty()) {
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
                        Err(_) => notes.incomplete = true,
                    }
                }
            }
            Err(_) => notes.incomplete = true,
        }
        if (old_tag.is_none() || new_tag.is_none())
            && let Ok(tags) = self.notes_list(repo, "tags").await
        {
            notes.incomplete |= tags.truncated;
            for tag in tags.tags {
                let Some(version) = tag_version(&tag.tag, package, repository).and_then(parsed)
                else {
                    continue;
                };
                template.get_or_insert_with(|| tag.tag.clone());
                if Ecosystem::Loose.order(&version, &old).is_eq() {
                    old_tag = Some(tag.tag.clone());
                }
                if Ecosystem::Loose.order(&version, &new).is_eq() {
                    new_tag = Some(tag.tag);
                }
            }
        }
        // Changelogs fill versions with missing/empty releases. Discover once;
        // parse each immutable blob once, irrespective of version/heading count.
        let tree = if notes.sections.iter().map(|s| s.body.len()).sum::<usize>() >= 12_000 {
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
                        .is_none_or(|dir| !file.path.starts_with(&format!("{dir}/"))),
                    file.path.clone(),
                )
            });
            if repository.directory.is_some()
                && files.iter().any(|f| {
                    repository
                        .directory
                        .as_ref()
                        .is_some_and(|d| f.path.starts_with(&format!("{d}/")))
                })
            {
                files.retain(|f| {
                    repository
                        .directory
                        .as_ref()
                        .is_some_and(|d| f.path.starts_with(&format!("{d}/")))
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
        // Sort parsed versions once, rather than reparsing inside the comparator.
        let mut sections: Vec<_> = notes
            .sections
            .drain(..)
            .filter_map(|s| Some((parsed(&s.version)?, s)))
            .collect();
        sections.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
        notes.sections.extend(sections.into_iter().map(|(_, s)| s));
        if old_tag.is_none() || new_tag.is_none() {
            notes.compare_inferred = true;
            notes.incomplete = true;
            if let Some(template) = template {
                old_tag = old_tag.or_else(|| inferred_tag(&template, package, repository, &old));
                new_tag = new_tag.or_else(|| inferred_tag(&template, package, repository, &new));
            }
        }
        if let (Some(old), Some(new)) = (old_tag, new_tag) {
            notes.compare = Some(compare(repo, &old, &new));
        } else {
            // No observed tag namespace: do not invent a compare URL.
            notes.incomplete = true;
        }
        Ok(notes)
    }
}

pub fn truncate(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A zero-width joiner immediately after @ neutralises mentions. URL tokens are
/// preserved. Bare and cross-repository issue references become explicit URLs.
pub fn sanitise(text: &str, repo: &str) -> String {
    static TOKENS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"https?://[^\s<>]+|(?P<cross>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)#(?P<number>[0-9]+)|(?P<issue>#[0-9]+)|(?P<at>@)").unwrap()
    });
    let text = text.replace("<!--", "&lt;!--").replace("-->", "--&gt;");
    TOKENS
        .replace_all(&text, |c: &regex::Captures<'_>| {
            if let Some(other) = c.name("cross") {
                format!(
                    "https://github.com/{}/issues/{}",
                    other.as_str(),
                    &c["number"]
                )
            } else if let Some(issue) = c.name("issue") {
                format!("https://github.com/{repo}/issues/{}", &issue.as_str()[1..])
            } else if c.name("at").is_some() {
                "@\u{200d}".into()
            } else {
                c[0].into()
            }
        })
        .into_owned()
}

impl Notes {
    /// Includes the source/compare links even when no upstream text is published.
    /// Budgets count UTF-8 bytes, a conservative bound on GitHub's character cap.
    pub fn render(&self, package: &str, limit: usize) -> String {
        if limit < 512 {
            return String::new();
        }
        let package = sanitise(truncate(package, 160), "").replace(['\n', '\r'], " ");
        let package = truncate(&package, 160);
        let Some(source) = &self.source else {
            return format!(
                "### {package}\n\nRelease notes unavailable: source repository could not be resolved.\n"
            );
        };
        if source.url.len() > 512 || source.url.len() + package.len() + 160 > limit {
            return format!(
                "### {package}\n\nSource found; upstream URL exceeds the release-note budget.\n"
            );
        }
        let mut text = format!("### {package}\n\n[Source]({})", source.url);
        if let Some(compare) = &self.compare {
            let label = if self.compare_inferred {
                "Compare changes (inferred tags)"
            } else {
                "Compare changes"
            };
            text.push_str(&format!(" · [{label}]({compare})"));
        }
        text.push_str("\n\n");
        if text.len() + 200 > limit {
            return format!(
                "### {package}\n\nRelease notes truncated; [view upstream]({}).\n",
                source.url
            );
        }
        let Some(repo) = source.github.as_deref() else {
            text.push_str(
                "Source found, unsupported host. Release notes currently support GitHub.\n",
            );
            return text;
        };
        if self.sections.is_empty() {
            text.push_str("No upstream release text or changelog sections found for this range.\n");
        }
        let mut details = false;
        let mut truncated = self.incomplete;
        let reserve = 1200;
        for (i, section) in self.sections.iter().enumerate() {
            if section.link.len() > 512 {
                truncated = true;
                continue;
            }
            if i == 3 {
                if text.len() + reserve + 80 > limit {
                    truncated = true;
                    break;
                }
                text.push_str("<details>\n<summary>Older releases</summary>\n\n");
                details = true;
            }
            let heading = format!("#### [{}]({})\n\n", section.version, section.link);
            let body = sanitise(&section.body, repo);
            if text.len() + heading.len() + reserve >= limit {
                truncated = true;
                break;
            }
            text.push_str(&heading);
            let available = limit.saturating_sub(text.len() + reserve);
            text.push_str(truncate(&body, available));
            text.push_str("\n\n");
            if body.len() > available {
                // Close a possibly cut fenced block before the explicit notice.
                if text.lines().filter(|line| line.starts_with("```")).count() % 2 != 0 {
                    text.push_str("\n```\n");
                }
                if text.lines().filter(|line| line.starts_with("~~~")).count() % 2 != 0 {
                    text.push_str("\n~~~\n");
                }
                text.push_str(&format!(
                    "Release notes truncated; [read the full notes]({}).\n\n",
                    section.link
                ));
                truncated = true;
                break;
            }
        }
        if details {
            text.push_str("</details>\n\n");
        }
        if truncated {
            text.push_str(&format!(
                "Release notes truncated or incomplete; [view upstream history]({}/releases).\n",
                source.url
            ));
        }
        if text.len() > limit {
            return format!(
                "### {package}\n\nRelease notes truncated; [view upstream]({}).\n",
                source.url
            );
        }
        text
    }
}
