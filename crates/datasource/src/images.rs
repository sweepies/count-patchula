//! Published SBOMs of container images, read by digest. The OS packages they
//! list become OSV queries under their source package names. Image layers are
//! never read.
use super::*;
use crate::docker::{Read, identity, validate_digest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_words::SbomSource;
use std::collections::BTreeSet;

const SPDX: &str = "https://spdx.dev/Document";
const BLOB: &str = "*/*";
/// Generators that catalog a whole image, so an empty OS list means none.
const CATALOGERS: [&str; 3] = ["docker-scout", "syft", "apko"];

fn is_spdx_predicate(value: &str) -> bool {
    value == SPDX
        || value
            .strip_prefix("https://spdx.dev/Document/v2.")
            .is_some_and(|minor| !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit()))
}

fn is_sigstore_bundle(media_type: &str) -> bool {
    media_type
        .strip_prefix("application/vnd.dev.sigstore.bundle")
        .is_some_and(|rest| {
            (rest.starts_with('.') || rest.starts_with('+')) && rest.contains("+json")
        })
}

fn predicate_annotation(annotations: &Value) -> Option<&str> {
    [
        "dev.sigstore.bundle.predicateType",
        "in-toto.io/predicate-type",
        "predicateType",
    ]
    .into_iter()
    .find_map(|key| annotations[key].as_str())
}

fn referrer_descriptor_candidate(descriptor: &Value) -> bool {
    let annotations = &descriptor["annotations"];
    if let Some(predicate) = predicate_annotation(annotations) {
        return is_spdx_predicate(predicate);
    }
    let artifact = descriptor["artifactType"].as_str().unwrap_or_default();
    if is_sigstore_bundle(artifact) {
        return annotations["dev.sigstore.bundle.content"]
            .as_str()
            .is_none_or(|c| c == "dsse-envelope");
    }
    artifact.contains("spdx")
        || matches!(
            artifact,
            "application/vnd.in-toto+json" | "application/vnd.dsse.envelope.v1+json"
        )
}

/// The OS-level contents of one platform's SBOM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sbom {
    pub source: SbomSource,
    pub generator: String,
    /// The generator catalogs whole images.
    pub cataloger: bool,
    /// OS packages listed, binary and source entries alike.
    pub packages: usize,
    /// The OSV ecosystem of the OS packages, or None when OSV does not cover it.
    pub ecosystem: Option<String>,
    /// The OS as the SBOM names it, such as "debian 12".
    pub os: Option<String>,
    /// Source package names and versions to query, the kernel left out.
    pub sources: BTreeSet<(String, String)>,
    /// Whether the SBOM says which source package each binary comes from.
    pub sources_known: bool,
}
impl Sbom {
    pub fn queries(&self) -> BTreeSet<AdvisoryQuery> {
        let Some(ecosystem) = &self.ecosystem else {
            return BTreeSet::new();
        };
        self.sources
            .iter()
            .map(|(package, version)| AdvisoryQuery {
                ecosystem: ecosystem.clone(),
                package: package.clone(),
                version: version.clone(),
            })
            .collect()
    }
}

/// What one image digest publishes for one platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSbom {
    Found(Sbom),
    /// The image publishes no SBOM the Count can read.
    Missing,
    /// The image has no manifest for this platform. Carries the first
    /// platform its index does have, if any.
    NoPlatform(Option<String>),
    /// The registry no longer has the digest.
    Gone,
}

/// The stored form of an SBOM blob: what the Count reads, never the files.
#[derive(Serialize, Deserialize)]
struct Compact {
    sha256: String,
    subjects: Vec<String>,
    generator: String,
    packages: usize,
    kinds: BTreeSet<String>,
    namespace: Option<String>,
    distro: Option<(String, String)>,
    sources: BTreeSet<(String, String)>,
    sources_known: bool,
}

/// The stored form of an image config: only the platform it names.
#[derive(Serialize, Deserialize)]
struct Platform {
    sha256: String,
    os: Option<String>,
    architecture: Option<String>,
    variant: Option<String>,
}

/// Hashes a body as it streams to the parser, so the stored form can stay
/// addressed by the document's digest without the bytes being held.
struct HashRead<'a> {
    body: &'a mut dyn std::io::Read,
    sha256: sha2::Sha256,
}
impl<'a> HashRead<'a> {
    fn new(body: &'a mut dyn std::io::Read) -> Self {
        use sha2::Digest;
        Self {
            body,
            sha256: sha2::Sha256::new(),
        }
    }
    /// The digest of everything read so far.
    fn digest(self) -> String {
        use sha2::Digest;
        self.sha256
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
impl std::io::Read for HashRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let read = self.body.read(buffer)?;
        self.sha256.update(&buffer[..read]);
        Ok(read)
    }
}

fn compact_platform(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    use patchula_http::CacheError::Document;
    /// The config's platform fields; its history and layers are skipped.
    #[derive(Deserialize)]
    struct Config {
        os: Option<String>,
        architecture: Option<String>,
        variant: Option<String>,
    }
    let mut read = HashRead::new(body);
    let config: Config =
        patchula_http::conditional::json_document(&mut read).map_err(|_| Document)?;
    serde_json::to_vec(&Platform {
        sha256: read.digest(),
        os: config.os,
        architecture: config.architecture,
        variant: config.variant,
    })
    .map_err(|_| Document)
}
pub(crate) const IMAGE_PLATFORM: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("image-platform", 8, compact_platform);

fn platform_name(os: &str, architecture: &str, variant: Option<&str>) -> String {
    match variant {
        Some(variant) => format!("{os}/{architecture}/{variant}"),
        None => format!("{os}/{architecture}"),
    }
}

