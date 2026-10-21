//! Release notes from GitLab and Gitea/Forgejo upstreams, through their public
//! APIs: releases, tags and a root changelog file. Requests are unauthenticated.
use super::*;
use source::{Forge, ForgeKind};

#[derive(Serialize, Deserialize)]
struct Release {
    tag: String,
    body: String,
    prerelease: bool,
}
#[derive(Serialize, Deserialize)]
struct Name {
    name: String,
}

/// One percent-encoded path segment, `/` included, as GitLab's project and
/// file IDs require.
fn encoded(path: &str) -> String {
    path.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn observe(
    tag: &str,
    version: &Parsed,
    old: &Parsed,
    new: &Parsed,
    tags: &mut (Option<String>, Option<String>),
) {
    if Ecosystem::Loose.order(version, old).is_eq() {
        tags.0 = Some(tag.to_owned());
    }
    if Ecosystem::Loose.order(version, new).is_eq() {
        tags.1 = Some(tag.to_owned());
    }
}
fn api(forge: &Forge, route: &str) -> String {
    match forge.kind {
        ForgeKind::Gitlab => format!(
            "https://{}/api/v4/projects/{}/{route}",
            forge.host,
            encoded(&forge.path)
        ),
        ForgeKind::Gitea => format!("https://{}/api/v1/repos/{}/{route}", forge.host, forge.path),
    }
}
pub(super) fn releases_url(forge: &Forge) -> String {
    match forge.kind {
        ForgeKind::Gitlab => format!("{}/-/releases", forge.web()),
        ForgeKind::Gitea => format!("{}/releases", forge.web()),
    }
}
fn release_link(forge: &Forge, tag: &str) -> String {
    let base = match forge.kind {
        ForgeKind::Gitlab => format!("{}/-/releases/", forge.web()),
        ForgeKind::Gitea => format!("{}/releases/tag/", forge.web()),
    };
    segment_url(&base, tag)
}
fn compare_link(forge: &Forge, old: &str, new: &str) -> String {
    let base = match forge.kind {
        ForgeKind::Gitlab => format!("{}/-/compare/", forge.web()),
        ForgeKind::Gitea => format!("{}/compare/", forge.web()),
    };
    segment_url(&base, &format!("{old}...{new}"))
}
fn file_link(forge: &Forge, path: &str, anchor: &str) -> String {
    let base = match forge.kind {
        ForgeKind::Gitlab => format!("{}/-/blob/HEAD/", forge.web()),
        ForgeKind::Gitea => format!("{}/src/HEAD/", forge.web()),
    };
    let mut url = reqwest::Url::parse(&base).expect("forge URL");
    url.path_segments_mut()
        .expect("HTTP URL")
        .pop_if_empty()
        .extend(path.split('/'));
    url.set_fragment(Some(anchor));
    url.into()
}

fn compact<T: Serialize>(
    bytes: &[u8],
    map: impl Fn(&Value) -> Option<T>,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let values: Vec<Value> =
        serde_json::from_slice(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    let items: Vec<T> = values.iter().filter_map(map).collect();
    serde_json::to_vec(&items).map_err(|_| patchula_http::CacheError::Document)
}
fn gitlab_releases(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(bytes, |r| {
        Some(Release {
            tag: r["tag_name"].as_str()?.into(),
            body: r["description"].as_str().unwrap_or("").into(),
            // An upcoming release is not published yet.
            prerelease: r["upcoming_release"] == true,
        })
    })
}
fn gitea_releases(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(bytes, |r| {
        (r["draft"] != true).then_some(())?;
        Some(Release {
            tag: r["tag_name"].as_str()?.into(),
            body: r["body"].as_str().unwrap_or("").into(),
            prerelease: r["prerelease"] == true,
        })
    })
}
fn names(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(bytes, |t| {
        Some(Name {
            name: t["name"].as_str()?.into(),
        })
    })
}
/// Root files only: `blob` on GitLab, `file` on Gitea.
fn files(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(bytes, |f| {
        matches!(f["type"].as_str(), Some("blob" | "file")).then_some(())?;
        Some(Name {
            name: f["path"].as_str()?.into(),
        })
    })
}
fn sections(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let text = std::str::from_utf8(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&parse_changelog(text)).map_err(|_| patchula_http::CacheError::Document)
}

impl Client {
    async fn forge_read<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        accept: &str,
        format: &'static str,
        derive: fn(&[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError>,
    ) -> Result<T> {
        let bytes = self
            .note_read(url, async {
                Ok(self
                    .http
                    .get_derived(url, accept, format, derive)
                    .await?
                    .body)
            })
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Notes for a GitLab or Gitea/Forgejo upstream: release bodies in the
    /// range, then a root changelog for versions without release text.
    pub(super) async fn forge_notes(
        &self,
        mut notes: Notes,
        forge: &Forge,
        old: &Parsed,
        new: &Parsed,
        package: &str,
    ) -> Result<Notes> {
        let repository = notes.source.clone().expect("forge source");
        let (releases, format, derive): (_, _, fn(&[u8]) -> _) = match forge.kind {
            ForgeKind::Gitlab => (
                api(forge, "releases?per_page=100"),
                "notes-gitlab-releases-v1",
                gitlab_releases,
            ),
            ForgeKind::Gitea => (
                api(forge, "releases?limit=50"),
                "notes-gitea-releases-v1",
                gitea_releases,
            ),
        };
        let mut tags = (None, None);
        let mut template = None;
        let mut versions = BTreeSet::new();
        match self
            .forge_read::<Vec<Release>>(&releases, "application/json", format, derive)
            .await
        {
            Ok(list) => {
                let mut bytes = 0;
                for release in list {
                    let Some(version) =
                        tag_version(&release.tag, package, &repository).and_then(parsed)
                    else {
                        continue;
                    };
                    template.get_or_insert_with(|| release.tag.clone());
                    observe(&release.tag, &version, old, new, &mut tags);
                    if release.prerelease && new.0.pre.is_empty()
                        || !in_range(&version, old, new)
                        || release.body.trim().is_empty()
                    {
                        continue;
                    }
                    if bytes >= 12_000 || notes.sections.len() >= 32 {
                        notes.incomplete = true;
                        continue;
                    }
                    bytes += release.body.len();
                    versions.insert(version_id(&version));
                    notes.sections.push(Section {
                        version: version_id(&version),
                        link: release_link(forge, &release.tag),
                        body: release.body,
                    });
                }
            }
            Err(_) => notes.incomplete = true,
        }
        if tags.0.is_none() || tags.1.is_none() {
            let route = match forge.kind {
                ForgeKind::Gitlab => "repository/tags?per_page=100",
                ForgeKind::Gitea => "tags?limit=50",
            };
            if let Ok(listed) = self
                .forge_read::<Vec<Name>>(
                    &api(forge, route),
                    "application/json",
                    "notes-forge-tags-v1",
                    names,
                )
                .await
            {
                for tag in listed {
                    if let Some(version) =
                        tag_version(&tag.name, package, &repository).and_then(parsed)
                    {
                        template.get_or_insert_with(|| tag.name.clone());
                        observe(&tag.name, &version, old, new, &mut tags);
                    }
                }
            }
        }
        if notes.sections.is_empty() {
            let route = match forge.kind {
                ForgeKind::Gitlab => "repository/tree?per_page=100",
                ForgeKind::Gitea => "contents",
            };
            match self
                .forge_read::<Vec<Name>>(
                    &api(forge, route),
                    "application/json",
                    "notes-forge-files-v1",
                    files,
                )
                .await
            {
                Ok(root) => {
                    if let Some(file) = root.iter().find(|f| changelog::is_changelog(&f.name)) {
                        let raw = match forge.kind {
                            ForgeKind::Gitlab => api(
                                forge,
                                &format!("repository/files/{}/raw?ref=HEAD", encoded(&file.name)),
                            ),
                            ForgeKind::Gitea => api(forge, &format!("raw/{}", file.name)),
                        };
                        match self
                            .forge_read::<Vec<Section>>(
                                &raw,
                                "text/plain",
                                "notes-forge-changelog-v1",
                                sections,
                            )
                            .await
                        {
                            Ok(parsed_sections) => {
                                for section in parsed_sections {
                                    let Some(version) = parsed(&section.version) else {
                                        continue;
                                    };
                                    if !in_range(&version, old, new)
                                        || !versions.insert(version_id(&version))
                                    {
                                        continue;
                                    }
                                    notes.sections.push(Section {
                                        version: version_id(&version),
                                        link: file_link(forge, &file.name, &section.link),
                                        body: section.body,
                                    });
                                }
                            }
                            Err(_) => notes.incomplete = true,
                        }
                    }
                }
                Err(_) => notes.incomplete = true,
            }
        }
        let mut sections: Vec<_> = notes
            .sections
            .drain(..)
            .filter_map(|s| Some((parsed(&s.version)?, s)))
            .collect();
        sections.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
        notes.sections.extend(sections.into_iter().map(|(_, s)| s));
        let (mut old_tag, mut new_tag) = tags;
        if old_tag.is_none() || new_tag.is_none() {
            notes.compare_inferred = true;
            notes.incomplete = true;
            if let Some(template) = template {
                old_tag = old_tag.or_else(|| inferred_tag(&template, package, &repository, old));
                new_tag = new_tag.or_else(|| inferred_tag(&template, package, &repository, new));
            }
        }
        if let (Some(old), Some(new)) = (old_tag, new_tag) {
            notes.compare = Some(compare_link(forge, &old, &new));
        } else {
            notes.incomplete = true;
        }
        Ok(notes)
    }
}
