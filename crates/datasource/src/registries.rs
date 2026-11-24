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
    Derived::new("terraform-catalog", 7, terraform);

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
/// A Python project name in its PEP 503 normal form, refusing anything that
/// is not a project name before it reaches a URL.
pub(super) fn normalize_python(package: &str) -> Result<String> {
    if package.is_empty()
        || !package
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_.".contains(&v))
    {
        return Err(Error::Invalid);
    }
    Ok(patchula_words::pep503(package))
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
/// bounded memory. The scan moves a cursor over the text and drops what it
/// passed once per fed chunk, so a page scans in time linear in its size.
/// Every delimiter it looks for is ASCII, so byte positions it finds are
/// always character boundaries.
struct Anchors {
    /// Bytes held back mid UTF-8 character.
    raw: Vec<u8>,
    /// Decoded text the scan has not dropped.
    text: String,
    /// Where the `<a` search resumes.
    at: usize,
    /// Inside an `<a` tag: its start, how far it is scanned, the open quote.
    tag: Option<(usize, usize, Option<u8>)>,
    /// Past an `<a>` tag: the tag's end, how far `</a>` is searched, and
    /// whether the tag carried `data-yanked`.
    close: Option<(usize, usize, bool)>,
    /// The page's anchors: text and whether the tag carried `data-yanked`.
    files: Vec<(String, bool)>,
    /// Invalid UTF-8 seen: the whole document fails.
    bad: bool,
}
/// Where ASCII `needle`, written in lower case, first occurs in `haystack`
/// without regard to ASCII case.
fn find_ignoring_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = needle[0];
    let mut from = 0;
    while let Some(offset) = haystack[from..]
        .iter()
        .position(|b| b.to_ascii_lowercase() == first)
    {
        let at = from + offset;
        let candidate = haystack.get(at..at + needle.len())?;
        if candidate.eq_ignore_ascii_case(needle) {
            return Some(at);
        }
        from = at + 1;
    }
    None
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
        let keep = self.scan(false);
        self.drop_before(keep);
    }
    /// Scans as far as the decoded text allows. Returns where the text the
    /// scan still needs starts; everything before it can be dropped.
    fn scan(&mut self, eof: bool) -> usize {
        loop {
            let text = self.text.as_bytes();
            if let Some((start, scan, mut quote)) = self.tag.take() {
                // The end of the tag, outside quotes.
                let mut end = None;
                for (offset, &byte) in text[scan..].iter().enumerate() {
                    match (quote, byte) {
                        (Some(q), byte) if byte == q => quote = None,
                        (None, b'"' | b'\'') => quote = Some(byte),
                        (None, b'>') => {
                            end = Some(scan + offset);
                            break;
                        }
                        _ => {}
                    }
                }
                match end {
                    Some(end) => {
                        let yanked =
                            find_ignoring_case(&text[start..end], b"data-yanked").is_some();
                        self.close = Some((end, end, yanked));
                    }
                    None => {
                        self.tag = Some((start, text.len(), quote));
                        return if eof { 0 } else { start };
                    }
                }
            } else if let Some((end, probe, yanked)) = self.close.take() {
                // The first `</a>` past the tag. A closer found straddling an
                // earlier search's end is not missed: the search resumes a
                // `</a>`'s width before it.
                let from = end.max(probe.saturating_sub(3));
                match find_ignoring_case(&text[from..], b"</a>") {
                    Some(found) => {
                        let close = from + found;
                        let name = unescape(self.text[end + 1..close].trim());
                        self.files.push((name, yanked));
                        self.at = close;
                    }
                    None => {
                        self.close = Some((end, text.len(), yanked));
                        return if eof { 0 } else { end };
                    }
                }
            } else {
                match find_ignoring_case(&text[self.at..], b"<a") {
                    Some(found) => {
                        let start = self.at + found + 2;
                        match text.get(start) {
                            Some(b) if b.is_ascii_whitespace() || *b == b'>' => {
                                self.tag = Some((start, start, None));
                            }
                            Some(_) => self.at = start,
                            // `<a` at the very end decides with more input.
                            None => return if eof { 0 } else { self.at + found },
                        }
                    }
                    None => {
                        // A trailing `<` may grow into `<a`; the rest is done.
                        let keep = text.len() - usize::from(text.ends_with(b"<"));
                        self.at = keep;
                        return if eof { 0 } else { keep };
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
        self.scan(true);
        Ok(self.files)
    }
}

/// How much of a private index's page is read at once.
const PAGE_BUFFER: usize = 64 * 1024;

/// A private index's page, as PEP 691 JSON or PEP 503 HTML, read into one
/// catalog per project its files name. A page whose files belong to another
/// project gives the requested one none of their versions.
fn simple_any(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
    use std::io::BufRead;
    // The format is sniffed from the first byte that is not whitespace: a
    // PEP 691 page is a JSON object, so it starts with `{`, which no HTML
    // page does. Anything else is scanned as HTML; a page starting with `{`
    // that is not a PEP 691 document is a document error.
    let mut body = std::io::BufReader::with_capacity(PAGE_BUFFER, body);
    let first = loop {
        let buffer = body.fill_buf().map_err(|_| CacheError::Document)?;
        if buffer.is_empty() {
            break None;
        }
        match buffer.iter().position(|b| !b.is_ascii_whitespace()) {
            Some(at) => break Some(buffer[at]),
            None => {
                let read = buffer.len();
                body.consume(read);
            }
        }
    };
    let files = if first == Some(b'{') {
        simple_files(&mut body)?
    } else {
        let mut anchors = Anchors::new();
        loop {
            let buffer = body.fill_buf().map_err(|_| CacheError::Document)?;
            if buffer.is_empty() {
                break;
            }
            anchors.feed(buffer);
            let read = buffer.len();
            body.consume(read);
        }
        anchors
            .finish()?
            .into_iter()
            .map(|(filename, yanked)| SimpleFile {
                filename,
                uploaded: None,
                yanked: Value::Bool(yanked),
            })
            .collect()
    };
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
    Derived::new("pypi-index-catalog", 7, simple_any);

fn simple(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
    encode(&catalog_of(&simple_files(body)?))
}
pub(crate) const PYPI_SIMPLE_CATALOG: Derived<CacheError> =
    Derived::new("pypi-simple-catalog", 7, simple);

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
    Some((patchula_words::pep503(project), version))
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

    /// Edge cases scan the same whatever the chunking: a `>` in a quoted
    /// attribute, `<a` prefixes of other tags, a closer split across chunks,
    /// mixed case, and an anchor still open at the end of the page.
    #[test]
    fn anchors_scan_edge_cases_at_every_chunk() {
        let page = concat!(
            "<abbr>not an anchor</abbr><area href=x>",
            "<a title='a > b' href=\"x\">pkg-1.0.tar.gz</a>",
            "<A\tDATA-YANKED=\"\">pkg-1.1.tar.gz</A>",
            "</a><a>pkg-1.2.tar.gz</a",
            ">tail<a href=\"y\">unfinished",
        );
        let mut buffered = Anchors::new();
        buffered.feed(page.as_bytes());
        let expected = buffered.finish().unwrap();
        assert_eq!(
            expected,
            vec![
                ("pkg-1.0.tar.gz".to_string(), false),
                ("pkg-1.1.tar.gz".to_string(), true),
                ("pkg-1.2.tar.gz".to_string(), false),
            ]
        );
        for chunk in 1..=24 {
            let mut anchors = Anchors::new();
            for part in page.as_bytes().chunks(chunk) {
                anchors.feed(part);
            }
            assert_eq!(anchors.finish().unwrap(), expected, "chunk size {chunk}");
        }
    }

    /// A large page keeps only what the scan still needs: the text it holds
    /// stays bounded however many anchors it has passed.
    #[test]
    fn anchors_hold_bounded_text_on_a_large_page() {
        let mut anchors = Anchors::new();
        for i in 0..20_000 {
            anchors
                .feed(format!("<a href=\"/p/pkg-{i}.0.tar.gz\">pkg-{i}.0.tar.gz</a>\n").as_bytes());
            assert!(anchors.text.len() < 64, "{} bytes held", anchors.text.len());
        }
        assert_eq!(anchors.finish().unwrap().len(), 20_000);
    }

    /// A private index's page is sniffed: PEP 691 JSON after leading
    /// whitespace reads as JSON, anything else scans as HTML, and both
    /// give the same catalog.
    #[test]
    fn a_private_page_is_read_as_json_or_html_by_its_first_byte() {
        let json = br#"  
 {"meta":{"api-version":"1.0"},"files":[{"filename":"pkg-1.0.tar.gz","yanked":false},{"filename":"pkg-2.0.tar.gz","yanked":true}]}"#;
        let html = b"<!DOCTYPE html><a href=\"x\">pkg-1.0.tar.gz</a><a data-yanked href=\"y\">pkg-2.0.tar.gz</a>";
        let read = |body: &[u8]| -> BTreeMap<String, Catalog> {
            serde_json::from_slice(&simple_any(&mut &body[..]).unwrap()).unwrap()
        };
        for catalogs in [read(json), read(html)] {
            let catalog = &catalogs["pkg"];
            assert_eq!(catalog.versions.len(), 2);
            assert_eq!(catalog.versions[0].version, "2.0");
            assert!(catalog.versions[0].yanked);
            assert!(!catalog.versions[1].yanked);
        }
        // A page that starts like JSON but is no PEP 691 document fails.
        assert!(simple_any(&mut &b"{\"files\": 3}"[..]).is_err());
        // An empty page has no files.
        assert!(read(b"").is_empty());
    }
}