fn verified(body: &[u8], digest: &str) -> Result<Value> {
    if !format!("sha256:{}", sha256_hex(body)).eq_ignore_ascii_case(digest) {
        return Err(Error::Invalid);
    }
    Ok(serde_json::from_slice(body)?)
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = text
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

type Purl = (String, String, String, String, BTreeMap<String, String>);
/// The parts of an OS package purl: type, namespace, name, version and qualifiers.
fn purl(text: &str) -> Option<Purl> {
    let rest = text.strip_prefix("pkg:")?;
    let (rest, qualifiers) = rest.split_once('?').unwrap_or((rest, ""));
    let (rest, _) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, version) = rest.rsplit_once('@')?;
    let mut parts = path.splitn(3, '/');
    let (kind, namespace, name) = (parts.next()?, parts.next()?, parts.next()?);
    let qualifiers = qualifiers
        .split('&')
        .filter_map(|q| q.split_once('='))
        .map(|(k, v)| (k.to_owned(), decode(v)))
        .collect();
    Some((
        kind.to_owned(),
        namespace.to_ascii_lowercase(),
        decode(name),
        decode(version),
        qualifiers,
    ))
}

/// An SPDX 2.x document, an in-toto statement around one, a DSSE envelope
/// around that, or a Sigstore bundle wrapping a DSSE envelope, read in one
/// pass: only the fields the compact form keeps, every other field skipped as
/// it streams.
#[derive(Deserialize)]
struct SbomDocument {
    /// A Sigstore bundle's DSSE envelope.
    #[serde(rename = "dsseEnvelope")]
    dsse_envelope: Option<DsseEnvelope>,
    /// A DSSE envelope's base64 statement.
    payload: Option<String>,
    /// An in-toto statement's subjects.
    #[serde(default)]
    subject: Vec<Subject>,
    #[serde(rename = "predicateType")]
    predicate_type: Option<String>,
    /// An in-toto statement's SPDX document.
    predicate: Option<Spdx>,
    // A bare SPDX document's own fields, spelled out rather than flattened:
    // `flatten` would buffer every other field, packages included.
    #[serde(rename = "spdxVersion")]
    version: Option<String>,
    #[serde(rename = "creationInfo", default)]
    creation: CreationInfo,
    #[serde(default)]
    relationships: Vec<Relationship>,
    #[serde(default)]
    packages: Vec<SpdxPackage>,
}
#[derive(Deserialize)]
struct DsseEnvelope {
    payload: Option<String>,
}
#[derive(Deserialize)]
struct Subject {
    #[serde(default)]
    digest: SubjectDigest,
}
#[derive(Default, Deserialize)]
struct SubjectDigest {
    sha256: Option<String>,
}
#[derive(Deserialize)]
struct Spdx {
    #[serde(rename = "spdxVersion")]
    version: Option<String>,
    #[serde(rename = "creationInfo", default)]
    creation: CreationInfo,
    #[serde(default)]
    relationships: Vec<Relationship>,
    #[serde(default)]
    packages: Vec<SpdxPackage>,
}
#[derive(Default, Deserialize)]
struct CreationInfo {
    #[serde(default)]
    creators: Vec<String>,
}
#[derive(Deserialize)]
struct Relationship {
    #[serde(rename = "relationshipType")]
    kind: Option<String>,
    #[serde(rename = "spdxElementId")]
    element: Option<String>,
}
#[derive(Deserialize)]
struct SpdxPackage {
    #[serde(rename = "SPDXID")]
    id: Option<String>,
    #[serde(rename = "externalRefs", default)]
    references: Vec<ExternalRef>,
}
#[derive(Deserialize)]
struct ExternalRef {
    #[serde(rename = "referenceType")]
    kind: Option<String>,
    #[serde(rename = "referenceLocator")]
    locator: Option<String>,
}

