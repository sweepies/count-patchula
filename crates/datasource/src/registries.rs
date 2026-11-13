use super::*;
use patchula_config::split_package;
use patchula_http::{CacheError, Derived, conditional};

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
fn terraform(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
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
    let doc: Document = conditional::json_document(body).map_err(|_| CacheError::Document)?;
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
pub(crate) const TERRAFORM_CATALOG: Derived<CacheError> =
    Derived::new("terraform-catalog", terraform);

/// One chart from a Helm repository index: its versions, and what release
/// notes read from each version's metadata.
#[derive(Default, Serialize, Deserialize)]
pub struct HelmChart {
    pub catalog: Catalog,
    #[serde(default)]
    pub notes: BTreeMap<String, upstream::ChartNotes>,
}
fn helm(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
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
        sources: Option<serde_yaml_ng::Value>,
        annotations: Option<serde_yaml_ng::Value>,
    }
    let index: Index = serde_yaml_ng::from_reader(body).map_err(|_| CacheError::Document)?;
    let charts: BTreeMap<_, _> = index
        .entries
        .into_iter()
        .map(|(name, entries)| {
            let mut chart = HelmChart::default();
            for v in entries {
                let sources = v
                    .sources
                    .as_ref()
                    .and_then(serde_yaml_ng::Value::as_sequence);
                let notes = upstream::ChartNotes::new(
                    sources
                        .into_iter()
                        .flatten()
                        .filter_map(serde_yaml_ng::Value::as_str),
                    v.annotations
                        .as_ref()
                        .and_then(|a| a.get(upstream::CHANGES))
                        .and_then(serde_yaml_ng::Value::as_str),
                );
                if !notes.is_empty() {
                    chart.notes.insert(v.version.clone(), notes);
                }
                chart.catalog.versions.push(Version {
                    version: v.version,
                    published_at: v.created,
                    digest: v.digest,
                    deprecated: v.deprecated,
                    yanked: false,
                });
            }
            sort(&mut chart.catalog.versions, Ecosystem::Npm);
            (name, chart)
        })
        .collect();
    serde_json::to_vec(&charts).map_err(|_| CacheError::Document)
}
pub(crate) const HELM_INDEX: Derived<CacheError> = Derived::new("helm-index", helm);

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
                &TERRAFORM_CATALOG,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    pub(super) async fn helm(&self, package: &str) -> Result<Catalog> {
        Ok(self.helm_chart(package).await?.catalog)
    }
    /// One chart from a Helm repository's index; `package` is `repository|chart`.
    pub async fn helm_chart(&self, package: &str) -> Result<HelmChart> {
        let (repo, chart) = split_package(package).ok_or(Error::Invalid)?;
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
            .get_derived(&url, "application/yaml", &HELM_INDEX)
            .await?;
        let mut charts: BTreeMap<String, HelmChart> = serde_json::from_slice(&response.body)?;
        charts.remove(chart).ok_or(Error::Invalid)
    }
    /// A package's versions from a private simple index at `index`.
    pub(super) async fn pypi_index(&self, index: &str, package: &str) -> Result<Catalog> {
        let package = normalize_python(package)?;
        let response = self
            .http
            .get_derived(
                &format!("{}/{package}/", index.trim_end_matches('/')),
                "application/vnd.pypi.simple.v1+json, text/html;q=0.1",
                &PYPI_INDEX_CATALOG,
            )
            .await?;
        let mut catalogs: BTreeMap<String, Catalog> = serde_json::from_slice(&response.body)?;
        Ok(catalogs.remove(&package).unwrap_or_default())
    }
    /// PEP 691 JSON; public identity is normalized according to PEP 503.
    pub async fn pypi_simple(&self, package: &str) -> Result<Catalog> {
        let package = normalize_python(package)?;
        let response = self
            .http
            .get_derived(
                &format!("https://pypi.org/simple/{package}/"),
                "application/vnd.pypi.simple.v1+json",
                &PYPI_SIMPLE_CATALOG,
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
/// `text` with the character references a PEP 503 page uses decoded.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The files of a PEP 503 HTML simple page: each anchor's text, and whether
/// it carries `data-yanked`. Tags are matched without regard to case, and a
/// `>` inside a quoted attribute value does not end one.
fn html_files(body: &[u8]) -> std::result::Result<Vec<(String, bool)>, CacheError> {
    let text = std::str::from_utf8(body).map_err(|_| CacheError::Document)?;
    let lower = text.to_ascii_lowercase();
    let mut files = Vec::new();
    let mut at = 0;
    while let Some(found) = lower[at..].find("<a") {
        let start = at + found + 2;
        at = start;
        if !lower[start..].starts_with(|c: char| c.is_ascii_whitespace() || c == '>') {
            continue;
        }
        // The end of the tag, outside quotes.
        let mut quote = None;
        let mut end = None;
        for (offset, c) in lower[start..].char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => {
                    end = Some(start + offset);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let Some(close) = lower[end..].find("</a>") else {
            break;
        };
        let attributes = &lower[start..end];
        let name = unescape(text[end + 1..end + close].trim());
        files.push((name, attributes.contains("data-yanked")));
        at = end + close;
    }
    Ok(files)
}
/// A private index's page, as PEP 691 JSON or PEP 503 HTML, read into one
/// catalog per project its files name. A page whose files belong to another
/// project gives the requested one none of their versions.
fn simple_any(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
    // The format is sniffed: a failed JSON read retries the same bytes as
    // HTML, so the body is buffered before either parse.
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .map_err(|_| CacheError::Document)?;
    let files = simple_files(&mut &bytes[..]).or_else(|_| {
        Ok::<_, CacheError>(
            html_files(&bytes)?
                .into_iter()
                .map(|(filename, yanked)| SimpleFile {
                    filename,
                    uploaded: None,
                    yanked: Value::Bool(yanked),
                })
                .collect(),
        )
    })?;
    let mut projects = BTreeMap::<String, Vec<SimpleFile>>::new();
    for file in files {
        if let Some((project, _)) = file_release(&file.filename) {
            projects.entry(project).or_default().push(file);
        }
    }
    let catalogs: BTreeMap<String, Catalog> = projects
        .into_iter()
        .map(|(project, files)| (project, catalog_of(&files)))
        .collect();
    serde_json::to_vec(&catalogs).map_err(|_| CacheError::Document)
}
pub(crate) const PYPI_INDEX_CATALOG: Derived<CacheError> =
    Derived::new("pypi-index-catalog", simple_any);

fn simple(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
    encode(&catalog_of(&simple_files(body)?))
}
pub(crate) const PYPI_SIMPLE_CATALOG: Derived<CacheError> =
    Derived::new("pypi-simple-catalog", simple);

#[derive(Deserialize)]
struct SimpleFile {
    filename: String,
    #[serde(rename = "upload-time")]
    uploaded: Option<String>,
    #[serde(default)]
    yanked: Value,
}

/// The files of a PEP 691 JSON page, of API version 1.
fn simple_files(body: &mut dyn std::io::Read) -> std::result::Result<Vec<SimpleFile>, CacheError> {
    #[derive(Deserialize)]
    struct Document {
        meta: Meta,
        files: Vec<SimpleFile>,
    }
    #[derive(Deserialize)]
    struct Meta {
        #[serde(rename = "api-version")]
        version: String,
    }
    let doc: Document = conditional::json_document(body).map_err(|_| CacheError::Document)?;
    if doc.meta.version.split('.').next() != Some("1") {
        return Err(CacheError::Document);
    }
    Ok(doc.files)
}

/// The normalised project and the version a wheel or sdist file name holds.
fn file_release(filename: &str) -> Option<(String, &str)> {
    let (project, version) = if let Some(wheel) = filename.strip_suffix(".whl") {
        let mut parts = wheel.split('-');
        (parts.next()?, parts.next()?)
    } else {
        filename
            .strip_suffix(".tar.gz")
            .or_else(|| filename.strip_suffix(".zip"))?
            .rsplit_once('-')?
    };
    Ecosystem::Pep440.parse(version).ok()?;
    let mut normalised = String::new();
    for c in project.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !normalised.ends_with('-') {
                normalised.push('-');
            }
        } else {
            normalised.push(c.to_ascii_lowercase());
        }
    }
    Some((normalised, version))
}

fn catalog_of(files: &[SimpleFile]) -> Catalog {
    let mut releases = BTreeMap::<String, Version>::new();
    for file in files {
        let Some((_, version)) = file_release(&file.filename) else {
            continue;
        };
        let yanked = file
            .yanked
            .as_bool()
            .unwrap_or_else(|| file.yanked.is_string());
        let v = releases.entry(version.into()).or_insert_with(|| Version {
            yanked: true,
            ..release(version.into())
        });
        v.yanked &= yanked;
        if let Some(time) = &file.uploaded
            && v.published_at.as_ref().is_none_or(|old| time < old)
        {
            v.published_at = Some(time.clone());
        }
    }
    let mut catalog = Catalog {
        versions: releases.into_values().collect(),
        ..Default::default()
    };
    sort(&mut catalog.versions, Ecosystem::Pep440);
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simple_groups_files_and_preserves_pep440_order() {
        let catalog: Catalog = serde_json::from_slice(&simple(&mut &br#"{"meta":{"api-version":"1.1"},"files":[{"filename":"pkg-1.0rc1.tar.gz","yanked":false},{"filename":"pkg-1.0-py3-none-any.whl","yanked":"reason"},{"filename":"pkg-1.0.tar.gz","yanked":false}]}"#[..]).unwrap()).unwrap();
        assert_eq!(catalog.versions[0].version, "1.0");
        assert!(!catalog.versions[0].yanked);
        assert_eq!(catalog.versions.len(), 2);
        assert_eq!(normalize_python("Some__Name.pkg").unwrap(), "some-name-pkg");
    }
}
