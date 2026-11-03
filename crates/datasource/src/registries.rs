use super::*;
use patchula_http::CacheError;

fn encode(catalog: &Catalog) -> std::result::Result<Vec<u8>, CacheError> {
    serde_json::to_vec(catalog).map_err(|_| CacheError::Document)
}
fn release(version: String) -> Version {
    Version {
        version,
        published_at: None,
        digest: None,
        yanked: false,
        deprecated: false,
    }
}
fn terraform(body: &[u8]) -> std::result::Result<Vec<u8>, CacheError> {
    #[derive(Deserialize)]
    struct Versions {
        versions: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        version: String,
    }
    #[derive(Deserialize)]
    struct Document {
        versions: Option<Vec<Entry>>,
        modules: Option<Vec<Versions>>,
    }
    let doc: Document = serde_json::from_slice(body).map_err(|_| CacheError::Document)?;
    let versions = doc
        .versions
        .or_else(|| {
            doc.modules
                .and_then(|v| v.into_iter().next().map(|m| m.versions))
        })
        .ok_or(CacheError::Document)?;
    let mut catalog = Catalog {
        versions: versions.into_iter().map(|v| release(v.version)).collect(),
        ..Default::default()
    };
    sort(&mut catalog.versions, Ecosystem::Hashicorp);
    encode(&catalog)
}

fn helm(body: &[u8]) -> std::result::Result<Vec<u8>, CacheError> {
    #[derive(Deserialize)]
    struct Index {
        entries: BTreeMap<String, Vec<Chart>>,
    }
    #[derive(Deserialize)]
    struct Chart {
        version: String,
        created: Option<String>,
        digest: Option<String>,
        #[serde(default)]
        deprecated: bool,
    }
    let index: Index = serde_yaml_ng::from_slice(body).map_err(|_| CacheError::Document)?;
    let catalogs: BTreeMap<_, _> = index
        .entries
        .into_iter()
        .map(|(name, entries)| {
            let mut catalog = Catalog {
                versions: entries
                    .into_iter()
                    .map(|v| Version {
                        version: v.version,
                        published_at: v.created,
                        digest: v.digest,
                        deprecated: v.deprecated,
                        yanked: false,
                    })
                    .collect(),
                ..Default::default()
            };
            sort(&mut catalog.versions, Ecosystem::Npm);
            (name, catalog)
        })
        .collect();
    serde_json::to_vec(&catalogs).map_err(|_| CacheError::Document)
}

