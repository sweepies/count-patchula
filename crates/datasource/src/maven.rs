//! Maven repositories: `maven-metadata.xml` lists versions, and each POM's
//! `Last-Modified` dates every one of them for the release-age wait.
use super::*;
use patchula_config::split_package;

const CENTRAL: &str = "https://repo.maven.apache.org/maven2";
const GOOGLE: &str = "https://dl.google.com/dl/android/maven2";

/// `group:artifact`, optionally prefixed by `repository-url|`. Android groups
/// default to Google's Maven repository, everything else to Maven Central.
fn coordinates(package: &str) -> Result<(String, String, String)> {
    let (repository, coordinate) = match split_package(package) {
        Some((repository, coordinate)) => (Some(repository), coordinate),
        None => (None, package),
    };
    let (group, artifact) = coordinate.split_once(':').ok_or(Error::Invalid)?;
    let valid = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    if !valid(group) || !valid(artifact) || group.split('.').any(|p| p.is_empty() || p == "..") {
        return Err(Error::Invalid);
    }
    let repository = match repository {
        Some(url) if url.starts_with("https://") && !url.contains(['?', '#', ' ']) => {
            url.trim_end_matches('/').to_string()
        }
        Some(_) => return Err(Error::Invalid),
        None if ["androidx.", "com.android.", "com.google.android."]
            .iter()
            .any(|p| group.starts_with(p)) =>
        {
            GOOGLE.into()
        }
        None => CENTRAL.into(),
    };
    Ok((repository, group.into(), artifact.into()))
}

fn versions(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let mut text = String::new();
    body.read_to_string(&mut text)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let text = text.as_str();
    let block = text
        .split_once("<versions>")
        .and_then(|(_, rest)| rest.split_once("</versions>"))
        .map_or("", |(block, _)| block);
    let versions: Vec<String> = block
        .split("<version>")
        .skip(1)
        .filter_map(|part| part.split_once("</version>"))
        .map(|(v, _)| v.trim().to_string())
        .filter(|v| !v.is_empty() && !v.to_ascii_uppercase().ends_with("-SNAPSHOT"))
        .collect();
    serde_json::to_vec(&versions).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const MAVEN_METADATA: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("maven-metadata", 2, versions);

/// The directory holding an artifact's `maven-metadata.xml`.
fn base(package: &str) -> Result<String> {
    let (repository, group, artifact) = coordinates(package)?;
    Ok(format!(
        "{repository}/{}/{artifact}",
        group.replace('.', "/")
    ))
}

impl Client {
    pub(crate) async fn maven(&self, package: &str) -> Result<Catalog> {
        let (_, _, artifact) = coordinates(package)?;
        let base = base(package)?;
        let response = self
            .http
            .get_derived(
                &format!("{base}/maven-metadata.xml"),
                "application/xml",
                &MAVEN_METADATA,
            )
            .await?;
        let listed: Vec<String> = serde_json::from_slice(&response.body)?;
        let mut versions: Vec<Version> = listed.into_iter().map(Version::new).collect();
        sort(&mut versions, Ecosystem::Maven);
        // Every version is dated once, by its POM, and the answer kept: a
        // new release on an older line is dated like a new major. A version
        // without a POM is not resolvable and is dropped; a failed request
        // leaves the version unchecked, which policy holds.
        let scope = dates::DatesScope {
            source: Source::Maven,
            package: base.clone(),
            credential: self
                .http
                .credential(&format!("{base}/"))
                .map(|entry| entry.scope().to_owned()),
        };
        self.date_versions(&scope, &mut versions, |version| {
            let url = format!("{base}/{version}/{artifact}-{version}.pom");
            async move {
                match self.http.registry_head(&url, "application/xml", None).await {
                    Ok(response) => Some(dates::Dated::At(
                        response
                            .headers
                            .get("last-modified")
                            .and_then(|modified| modified.to_str().ok())
                            .and_then(|modified| {
                                chrono::DateTime::parse_from_rfc2822(modified).ok()
                            })
                            .map(|t| {
                                t.to_utc()
                                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                            }),
                    )),
                    Err(patchula_http::CacheError::Status(404 | 410)) => {
                        Some(dates::Dated::Missing)
                    }
                    Err(_) => None,
                }
            }
        })
        .await?;
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        })
    }
}
