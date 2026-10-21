//! Maven repositories: `maven-metadata.xml` lists versions, and each POM's
//! `Last-Modified` dates the newest releases for the release-age wait.
use super::*;

const CENTRAL: &str = "https://repo.maven.apache.org/maven2";
const GOOGLE: &str = "https://dl.google.com/dl/android/maven2";
/// Only recent releases can still be inside a release-age wait.
const DATED: usize = 10;

/// `group:artifact`, optionally prefixed by `repository-url|`. Android groups
/// default to Google's Maven repository, everything else to Maven Central.
fn coordinates(package: &str) -> Result<(String, String, String)> {
    let (repository, coordinate) = match package.split_once('|') {
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

fn versions(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let text = std::str::from_utf8(bytes).map_err(|_| patchula_http::CacheError::Document)?;
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

impl Client {
    pub(crate) async fn maven(&self, package: &str) -> Result<Catalog> {
        let (repository, group, artifact) = coordinates(package)?;
        let base = format!("{repository}/{}/{artifact}", group.replace('.', "/"));
        let response = self
            .http
            .get_derived(
                &format!("{base}/maven-metadata.xml"),
                "application/xml",
                "maven-metadata-v1",
                versions,
            )
            .await?;
        let listed: Vec<String> = serde_json::from_slice(&response.body)?;
        let mut versions: Vec<Version> = listed
            .into_iter()
            .map(|version| Version {
                version,
                published_at: None,
                digest: None,
                deprecated: false,
                yanked: false,
            })
            .collect();
        sort(&mut versions, Ecosystem::Maven);
        let dates = futures_util::future::join_all(versions.iter().take(DATED).map(|v| {
            let url = format!("{base}/{0}/{artifact}-{0}.pom", v.version);
            async move {
                let response = self
                    .http
                    .registry_head(&url, "application/xml", None)
                    .await
                    .ok()?;
                let modified = response.headers.get("last-modified")?.to_str().ok()?;
                chrono::DateTime::parse_from_rfc2822(modified)
                    .ok()
                    .map(|t| {
                        t.to_utc()
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    })
            }
        }))
        .await;
        for (version, date) in versions.iter_mut().zip(dates) {
            version.published_at = date;
        }
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        })
    }
}
