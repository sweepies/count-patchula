use super::*;
use patchula_config::split_package;
use patchula_http::{CacheError, Derived, conditional};

fn encode(catalog: &Catalog) -> std::result::Result<Vec<u8>, CacheError> {
    serde_json::to_vec(catalog).map_err(|_| CacheError::Document)
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
        versions: versions
            .into_iter()
            .map(|v| Version::new(v.version))
            .collect(),
        ..Default::default()
    };
    sort(&mut catalog.versions, Ecosystem::Hashicorp);
    encode(&catalog)
}
pub(crate) const TERRAFORM_CATALOG: Derived<CacheError> =
    Derived::new("terraform-catalog", 3, terraform);

/// One chart from a Helm repository index: its versions, and what release
/// notes read from each version's metadata.
#[derive(Default, Serialize, Deserialize)]
pub struct HelmChart {
    pub catalog: Catalog,
    #[serde(default)]
    pub notes: BTreeMap<String, upstream::ChartNotes>,
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
            .get_derived(&url, "application/yaml", &crate::helm::HELM_INDEX)
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

/// A PEP 503 page's anchors, scanned as the bytes arrive: `<a ...>text</a>`
/// pairs. Tags are matched without regard to case, and a `>` inside a quoted
/// attribute value does not end one. Only the text the scan has not passed —
/// an unfinished anchor's tag and text — is held, so a large page scans in
/// bounded memory.
struct Anchors {
    /// Bytes held back mid UTF-8 character.
    raw: Vec<u8>,
    /// Decoded text the scan has not passed.
    text: String,
    /// Where the `<a` search resumes.
    at: usize,
    /// Inside an `<a` tag: its start, how far it is scanned, the open quote.
    tag: Option<(usize, usize, Option<char>)>,
    /// Past an `<a>` tag: the tag's end, how far `</a>` is searched, and
    /// whether the tag carried `data-yanked`.
    close: Option<(usize, usize, bool)>,
    /// The page's anchors: text and whether the tag carried `data-yanked`.
    files: Vec<(String, bool)>,
    /// Invalid UTF-8 seen: the whole document fails.
    bad: bool,
}
impl Anchors {
    fn new() -> Self {
        Self {
            raw: Vec::new(),
            text: String::new(),
            at: 0,
            tag: None,
            close: None,
            files: Vec::new(),
            bad: false,
        }
    }
    /// Drops text before `at`, an open tag's start or an open anchor's end —
    /// none of which the scan reads again.
    fn drop_before(&mut self, at: usize) {
        self.text.drain(..at);
        self.at = self.at.saturating_sub(at);
        if let Some((start, scan, _)) = &mut self.tag {
            *start = start.saturating_sub(at);
            *scan = scan.saturating_sub(at);
        }
        if let Some((end, probe, _)) = &mut self.close {
            *end = end.saturating_sub(at);
            *probe = probe.saturating_sub(at);
        }
    }
    fn feed(&mut self, bytes: &[u8]) {
        if self.bad {
            return;
        }
        self.raw.extend_from_slice(bytes);
        match std::str::from_utf8(&self.raw) {
            Ok(valid) => {
                self.text.push_str(valid);
                self.raw.clear();
            }
            Err(error) => {
                let valid = error.valid_up_to();
                self.text
                    .push_str(std::str::from_utf8(&self.raw[..valid]).unwrap_or_default());
                match error.error_len() {
                    // An invalid sequence fails the whole document.
                    Some(_) => {
                        self.bad = true;
                        return;
                    }
                    // An incomplete one may finish with the next chunk.
                    None => {
                        self.raw.drain(..valid);
                    }
                }
            }
        }
        self.pump(false);
    }
    /// Scans as far as the decoded text allows, then keeps only what the
    /// scan still needs.
    fn pump(&mut self, eof: bool) {
        loop {
            if let Some((start, scan, mut quote)) = self.tag.take() {
                // The end of the tag, outside quotes.
                let mut end = None;
                for (offset, c) in self.text[scan..].char_indices() {
                    match (quote, c) {
                        (Some(q), c) if c == q => quote = None,
                        (None, '"' | '\'') => quote = Some(c),
                        (None, '>') => {
                            end = Some(scan + offset);
                            break;
                        }
                        _ => {}
                    }
                }
                match end {
                    Some(end) => {
                        let yanked = self.text[start..end]
                            .to_ascii_lowercase()
                            .contains("data-yanked");
                        self.close = Some((end, end, yanked));
                    }
                    None => {
                        if eof {
                            return;
                        }
                        self.tag = Some((start, self.text.len(), quote));
                        self.drop_before(start);
                        return;
                    }
                }
            } else if let Some((end, probe, yanked)) = self.close.take() {
                // The first `</a>` past the tag. A closer found straddling an
                // earlier search's end is not missed: the search resumes a
                // `</a>`'s width before it, shifted to a character's start.
                let mut from = probe.saturating_sub(3);
                while !self.text.is_char_boundary(from) {
                    from -= 1;
                }
                let from = end.max(from);
                if let Some(found) = self.text[from..].to_ascii_lowercase().find("</a>") {
                    let close = from + found;
                    let name = unescape(self.text[end + 1..close].trim());
                    self.files.push((name, yanked));
                    self.at = close;
                    self.drop_before(close);
                } else {
                    if eof {
                        return;
                    }
                    self.close = Some((end, self.text.len(), yanked));
                    self.drop_before(end);
                    return;
                }
            } else {
                match self.text[self.at..].to_ascii_lowercase().find("<a") {
                    Some(found) => {
                        let start = self.at + found + 2;
                        if start < self.text.len()
                            && self.text[start..]
                                .starts_with(|c: char| c.is_ascii_whitespace() || c == '>')
                        {
                            self.tag = Some((start, start, None));
                        } else if start < self.text.len() {
                            self.at = start;
                        } else {
                            // `<a` at the very end decides with more input.
                            if eof {
                                return;
                            }
                            self.drop_before(self.at + found);
                            return;
                        }
                    }
                    None => {
                        if eof {
                            return;
                        }
                        // A trailing `<` may grow into `<a`; the rest is done.
                        let keep = self
                            .text
                            .len()
                            .saturating_sub(usize::from(self.text.ends_with('<')));
                        self.drop_before(keep);
                        return;
                    }
                }
            }
        }
    }
    /// The anchors the page held, or the document error an invalid page is.
    fn finish(mut self) -> std::result::Result<Vec<(String, bool)>, CacheError> {
        if !self.raw.is_empty() {
            self.bad = true;
        }
        if self.bad {
            return Err(CacheError::Document);
        }
        self.pump(true);
        Ok(self.files)
    }
}

/// Feeds every byte the JSON parse reads into the anchor scan as well, so a
/// failed JSON read can retry the same bytes as HTML without the body held.
struct Tee<'a> {
    body: &'a mut dyn std::io::Read,
    anchors: &'a mut Anchors,
}
impl std::io::Read for Tee<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.body.read(buffer)?;
        self.anchors.feed(&buffer[..read]);
        Ok(read)
    }
}