/// Read an SPDX 2.x document from a Sigstore bundle, a DSSE envelope, an
/// in-toto statement, or on its own.
fn compact_sbom(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    use patchula_http::CacheError::Document;
    // The stored SBOM names its document's digest: the bytes are hashed as
    // they stream to the parser, never read whole.
    let mut read = HashRead::new(body);
    let mut document: SbomDocument =
        patchula_http::conditional::json_document(&mut read).map_err(|_| Document)?;
    let sha256 = read.digest();
    if let Some(payload) = document
        .dsse_envelope
        .as_ref()
        .and_then(|envelope| envelope.payload.as_deref())
        .or(document.payload.as_deref())
    {
        let decoded = STANDARD.decode(payload).map_err(|_| Document)?;
        document = serde_json::from_slice(&decoded).map_err(|_| Document)?;
    }
    let subjects = document
        .subject
        .into_iter()
        .filter_map(|s| s.digest.sha256)
        .collect();
    let spdx = match document.predicate {
        Some(predicate) => {
            if !document
                .predicate_type
                .as_deref()
                .is_some_and(is_spdx_predicate)
            {
                return Err(Document);
            }
            predicate
        }
        None => Spdx {
            version: document.version,
            creation: document.creation,
            relationships: document.relationships,
            packages: document.packages,
        },
    };
    if !spdx
        .version
        .as_deref()
        .is_some_and(|v| v.starts_with("SPDX-2."))
    {
        return Err(Document);
    }
    let generator = spdx
        .creation
        .creators
        .iter()
        .filter_map(|c| c.strip_prefix("Tool: "))
        .collect::<Vec<_>>()
        .join(", ");
    // A binary GENERATED_FROM a source entry is covered by that entry.
    let derived: BTreeSet<&str> = spdx
        .relationships
        .iter()
        .filter(|r| r.kind.as_deref() == Some("GENERATED_FROM"))
        .filter_map(|r| r.element.as_deref())
        .collect();
    let mut compact = Compact {
        sha256,
        subjects,
        sources_known: !derived.is_empty() || generator.contains("docker-scout"),
        generator,
        packages: 0,
        kinds: BTreeSet::new(),
        namespace: None,
        distro: None,
        sources: BTreeSet::new(),
    };
    for package in &spdx.packages {
        let listed = package.id.as_deref().is_some_and(|id| derived.contains(id));
        for reference in &package.references {
            if reference.kind.as_deref() != Some("purl") {
                continue;
            }
            let Some((kind, namespace, name, version, qualifiers)) =
                reference.locator.as_deref().and_then(purl)
            else {
                continue;
            };
            if !matches!(kind.as_str(), "deb" | "apk" | "rpm") {
                continue;
            }
            compact.packages += 1;
            compact.kinds.insert(kind.clone());
            // Local melange packages carry namespace `unknown` alongside distro
            // packages; never let `unknown` hide the real distro namespace.
            if namespace != "unknown" && compact.namespace.as_deref().is_none_or(|n| n == "unknown")
            {
                compact.namespace = Some(namespace);
            } else {
                compact.namespace.get_or_insert(namespace);
            }
            if compact.distro.is_none() {
                compact.distro = match (qualifiers.get("os_name"), qualifiers.get("os_version")) {
                    (Some(name), Some(version)) => {
                        Some((name.to_ascii_lowercase(), version.clone()))
                    }
                    _ => qualifiers.get("distro").filter(|d| !d.is_empty()).map(|d| {
                        match d.split_once('-') {
                            Some((name, version)) => {
                                (name.to_ascii_lowercase(), version.to_owned())
                            }
                            None => (d.to_ascii_lowercase(), String::new()),
                        }
                    }),
                };
            }
            let source = match qualifiers
                .get("upstream")
                .filter(|s| !s.is_empty())
                .or_else(|| qualifiers.get("origin").filter(|s| !s.is_empty()))
            {
                Some(upstream) => {
                    compact.sources_known = true;
                    match upstream.split_once('@') {
                        Some((name, version)) => (name.to_owned(), version.to_owned()),
                        None => (upstream.clone(), version.clone()),
                    }
                }
                None => (name, version),
            };
            // A container never runs the image's kernel.
            if listed || (kind == "deb" && source.0 == "linux") {
                continue;
            }
            compact.sources.insert(source);
        }
    }
    serde_json::to_vec(&compact).map_err(|_| Document)
}
pub(crate) const SBOM_OS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("sbom-os", 8, compact_sbom);

/// OSV's ecosystem name for an OS release, when OSV covers it.
fn ecosystem(kind: &str, namespace: &str, distro: Option<&(String, String)>) -> Option<String> {
    let version = distro.map(|(_, v)| v.as_str()).unwrap_or_default();
    let numbers: Vec<_> = version.split('.').filter(|v| !v.is_empty()).collect();
    match (kind, namespace, numbers.as_slice()) {
        ("deb", "debian", [major, ..]) => Some(format!("Debian:{major}")),
        ("deb", "ubuntu", [year, month, ..]) => {
            let lts = *month == "04" && year.parse::<u32>().ok()? % 2 == 0;
            Some(format!(
                "Ubuntu:{year}.{month}{}",
                if lts { ":LTS" } else { "" }
            ))
        }
        ("apk", "alpine", [major, minor, ..]) => Some(format!("Alpine:v{major}.{minor}")),
        ("apk", "wolfi", _) => Some("Wolfi".into()),
        ("apk", "chainguard", _) => Some("Chainguard".into()),
        _ => None,
    }
}

impl Client {
    /// The SBOM that one image digest publishes for a platform such as
    /// "linux/amd64". Docker official images are read from mirror.gcr.io
    /// first. Every byte is checked against its digest, so a mirror cannot
    /// change what is read.
    pub async fn image_sbom(
        &self,
        package: &str,
        digest: &str,
        platform: &str,
    ) -> Result<ImageSbom> {
        validate_digest(digest)?;
        let (host, path) = identity(package)?;
        if host == "registry-1.docker.io" && path.starts_with("library/") {
            let mirrored = self
                .read_sbom("mirror.gcr.io", &path, digest, platform)
                .await;
            if let Ok(found) = mirrored
                && found != ImageSbom::Gone
            {
                return Ok(found);
            }
        }
        self.read_sbom(&host, &path, digest, platform).await
    }