impl Client {
    pub(super) async fn terraform(&self, package: &str) -> Result<Catalog> {
        // Two components identify providers; three identify registry modules.
        let package = package
            .strip_prefix("registry.terraform.io/")
            .unwrap_or(package);
        let parts: Vec<_> = package.split('/').collect();
        if !matches!(parts.len(), 2 | 3)
            || parts.iter().any(|v| {
                v.is_empty()
                    || !v
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            })
        {
            return Err(Error::Invalid);
        }
        let kind = if parts.len() == 2 {
            "providers"
        } else {
            "modules"
        };
        let response = self
            .http
            .get_derived(
                &format!("https://registry.terraform.io/v1/{kind}/{package}/versions"),
                "application/json",
                "terraform-catalog-v1",
                terraform,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    pub(super) async fn helm(&self, package: &str) -> Result<Catalog> {
        let (repo, chart) = package.rsplit_once('|').ok_or(Error::Invalid)?;
        if chart.is_empty()
            || !chart
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(Error::Invalid);
        }
        let url = format!("{}/index.yaml", repo.trim_end_matches('/'));
        let response = self
            .http
            .get_derived(&url, "application/yaml", "helm-index-v1", helm)
            .await?;
        let mut catalogs: BTreeMap<String, Catalog> = serde_json::from_slice(&response.body)?;
        catalogs.remove(chart).ok_or(Error::Invalid)
    }
    /// A package's versions from a private simple index at `index`.
    pub(super) async fn pypi_index(&self, index: &str, package: &str) -> Result<Catalog> {
        let package = normalize_python(package)?;
        let response = self
            .http
            .get_derived(
                &format!("{}/{package}/", index.trim_end_matches('/')),
                "application/vnd.pypi.simple.v1+json, text/html;q=0.1",
                "pypi-index-catalog-v1",
                simple_any,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    /// PEP 691 JSON; public identity is normalized according to PEP 503.
    pub async fn pypi_simple(&self, package: &str) -> Result<Catalog> {
        let package = normalize_python(package)?;
        let response = self
            .http
            .get_derived(
                &format!("https://pypi.org/simple/{package}/"),
                "application/vnd.pypi.simple.v1+json",
                "pypi-simple-catalog-v1",
                simple,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
}
pub(super) fn normalize_python(package: &str) -> Result<String> {
    if package.is_empty()
        || !package
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_.".contains(&v))
    {
        return Err(Error::Invalid);
    }
    let mut out = String::new();
    for c in package.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    Ok(out)
}
/// The files of a PEP 503 HTML simple page: each anchor's text, and whether it
/// carries `data-yanked`.
fn html_files(body: &[u8]) -> std::result::Result<Vec<(String, bool)>, CacheError> {
    let text = std::str::from_utf8(body).map_err(|_| CacheError::Document)?;
    let mut files = Vec::new();
    for anchor in text.split("<a ").skip(1) {
        let Some((attributes, rest)) = anchor.split_once('>') else {
            continue;
        };
        let Some((name, _)) = rest.split_once("</a>") else {
            continue;
        };
        files.push((name.trim().to_owned(), attributes.contains("data-yanked")));
    }
    Ok(files)
}
/// A private index's simple page, as PEP 691 JSON or, failing that, PEP 503
/// HTML, which carries no upload times.
fn simple_any(body: &[u8]) -> std::result::Result<Vec<u8>, CacheError> {
    simple(body).or_else(|_| {
        let files = html_files(body)?
            .into_iter()
            .map(|(filename, yanked)| serde_json::json!({"filename": filename, "yanked": yanked}))
            .collect::<Vec<_>>();
        let document = serde_json::json!({"meta": {"api-version": "1.0"}, "files": files});
        simple(&serde_json::to_vec(&document).map_err(|_| CacheError::Document)?)
    })
}
fn simple(body: &[u8]) -> std::result::Result<Vec<u8>, CacheError> {
    #[derive(Deserialize)]
    struct Document {
        meta: Meta,
        files: Vec<File>,
    }
    #[derive(Deserialize)]
    struct Meta {
        #[serde(rename = "api-version")]
        version: String,
    }
    #[derive(Deserialize)]
    struct File {
        filename: String,
        #[serde(rename = "upload-time")]
        uploaded: Option<String>,
        #[serde(default)]
        yanked: Value,
    }
    let doc: Document = serde_json::from_slice(body).map_err(|_| CacheError::Document)?;
    if doc.meta.version.split('.').next() != Some("1") {
        return Err(CacheError::Document);
    }
    let mut releases = BTreeMap::<String, Version>::new();
    for file in doc.files {
        let filename = file
            .filename
            .strip_suffix(".tar.gz")
            .or_else(|| file.filename.strip_suffix(".zip"));
        let version = if file.filename.ends_with(".whl") {
            file.filename.split('-').nth(1)
        } else {
            filename.and_then(|v| v.rsplit_once('-').map(|(_, v)| v))
        };
        let Some(version) = version else {
            continue;
        };
        if Ecosystem::Pep440.parse(version).is_err() {
            continue;
        }
        let yanked = file
            .yanked
            .as_bool()
            .unwrap_or_else(|| file.yanked.is_string());
        let v = releases.entry(version.into()).or_insert_with(|| Version {
            yanked: true,
            ..release(version.into())
        });
        v.yanked &= yanked;
        if let Some(time) = file.uploaded
            && v.published_at.as_ref().is_none_or(|old| time < *old)
        {
            v.published_at = Some(time);
        }
    }
    let mut catalog = Catalog {
        versions: releases.into_values().collect(),
        ..Default::default()
    };
    sort(&mut catalog.versions, Ecosystem::Pep440);
    encode(&catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simple_groups_files_and_preserves_pep440_order() {
        let catalog: Catalog = serde_json::from_slice(&simple(br#"{"meta":{"api-version":"1.1"},"files":[{"filename":"pkg-1.0rc1.tar.gz","yanked":false},{"filename":"pkg-1.0-py3-none-any.whl","yanked":"reason"},{"filename":"pkg-1.0.tar.gz","yanked":false}]}"#).unwrap()).unwrap();
        assert_eq!(catalog.versions[0].version, "1.0");
        assert!(!catalog.versions[0].yanked);
        assert_eq!(catalog.versions.len(), 2);
        assert_eq!(normalize_python("Some__Name.pkg").unwrap(), "some-name-pkg");
    }
}