/// A private index's page, as PEP 691 JSON or PEP 503 HTML, read into one
/// catalog per project its files name. A page whose files belong to another
/// project gives the requested one none of their versions.
fn simple_any(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
    // The format is sniffed: a failed JSON read retries the same bytes as
    // HTML, so the anchor scan watches the stream as it is read and the rest
    // of the body drains through it on the retry.
    let mut anchors = Anchors::new();
    let files = {
        let mut tee = Tee {
            body: &mut *body,
            anchors: &mut anchors,
        };
        simple_files(&mut tee)
    }
    .or_else(|_| {
        let mut rest = [0; 8192];
        loop {
            match body.read(&mut rest) {
                Ok(0) => break,
                Ok(read) => anchors.feed(&rest[..read]),
                Err(_) => return Err(CacheError::Document),
            }
        }
        anchors.finish().map(|files| {
            files
                .into_iter()
                .map(|(filename, yanked)| SimpleFile {
                    filename,
                    uploaded: None,
                    yanked: Value::Bool(yanked),
                })
                .collect()
        })
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
    Derived::new("pypi-index-catalog", 3, simple_any);

fn simple(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
    encode(&catalog_of(&simple_files(body)?))
}
pub(crate) const PYPI_SIMPLE_CATALOG: Derived<CacheError> =
    Derived::new("pypi-simple-catalog", 3, simple);

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
            ..Version::new(version.into())
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

    /// Non-ASCII text around every part of an anchor scans the same however
    /// the bytes arrive: a resumed `</a>` search never splits a character.
    #[test]
    fn anchors_scan_matches_a_buffered_page_at_every_chunk() {
        let page = concat!(
            "<!DOCTYPE html><html><body>",
            "<a href=\"/simple/prénom/fichier-1.0.tar.gz\" é=\"\">fichier-1.0.tar.gz</a>",
            "<a href='/simple/汉字/汉字-2.0.tar.gz'>汉字-2.0.tar.gz🎉</a>",
            "<a HREF=\"/simple/pkg/pkg-3.0.tar.gz\">pkg-3.0.tar.gz🎉</A>",
            "<a data-yanked href=\"/simple/x/🎉-4.0.tar.gz\">🎉-4.0.tar.gz</a>",
            "<a href=\"/simple/x/&eacute;-5.0.tar.gz\">caf&eacute;-5.0.tar.gz</a>",
            "</body></html>",
        );
        let mut buffered = Anchors::new();
        buffered.feed(page.as_bytes());
        let expected = buffered.finish().unwrap();
        assert_eq!(
            expected,
            vec![
                ("fichier-1.0.tar.gz".to_string(), false),
                ("汉字-2.0.tar.gz🎉".to_string(), false),
                ("pkg-3.0.tar.gz🎉".to_string(), false),
                ("🎉-4.0.tar.gz".to_string(), true),
                ("caf&eacute;-5.0.tar.gz".to_string(), false),
            ]
        );
        for chunk in 1..=16 {
            let mut anchors = Anchors::new();
            for part in page.as_bytes().chunks(chunk) {
                anchors.feed(part);
            }
            assert_eq!(anchors.finish().unwrap(), expected, "chunk size {chunk}");
        }
    }
}