    async fn read_sbom(
        &self,
        host: &str,
        path: &str,
        digest: &str,
        platform: &str,
    ) -> Result<ImageSbom> {
        let url = |reference: &str| format!("https://{host}/v2/{path}/manifests/{reference}");
        let index = match self
            .registry(host, path, &url(digest), Read::Manifest)
            .await
        {
            Ok(response) => verified(&response.body, digest)?,
            Err(Error::Http(patchula_http::CacheError::Status(404))) => {
                return Ok(ImageSbom::Gone);
            }
            Err(error) => return Err(error),
        };
        let (os, rest) = platform.split_once('/').ok_or(Error::Invalid)?;
        let (architecture, variant) = match rest.split_once('/') {
            Some((architecture, variant)) => (architecture, Some(variant)),
            None => (rest, None),
        };
        let mut candidates = vec![digest.to_owned()];
        if let Some(manifests) = index["manifests"].as_array() {
            let Some(child) = platform_manifest(manifests, os, architecture, variant) else {
                let first = manifests
                    .iter()
                    .map(|m| &m["platform"])
                    .filter(|p| p["os"] != "unknown")
                    .find_map(|p| {
                        let os = p["os"].as_str()?;
                        let architecture = p["architecture"].as_str()?;
                        Some(platform_name(os, architecture, p["variant"].as_str()))
                    });
                return Ok(ImageSbom::NoPlatform(first));
            };
            let child = child["digest"].as_str().ok_or(Error::Invalid)?.to_owned();
            validate_digest(&child)?;
            let attestations = manifests.iter().filter(|m| {
                m["annotations"]["vnd.docker.reference.type"] == "attestation-manifest"
                    && m["annotations"]["vnd.docker.reference.digest"] == child.as_str()
            });
            // An image can carry several attestations, such as provenance
            // beside the SBOM, in any order.
            for attestation in attestations {
                let reference = attestation["digest"].as_str().ok_or(Error::Invalid)?;
                validate_digest(reference)?;
                let response = self
                    .registry(host, path, &url(reference), Read::Manifest)
                    .await?;
                let manifest = verified(&response.body, reference)?;
                for layer in manifest["layers"].as_array().into_iter().flatten() {
                    if layer["annotations"]["in-toto.io/predicate-type"]
                        .as_str()
                        .is_some_and(is_spdx_predicate)
                        && let Some(compact) = self.sbom_blob(host, path, layer).await?
                    {
                        return Ok(found(compact, SbomSource::Attestation));
                    }
                }
            }
            candidates.insert(0, child);
        } else {
            // A single manifest names its platform only in its config.
            let config = index["config"]["digest"].as_str().ok_or(Error::Invalid)?;
            validate_digest(config)?;
            let actual = self.image_platform(host, path, config).await?;
            let named = actual.os.as_deref().zip(actual.architecture.as_deref());
            if named != Some((os, architecture))
                || variant.is_some_and(|v| actual.variant.as_deref() != Some(v))
            {
                return Ok(ImageSbom::NoPlatform(named.map(|(os, architecture)| {
                    platform_name(os, architecture, actual.variant.as_deref())
                })));
            }
        }
        // OCI 1.1 referrers come next (platform first, then index). When a
        // registry answers 400/404/405/501 on `/referrers/`, its `sha256-<hex>`
        // fallback tag is probed alongside the cosign `.att` and `.sbom` tags
        // in one parallel HEAD batch, and the answers are taken in priority
        // order: referrers (platform, index), then `.att`, then `.sbom`.
        enum ReferrerAnswer {
            Api(reqwest::Url, patchula_http::Response),
            FallbackTag,
            Failed(Error),
        }
        let referrer_urls: Vec<String> = candidates
            .iter()
            .map(|candidate| format!("https://{host}/v2/{path}/referrers/{candidate}"))
            .collect();
        let referrer_responses = futures_util::future::join_all(
            referrer_urls
                .iter()
                .map(|referrer_url| self.registry(host, path, referrer_url, Read::Referrers)),
        )
        .await;
        let mut remaining_referrers: Vec<(&str, ReferrerAnswer)> = Vec::new();
        let mut saw_fallback = false;
        for ((candidate, referrer_url), answer) in
            candidates.iter().zip(referrer_urls).zip(referrer_responses)
        {
            let answer = match answer {
                Ok(response) => {
                    let parsed = reqwest::Url::parse(&referrer_url).map_err(|_| Error::Invalid)?;
                    ReferrerAnswer::Api(parsed, response)
                }
                Err(Error::Http(patchula_http::CacheError::Status(400 | 404 | 405 | 501))) => {
                    ReferrerAnswer::FallbackTag
                }
                Err(error) => ReferrerAnswer::Failed(error),
            };
            if saw_fallback {
                remaining_referrers.push((candidate.as_str(), answer));
                continue;
            }
            match answer {
                ReferrerAnswer::Api(parsed, response) => {
                    if let Some(compact) = self
                        .read_referrers_pages(host, path, candidate, parsed, response)
                        .await?
                    {
                        return Ok(found(compact, SbomSource::Referrer));
                    }
                }
                ReferrerAnswer::FallbackTag => {
                    saw_fallback = true;
                    remaining_referrers.push((candidate.as_str(), ReferrerAnswer::FallbackTag));
                }
                ReferrerAnswer::Failed(error) => return Err(error),
            }
        }
        // Build one parallel HEAD batch for any OCI 1.1 fallback tags and the
        // cosign `.att` and `.sbom` tags.
        let fallback_tags: Vec<(usize, String)> = remaining_referrers
            .iter()
            .enumerate()
            .filter(|(_, (_, answer))| matches!(answer, ReferrerAnswer::FallbackTag))
            .map(|(index, (candidate, _))| (index, candidate.replace(':', "-")))
            .collect();
        let cosign_probes: Vec<(SbomSource, &str, String)> = [
            (".att", SbomSource::CosignAtt),
            (".sbom", SbomSource::CosignSbom),
        ]
        .into_iter()
        .flat_map(|(suffix, source)| {
            candidates.iter().map(move |candidate| {
                let tag = format!("{}{suffix}", candidate.replace(':', "-"));
                (source, candidate.as_str(), tag)
            })
        })
        .collect();
        let head_urls: Vec<String> = fallback_tags
            .iter()
            .map(|(_, tag)| url(tag))
            .chain(cosign_probes.iter().map(|(_, _, tag)| url(tag)))
            .collect();
        let mut heads = futures_util::future::join_all(
            head_urls
                .iter()
                .map(|tag_url| self.registry(host, path, tag_url, Read::Head)),
        )
        .await
        .into_iter();
        let mut fallback_heads: BTreeMap<usize, (String, Result<patchula_http::Response>)> =
            BTreeMap::new();
        for (index, tag) in fallback_tags {
            if let Some(head) = heads.next() {
                fallback_heads.insert(index, (tag, head));
            }
        }
        for (index, (candidate, answer)) in remaining_referrers.into_iter().enumerate() {
            match answer {
                ReferrerAnswer::Api(parsed, response) => {
                    if let Some(compact) = self
                        .read_referrers_pages(host, path, candidate, parsed, response)
                        .await?
                    {
                        return Ok(found(compact, SbomSource::Referrer));
                    }
                }
                ReferrerAnswer::FallbackTag => {
                    let (tag, head) = fallback_heads.remove(&index).expect("fallback head");
                    let head = match head {
                        Ok(head) => head,
                        Err(Error::Http(patchula_http::CacheError::Status(404))) => continue,
                        Err(error) => return Err(error),
                    };
                    let Some(manifest) = self.tagged_manifest(host, path, &tag, &head).await?
                    else {
                        continue;
                    };
                    if let Some(compact) = self
                        .read_referrer_index(host, path, candidate, &manifest)
                        .await?
                    {
                        return Ok(found(compact, SbomSource::Referrer));
                    }
                }
                ReferrerAnswer::Failed(error) => return Err(error),
            }
        }
        for ((source, candidate, tag), head) in cosign_probes.into_iter().zip(heads) {
            let head = match head {
                Ok(head) => head,
                Err(Error::Http(patchula_http::CacheError::Status(404))) => continue,
                Err(error) => return Err(error),
            };
            let Some(manifest) = self.tagged_manifest(host, path, &tag, &head).await? else {
                continue;
            };
            for layer in manifest["layers"].as_array().into_iter().flatten() {
                let wanted = if source == SbomSource::CosignAtt {
                    predicate_annotation(&layer["annotations"]).is_some_and(is_spdx_predicate)
                } else {
                    layer["mediaType"]
                        .as_str()
                        .is_some_and(|t| t.contains("spdx"))
                };
                if !wanted {
                    continue;
                }
                let Some(compact) = self.sbom_blob(host, path, layer).await? else {
                    continue;
                };
                let subject = candidate.trim_start_matches("sha256:");
                if source == SbomSource::CosignAtt
                    && !compact
                        .subjects
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(subject))
                {
                    continue;
                }
                return Ok(found(compact, source));
            }
        }
        Ok(ImageSbom::Missing)
    }

    /// Read the manifest a mutable tag points to, checking its bytes against
    /// `Docker-Content-Digest`. Returns `None` when the registry names no valid
    /// digest on HEAD or GET.
    async fn tagged_manifest(
        &self,
        host: &str,
        path: &str,
        tag: &str,
        head: &patchula_http::Response,
    ) -> Result<Option<Value>> {
        let url = |reference: &str| format!("https://{host}/v2/{path}/manifests/{reference}");
        let digest_of = |headers: &reqwest::header::HeaderMap| {
            headers
                .get("docker-content-digest")
                .and_then(|v| v.to_str().ok())
                .filter(|d| validate_digest(d).is_ok())
                .map(str::to_owned)
        };
        match digest_of(&head.headers) {
            Some(named) => {
                let response = self
                    .registry(host, path, &url(&named), Read::Manifest)
                    .await?;
                Ok(Some(verified(&response.body, &named)?))
            }
            None => {
                let response = self.registry(host, path, &url(tag), Read::Manifest).await?;
                let Some(named) = digest_of(&response.headers) else {
                    return Ok(None);
                };
                Ok(Some(verified(&response.body, &named)?))
            }
        }
    }

    async fn read_referrers_pages(
        &self,
        host: &str,
        path: &str,
        candidate: &str,
        mut url: reqwest::Url,
        mut response: patchula_http::Response,
    ) -> Result<Option<Compact>> {
        let origin = url.origin();
        let expected_path = url.path().to_owned();
        let mut pages = Pages::starting_at(100, url.as_str());
        loop {
            let index: Value = serde_json::from_slice(&response.body)?;
            if let Some(compact) = self
                .read_referrer_index(host, path, candidate, &index)
                .await?
            {
                return Ok(Some(compact));
            }
            let next = patchula_github::next_link(&response, &url).map_err(|_| Error::Invalid)?;
            let Some(next) = next else {
                return Ok(None);
            };
            if next.origin() != origin
                || decode(next.path()) != decode(&expected_path)
                || !next.username().is_empty()
                || next.password().is_some()
            {
                return Err(Error::Invalid);
            }
            pages.follow(next.as_str())?;
            url = next;
            response = self
                .registry(host, path, url.as_str(), Read::Referrers)
                .await?;
        }
    }

    async fn read_referrer_index(
        &self,
        host: &str,
        path: &str,
        candidate: &str,
        index: &Value,
    ) -> Result<Option<Compact>> {
        let subject = candidate.trim_start_matches("sha256:");
        for descriptor in index["manifests"].as_array().into_iter().flatten() {
            if !referrer_descriptor_candidate(descriptor) {
                continue;
            }
            // A referrers index is multi-writer and may list non-sha256
            // digests or manifests deleted since listing; skip those rather
            // than aborting the rest of the scan.
            let Some(reference) = descriptor["digest"]
                .as_str()
                .filter(|d| validate_digest(d).is_ok())
            else {
                continue;
            };
            let response = match self
                .registry(
                    host,
                    path,
                    &format!("https://{host}/v2/{path}/manifests/{reference}"),
                    Read::Manifest,
                )
                .await
            {
                Ok(response) => response,
                Err(Error::Http(patchula_http::CacheError::Status(404))) => continue,
                Err(error) => return Err(error),
            };
            let manifest = verified(&response.body, reference)?;
            let Some(manifest_subject) = manifest["subject"]["digest"].as_str() else {
                continue;
            };
            if validate_digest(manifest_subject).is_err()
                || !manifest_subject.eq_ignore_ascii_case(candidate)
            {
                continue;
            }
            let manifest_annotations = &manifest["annotations"];
            if predicate_annotation(manifest_annotations)
                .is_some_and(|predicate| !is_spdx_predicate(predicate))
                || manifest_annotations["dev.sigstore.bundle.content"]
                    .as_str()
                    .is_some_and(|c| c != "dsse-envelope")
            {
                continue;
            }
            for layer in manifest["layers"].as_array().into_iter().flatten() {
                let media = layer["mediaType"].as_str().unwrap_or_default();
                let layer_predicate = predicate_annotation(&layer["annotations"]);
                let attestation = layer_predicate.is_some()
                    || is_sigstore_bundle(media)
                    || matches!(
                        media,
                        "application/vnd.in-toto+json" | "application/vnd.dsse.envelope.v1+json"
                    );
                let wanted = match layer_predicate {
                    Some(predicate) => is_spdx_predicate(predicate),
                    None => attestation || media.contains("spdx"),
                };
                if !wanted {
                    continue;
                }
                let Some(compact) = self.sbom_blob(host, path, layer).await? else {
                    continue;
                };
                if (attestation || !compact.subjects.is_empty())
                    && !compact
                        .subjects
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(subject))
                {
                    continue;
                }
                return Ok(Some(compact));
            }
        }
        Ok(None)
    }

    async fn image_platform(&self, host: &str, path: &str, digest: &str) -> Result<Platform> {
        let url = format!("https://{host}/v2/{path}/blobs/{digest}");
        let response = self
            .registry(
                host,
                path,
                &url,
                Read::Blob {
                    accept: BLOB,
                    derived: IMAGE_PLATFORM,
                },
            )
            .await?;
        let platform: Platform = serde_json::from_slice(&response.body)?;
        if format!("sha256:{}", platform.sha256) != digest {
            return Err(Error::Invalid);
        }
        Ok(platform)
    }

    async fn sbom_blob(&self, host: &str, path: &str, layer: &Value) -> Result<Option<Compact>> {
        let digest = layer["digest"].as_str().ok_or(Error::Invalid)?;
        validate_digest(digest)?;
        let url = format!("https://{host}/v2/{path}/blobs/{digest}");
        let response = match self
            .registry(
                host,
                path,
                &url,
                Read::Blob {
                    accept: BLOB,
                    derived: SBOM_OS,
                },
            )
            .await
        {
            Ok(response) => response,
            Err(Error::Http(patchula_http::CacheError::Document)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let compact: Compact = serde_json::from_slice(&response.body)?;
        if !format!("sha256:{}", compact.sha256).eq_ignore_ascii_case(digest) {
            return Err(Error::Invalid);
        }
        Ok(Some(compact))
    }
}

fn found(compact: Compact, source: SbomSource) -> ImageSbom {
    let kind = compact.kinds.iter().next().cloned().unwrap_or_default();
    let ecosystem = compact
        .namespace
        .as_deref()
        .filter(|_| compact.kinds.len() <= 1)
        .and_then(|namespace| ecosystem(&kind, namespace, compact.distro.as_ref()));
    ImageSbom::Found(Sbom {
        source,
        cataloger: CATALOGERS.iter().any(|c| compact.generator.contains(c)),
        generator: compact.generator,
        packages: compact.packages,
        ecosystem,
        os: compact.distro.map(|(name, version)| {
            if version.is_empty() {
                name
            } else {
                format!("{name} {version}")
            }
        }),
        sources: compact.sources,
        sources_known: compact.sources_known,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(document: Value) -> Compact {
        let bytes = compact_sbom(&mut &serde_json::to_vec(&document).unwrap()[..]).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    fn package(purl: &str) -> Value {
        json!({"name": "x", "externalRefs": [{"referenceType": "purl", "referenceLocator": purl}]})
    }

    #[test]
    fn syft_upstream_names_the_source_package_and_the_kernel_is_left_out() {
        let compact = parse(json!({
            "spdxVersion": "SPDX-2.3",
            "creationInfo": {"creators": ["Tool: syft-v1.51.0"]},
            "packages": [
                package("pkg:deb/debian/bash@5.2.37-2%2Bb10?arch=amd64&distro=debian-13.7&upstream=bash%405.2.37-2"),
                package("pkg:deb/debian/libssl3t64@3.5.1-1?arch=amd64&distro=debian-13.7&upstream=openssl"),
                package("pkg:deb/debian/linux-libc-dev@6.12.48-1?arch=amd64&distro=debian-13.7&upstream=linux"),
                package("pkg:golang/golang.org/x/net@v0.38.0")
            ]
        }));
        assert!(compact.sources_known);
        assert_eq!(compact.packages, 3);
        assert_eq!(compact.distro, Some(("debian".into(), "13.7".into())));
        assert_eq!(
            compact.sources,
            BTreeSet::from([
                ("bash".to_string(), "5.2.37-2".to_string()),
                ("openssl".to_string(), "3.5.1-1".to_string())
            ])
        );
        assert_eq!(
            ecosystem("deb", "debian", compact.distro.as_ref()).as_deref(),
            Some("Debian:13")
        );
    }

    #[test]
    fn scout_source_entries_are_queried_as_listed() {
        let compact = parse(json!({
            "_type": "https://in-toto.io/Statement/v0.1",
            "predicateType": SPDX,
            "subject": [],
            "predicate": {
                "spdxVersion": "SPDX-2.3",
                "creationInfo": {"creators": ["Organization: Docker, Inc", "Tool: docker-scout-1.18.1"]},
                "packages": [
                    package("pkg:apk/alpine/libssl3@3.3.7-r0?os_name=alpine&os_version=3.20"),
                    package("pkg:apk/alpine/openssl@3.3.7-r0?os_name=alpine&os_version=3.20")
                ]
            }
        }));
        assert!(compact.sources_known);
        assert!(
            compact
                .sources
                .contains(&("openssl".into(), "3.3.7-r0".into()))
        );
        assert_eq!(
            ecosystem("apk", "alpine", compact.distro.as_ref()).as_deref(),
            Some("Alpine:v3.20")
        );
    }

    #[test]
    fn ecosystems_follow_osv_naming() {
        let release = |v: &str| Some(("x".to_string(), v.to_string()));
        let name = |kind, namespace, v: &str| ecosystem(kind, namespace, release(v).as_ref());
        assert_eq!(
            name("deb", "ubuntu", "24.04").as_deref(),
            Some("Ubuntu:24.04:LTS")
        );
        assert_eq!(
            name("deb", "ubuntu", "24.10").as_deref(),
            Some("Ubuntu:24.10")
        );
        assert_eq!(
            name("deb", "ubuntu", "23.04").as_deref(),
            Some("Ubuntu:23.04")
        );
        assert_eq!(name("deb", "debian", "12").as_deref(), Some("Debian:12"));
        assert_eq!(name("rpm", "redhat", "9.4"), None);
        assert_eq!(ecosystem("deb", "debian", None), None);
    }

    #[test]
    fn a_dsse_envelope_keeps_its_subjects() {
        let statement = json!({
            "predicateType": SPDX,
            "subject": [{"name": "x", "digest": {"sha256": "ab"}}],
            "predicate": {"spdxVersion": "SPDX-2.3", "creationInfo": {"creators": ["Tool: ko"]}, "packages": []}
        });
        let payload = STANDARD.encode(serde_json::to_vec(&statement).unwrap());
        let compact = parse(
            json!({"payloadType": "application/vnd.in-toto+json", "payload": payload, "signatures": []}),
        );
        assert_eq!(compact.subjects, vec!["ab".to_string()]);
        assert_eq!(compact.packages, 0);
        assert_eq!(compact.generator, "ko");
    }

    #[test]
    fn other_predicates_are_not_sboms() {
        let provenance = json!({"predicateType": "https://slsa.dev/provenance/v1", "subject": [], "predicate": {}});
        assert!(compact_sbom(&mut &serde_json::to_vec(&provenance).unwrap()[..]).is_err());
        let message_signature = json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "messageSignature": {"messageDigest": {"algorithm": "SHA2_256", "digest": "AA=="}, "signature": "AA=="}
        });
        assert!(compact_sbom(&mut &serde_json::to_vec(&message_signature).unwrap()[..]).is_err());
    }

    #[test]
    fn a_sigstore_bundle_unwraps_its_spdx_v2_3_statement() {
        let statement = json!({
            "_type": "https://in-toto.io/Statement/v1",
            "predicateType": "https://spdx.dev/Document/v2.3",
            "subject": [{"name": "ghcr.io/sweepies/count-patchula", "digest": {"sha256": "cd"}}],
            "predicate": {
                "spdxVersion": "SPDX-2.3",
                "creationInfo": {"creators": ["Tool: apko (v1.4.13)", "Organization: Chainguard, Inc"]},
                "packages": [
                    package("pkg:apk/wolfi/libcurl-openssl4@8.23.0-r4?arch=x86_64&origin=curl")
                ]
            }
        });
        let payload = STANDARD.encode(serde_json::to_vec(&statement).unwrap());
        let compact = parse(json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {"certificate": {"rawBytes": "ignored"}},
            "dsseEnvelope": {
                "payloadType": "application/vnd.in-toto+json",
                "payload": payload,
                "signatures": [{"sig": "ignored"}]
            }
        }));
        assert_eq!(compact.subjects, vec!["cd".to_string()]);
        assert_eq!(compact.generator, "apko (v1.4.13)");
        assert!(compact.sources_known);
        assert!(
            compact
                .sources
                .contains(&("curl".into(), "8.23.0-r4".into()))
        );
    }

    #[test]
    fn apko_wolfi_sboms_use_origin_qualifiers_rolling_distro_and_ignore_unknown_namespace() {
        let compact = parse(json!({
            "spdxVersion": "SPDX-2.3",
            "creationInfo": {"creators": ["Tool: apko (v1.4.13)"]},
            "relationships": [
                {
                    "spdxElementId": "SPDXRef-Package-apk-libcurl-openssl4-8.23.0-r4",
                    "relationshipType": "GENERATED_FROM",
                    "relatedSpdxElement": "SPDXRef-Package-Source-curl"
                },
                {
                    "spdxElementId": "SPDXRef-Package-apk-sqlite-libs-3.54.1-r1",
                    "relationshipType": "GENERATED_FROM",
                    "relatedSpdxElement": "SPDXRef-Package-Source-sqlite"
                }
            ],
            "packages": [
                package("pkg:apk/unknown/patchula@0.11.1-r0?arch=x86_64"),
                {
                    "SPDXID": "SPDXRef-Package-apk-libcurl-openssl4-8.23.0-r4",
                    "name": "libcurl-openssl4",
                    "externalRefs": [{"referenceType": "purl", "referenceLocator": "pkg:apk/wolfi/libcurl-openssl4@8.23.0-r4?arch=x86_64&distro=wolfi"}]
                },
                {
                    "SPDXID": "SPDXRef-Package-apk-libcurl-openssl4-deadbeef",
                    "name": "libcurl-openssl4",
                    "externalRefs": [{"referenceType": "purl", "referenceLocator": "pkg:apk/wolfi/libcurl-openssl4@8.23.0-r4?arch=x86_64&origin=curl"}]
                },
                {
                    "SPDXID": "SPDXRef-Package-apk-sqlite-libs-3.54.1-r1",
                    "name": "sqlite-libs",
                    "externalRefs": [{"referenceType": "purl", "referenceLocator": "pkg:apk/wolfi/sqlite-libs@3.54.1-r1?arch=x86_64&distro=wolfi"}]
                },
                {
                    "SPDXID": "SPDXRef-Package-apk-sqlite-libs-cafebabe",
                    "name": "sqlite-libs",
                    "externalRefs": [{"referenceType": "purl", "referenceLocator": "pkg:apk/wolfi/sqlite-libs@3.54.1-r1?arch=x86_64&origin=sqlite"}]
                }
            ]
        }));
        let ImageSbom::Found(sbom) = found(compact, SbomSource::Referrer) else {
            panic!("expected Found");
        };
        assert_eq!(sbom.source, SbomSource::Referrer);
        assert!(sbom.cataloger);
        assert!(sbom.sources_known);
        assert_eq!(sbom.ecosystem.as_deref(), Some("Wolfi"));
        assert_eq!(sbom.os.as_deref(), Some("wolfi"));
        assert!(sbom.sources.contains(&("curl".into(), "8.23.0-r4".into())));
        assert!(
            sbom.sources
                .contains(&("sqlite".into(), "3.54.1-r1".into()))
        );
        assert!(
            !sbom
                .sources
                .contains(&("libcurl-openssl4".into(), "8.23.0-r4".into()))
        );
        assert!(
            !sbom
                .sources
                .contains(&("sqlite-libs".into(), "3.54.1-r1".into()))
        );
    }

    #[test]
    fn spdx_predicate_types_cover_spdx_v2_only() {
        assert!(is_spdx_predicate("https://spdx.dev/Document"));
        assert!(is_spdx_predicate("https://spdx.dev/Document/v2.2"));
        assert!(is_spdx_predicate("https://spdx.dev/Document/v2.3"));
        assert!(!is_spdx_predicate("https://spdx.dev/Document/v3.0"));
        assert!(!is_spdx_predicate("https://spdx.dev/Document/v2."));
        assert!(!is_spdx_predicate("https://slsa.dev/provenance/v1"));
    }

    #[test]
    fn sigstore_bundle_media_types_cover_v0_1_through_v0_3_and_empty_upstream_falls_back_to_origin()
    {
        for media_type in [
            "application/vnd.dev.sigstore.bundle+json",
            "application/vnd.dev.sigstore.bundle+json;version=0.1",
            "application/vnd.dev.sigstore.bundle+json;version=0.2",
            "application/vnd.dev.sigstore.bundle+json;version=0.3",
            "application/vnd.dev.sigstore.bundle.v0.3+json",
        ] {
            assert!(is_sigstore_bundle(media_type), "{media_type}");
        }
        assert!(!is_sigstore_bundle("application/vnd.cncf.notary.signature"));
        assert!(!is_sigstore_bundle(
            "application/vnd.dev.sigstore.bundleextra+json"
        ));
        let compact = parse(json!({
            "spdxVersion": "SPDX-2.3",
            "creationInfo": {"creators": ["Tool: apko (v1.4.13)"]},
            "packages": [
                package("pkg:apk/wolfi/libcurl-openssl4@8.23.0-r4?upstream=&origin=curl")
            ]
        }));
        assert!(compact.sources_known);
        assert!(
            compact
                .sources
                .contains(&("curl".into(), "8.23.0-r4".into()))
        );
        assert!(
            !compact
                .sources
                .contains(&("libcurl-openssl4".into(), "8.23.0-r4".into()))
        );
    }

    /// A reader that hands out `size` bytes a call, so a streamed derive can
    /// be checked against every chunk boundary.
    struct Chunks<'a> {
        bytes: &'a [u8],
        size: usize,
    }
    impl std::io::Read for Chunks<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let take = self.bytes.len().min(self.size).min(buffer.len());
            buffer[..take].copy_from_slice(&self.bytes[..take]);
            self.bytes = &self.bytes[take..];
            Ok(take)
        }
    }

    /// The hashed body's digest — and so the derived platform — is the same
    /// however the bytes arrive.
    #[test]
    fn the_platform_digest_matches_at_every_chunk() {
        let body = br#"{"os":"linux","architecture":"amd64","variant":"v1"}"#;
        let expected = compact_platform(&mut &body[..]).unwrap();
        for chunk in 1..=16 {
            let mut chunks = Chunks {
                bytes: &body[..],
                size: chunk,
            };
            assert_eq!(
                compact_platform(&mut chunks).unwrap(),
                expected,
                "chunk size {chunk}"
            );
        }
    }
}
