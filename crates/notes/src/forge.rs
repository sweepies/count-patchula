//! Release notes from GitLab and Gitea/Forgejo upstreams, through their public
//! APIs: releases, tags and a root changelog file. Requests are unauthenticated.
use super::*;
use crate::{Forge, ForgeKind};

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
pub(super) fn compare_link(forge: &Forge, old: &str, new: &str) -> String {
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
    body: &mut dyn std::io::Read,
    map: impl Fn(&Value) -> Option<T>,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let values: Vec<Value> = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let items: Vec<T> = values.iter().filter_map(map).collect();
    serde_json::to_vec(&items).map_err(|_| patchula_http::CacheError::Document)
}
fn gitlab_releases(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(body, |r| {
        Some(Release {
            tag: r["tag_name"].as_str()?.into(),
            body: r["description"].as_str().unwrap_or("").into(),
            // An upcoming release is not published yet.
            prerelease: r["upcoming_release"].as_bool() == Some(true),
        })
    })
}
pub(crate) const NOTES_GITLAB_RELEASES: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-gitlab-releases", 6, gitlab_releases);
fn gitea_releases(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(body, |r| {
        (r["draft"].as_bool() != Some(true)).then_some(())?;
        Some(Release {
            tag: r["tag_name"].as_str()?.into(),
            body: r["body"].as_str().unwrap_or("").into(),
            prerelease: r["prerelease"].as_bool() == Some(true),
        })
    })
}
pub(crate) const NOTES_GITEA_RELEASES: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-gitea-releases", 6, gitea_releases);
fn names(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(body, |t| {
        Some(Name {
            name: t["name"].as_str()?.into(),
        })
    })
}
pub(crate) const NOTES_FORGE_TAGS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-forge-tags", 6, names);
/// Root files only: `blob` on GitLab, `file` on Gitea.
fn files(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    compact(body, |f| {
        matches!(f["type"].as_str(), Some("blob" | "file")).then_some(())?;
        Some(Name {
            name: f["path"].as_str()?.into(),
        })
    })
}
pub(crate) const NOTES_FORGE_FILES: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-forge-files", 6, files);
fn sections(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let mut text = String::new();
    body.read_to_string(&mut text)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&parse_changelog(&text)).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NOTES_FORGE_CHANGELOG: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-forge-changelog", 6, sections);

impl Client {
    async fn forge_read<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        accept: &str,
        derived: patchula_http::Derived<patchula_http::CacheError>,
    ) -> Result<T> {
        let bytes = self
            .note_read(url, async {
                Ok(self
                    .sources
                    .http
                    .get_derived(url, accept, &derived)
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
        notes: &mut Notes,
        repository: &Repository,
        forge: &Forge,
        old: &Parsed,
        new: &Parsed,
        package: &crate::Namespace<'_>,
    ) {
        let (releases, derived) = match forge.kind {
            ForgeKind::Gitlab => (forge.api("releases?per_page=100"), NOTES_GITLAB_RELEASES),
            ForgeKind::Gitea => (forge.api("releases?limit=50"), NOTES_GITEA_RELEASES),
        };
        let mut observed = Observed::default();
        let mut versions = BTreeSet::new();
        match self
            .forge_read::<Vec<Release>>(&releases, "application/json", derived)
            .await
        {
            Ok(list) => {
                let mut candidates = Vec::new();
                for release in list {
                    let Some(version) =
                        tag_version(&release.tag, package, repository).and_then(parsed)
                    else {
                        continue;
                    };
                    observed.see(&release.tag, &version, old, new);
                    if hidden_prerelease(release.prerelease, new) || !in_range(&version, old, new) {
                        continue;
                    }
                    candidates.push((version, release));
                }
                newest_first(&mut candidates);
                versions = take_releases(notes, candidates, |release| {
                    if release.body.trim().is_empty() {
                        ReleaseText::Empty
                    } else {
                        ReleaseText::Body {
                            link: release_link(forge, &release.tag),
                            body: release.body,
                        }
                    }
                });
            }
            Err(_) => notes.incomplete = true,
        }
        if !observed.bounded() {
            let route = match forge.kind {
                ForgeKind::Gitlab => "repository/tags?per_page=100",
                ForgeKind::Gitea => "tags?limit=50",
            };
            if let Ok(listed) = self
                .forge_read::<Vec<Name>>(&forge.api(route), "application/json", NOTES_FORGE_TAGS)
                .await
            {
                for tag in listed {
                    if let Some(version) =
                        tag_version(&tag.name, package, repository).and_then(parsed)
                    {
                        observed.see(&tag.name, &version, old, new);
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
                .forge_read::<Vec<Name>>(&forge.api(route), "application/json", NOTES_FORGE_FILES)
                .await
            {
                Ok(root) => {
                    if let Some(file) = root.iter().find(|f| changelog::is_changelog(&f.name)) {
                        let raw = match forge.kind {
                            ForgeKind::Gitlab => forge.api(&format!(
                                "repository/files/{}/raw?ref=HEAD",
                                patchula_datasource::upstream::percent_encoded(&file.name)
                            )),
                            ForgeKind::Gitea => forge.api(&format!("raw/{}", file.name)),
                        };
                        match self
                            .forge_read::<Vec<Section>>(&raw, "text/plain", NOTES_FORGE_CHANGELOG)
                            .await
                        {
                            Ok(sections) => merge_changelog(
                                notes,
                                &mut versions,
                                &sections,
                                (old, new),
                                |anchor| file_link(forge, &file.name, anchor),
                            ),
                            Err(_) => notes.incomplete = true,
                        }
                    }
                }
                Err(_) => notes.incomplete = true,
            }
        }
        let ends = Ends {
            package,
            repository,
            old,
            new,
        };
        finish(notes, observed, &ends, |old, new| {
            compare_link(forge, old, new)
        });
    }
}
