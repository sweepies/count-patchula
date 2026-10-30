//! Published SBOMs of container images, read by digest. The OS packages they
//! list become OSV queries under their source package names. Image layers are
//! never read.
use super::*;
use crate::docker::{Derive, MANIFEST, identity, validate_digest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::Method;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const SPDX: &str = "https://spdx.dev/Document";
const BLOB: &str = "*/*";
/// Generators that catalog a whole image, so an empty OS list means none.
const CATALOGERS: [&str; 3] = ["docker-scout", "syft", "apko"];

/// Where an image's SBOM was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SbomSource {
    Attestation,
    CosignAtt,
    CosignSbom,
}
impl SbomSource {
    pub fn word(self) -> &'static str {
        match self {
            Self::Attestation => "attestation",
            Self::CosignAtt => "cosign-att",
            Self::CosignSbom => "cosign-sbom",
        }
    }
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

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn verified(body: &[u8], digest: &str) -> Result<Value> {
    if format!("sha256:{}", hex(body)) != digest {
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

/// Read an SPDX 2.3 document from an in-toto statement, a DSSE envelope
/// around one, or on its own.
fn compact_sbom(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    use patchula_http::CacheError::Document;
    let sha256 = hex(bytes);
    let mut value: Value = serde_json::from_slice(bytes).map_err(|_| Document)?;
    if let Some(payload) = value["payload"].as_str() {
        let decoded = STANDARD.decode(payload).map_err(|_| Document)?;
        value = serde_json::from_slice(&decoded).map_err(|_| Document)?;
    }
    let subjects = value["subject"]
        .as_array()
        .map(|s| {
            s.iter()
                .filter_map(|s| s["digest"]["sha256"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if value.get("predicate").is_some() {
        if value["predicateType"] != SPDX {
            return Err(Document);
        }
        value = value["predicate"].take();
    }
    if !value["spdxVersion"]
        .as_str()
        .is_some_and(|v| v.starts_with("SPDX-2."))
    {
        return Err(Document);
    }
    let generator = value["creationInfo"]["creators"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str()?.strip_prefix("Tool: "))
        .collect::<Vec<_>>()
        .join(", ");
    // A binary GENERATED_FROM a source entry is covered by that entry.
    let derived: BTreeSet<&str> = value["relationships"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["relationshipType"] == "GENERATED_FROM")
        .filter_map(|r| r["spdxElementId"].as_str())
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
    for package in value["packages"].as_array().into_iter().flatten() {
        let listed = package["SPDXID"]
            .as_str()
            .is_some_and(|id| derived.contains(id));
        for reference in package["externalRefs"].as_array().into_iter().flatten() {
            if reference["referenceType"] != "purl" {
                continue;
            }
            let Some((kind, namespace, name, version, qualifiers)) =
                reference["referenceLocator"].as_str().and_then(purl)
            else {
                continue;
            };
            if !matches!(kind.as_str(), "deb" | "apk" | "rpm") {
                continue;
            }
            compact.packages += 1;
            compact.kinds.insert(kind.clone());
            compact.namespace.get_or_insert(namespace);
            if compact.distro.is_none() {
                compact.distro = match (qualifiers.get("os_name"), qualifiers.get("os_version")) {
                    (Some(name), Some(version)) => {
                        Some((name.to_ascii_lowercase(), version.clone()))
                    }
                    _ => qualifiers.get("distro").and_then(|d| {
                        let (name, version) = d.split_once('-')?;
                        Some((name.to_ascii_lowercase(), version.to_owned()))
                    }),
                };
            }
            let source = match qualifiers.get("upstream") {
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
            .registry_with(host, path, &url(digest), MANIFEST, Method::GET, None)
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
            let Some(child) = manifests.iter().find(|m| {
                m["platform"]["os"] == os
                    && m["platform"]["architecture"] == architecture
                    && variant.is_none_or(|v| m["platform"]["variant"] == v)
            }) else {
                let first = manifests
                    .iter()
                    .map(|m| &m["platform"])
                    .filter(|p| p["os"] != "unknown")
                    .find_map(|p| {
                        let os = p["os"].as_str()?;
                        let architecture = p["architecture"].as_str()?;
                        Some(match p["variant"].as_str() {
                            Some(variant) => format!("{os}/{architecture}/{variant}"),
                            None => format!("{os}/{architecture}"),
                        })
                    });
                return Ok(ImageSbom::NoPlatform(first));
            };
            let child = child["digest"].as_str().ok_or(Error::Invalid)?.to_owned();
            validate_digest(&child)?;
            let attestation = manifests.iter().find(|m| {
                m["annotations"]["vnd.docker.reference.type"] == "attestation-manifest"
                    && m["annotations"]["vnd.docker.reference.digest"] == child.as_str()
            });
            if let Some(attestation) = attestation {
                let reference = attestation["digest"].as_str().ok_or(Error::Invalid)?;
                validate_digest(reference)?;
                let response = self
                    .registry_with(host, path, &url(reference), MANIFEST, Method::GET, None)
                    .await?;
                let manifest = verified(&response.body, reference)?;
                for layer in manifest["layers"].as_array().into_iter().flatten() {
                    if layer["annotations"]["in-toto.io/predicate-type"] == SPDX
                        && let Some(compact) = self.sbom_blob(host, path, layer).await?
                    {
                        return Ok(found(compact, SbomSource::Attestation));
                    }
                }
            }
            candidates.insert(0, child);
        }
        // cosign attaches by tag. The platform digest comes first, since some
        // publishers list packages only there.
        let kinds = [
            (".att", SbomSource::CosignAtt),
            (".sbom", SbomSource::CosignSbom),
        ];
        for (suffix, source) in kinds {
            for candidate in &candidates {
                let tag = format!("{}{suffix}", candidate.replace(':', "-"));
                let head = match self
                    .registry_with(host, path, &url(&tag), MANIFEST, Method::HEAD, None)
                    .await
                {
                    Ok(head) => head,
                    Err(Error::Http(patchula_http::CacheError::Status(404))) => continue,
                    Err(error) => return Err(error),
                };
                // The tag moves, but the manifest it names is read by digest,
                // so a warm run reads it from the cache.
                let named = head
                    .headers
                    .get("docker-content-digest")
                    .and_then(|v| v.to_str().ok())
                    .filter(|d| validate_digest(d).is_ok())
                    .map(str::to_owned);
                let manifest: Value = match &named {
                    Some(named) => {
                        let response = self
                            .registry_with(host, path, &url(named), MANIFEST, Method::GET, None)
                            .await?;
                        verified(&response.body, named)?
                    }
                    None => {
                        let response = self
                            .registry_with(host, path, &url(&tag), MANIFEST, Method::GET, None)
                            .await?;
                        serde_json::from_slice(&response.body)?
                    }
                };
                for layer in manifest["layers"].as_array().into_iter().flatten() {
                    let wanted = if source == SbomSource::CosignAtt {
                        layer["annotations"]["predicateType"] == SPDX
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
                        && !compact.subjects.iter().any(|s| s == subject)
                    {
                        continue;
                    }
                    return Ok(found(compact, source));
                }
            }
        }
        Ok(ImageSbom::Missing)
    }

    async fn sbom_blob(&self, host: &str, path: &str, layer: &Value) -> Result<Option<Compact>> {
        let digest = layer["digest"].as_str().ok_or(Error::Invalid)?;
        validate_digest(digest)?;
        let url = format!("https://{host}/v2/{path}/blobs/{digest}");
        let derive: Derive = compact_sbom;
        let response = match self
            .registry_with(
                host,
                path,
                &url,
                BLOB,
                Method::GET,
                Some(("sbom-os-v1", derive)),
            )
            .await
        {
            Ok(response) => response,
            Err(Error::Http(patchula_http::CacheError::Document)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let compact: Compact = serde_json::from_slice(&response.body)?;
        if format!("sha256:{}", compact.sha256) != digest {
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
        os: compact
            .distro
            .map(|(name, version)| format!("{name} {version}")),
        sources: compact.sources,
        sources_known: compact.sources_known,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(document: Value) -> Compact {
        let bytes = compact_sbom(&serde_json::to_vec(&document).unwrap()).unwrap();
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
        assert!(compact_sbom(&serde_json::to_vec(&provenance).unwrap()).is_err());
    }
}
