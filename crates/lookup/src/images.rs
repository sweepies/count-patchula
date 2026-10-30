//! Whether a new image digest fixes advisories its old digest has, from the OS
//! packages their SBOMs list. Only OSV's answers decide a fix.
use patchula_datasource::{Advisory, AdvisoryListing, AdvisoryQuery, ImageSbom, Sbom};
use patchula_report::{ImageAdvisory, ImageCheck};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// OSV's records for each queried source package, for the advisories read.
pub type Found = BTreeMap<AdvisoryQuery, Vec<Arc<Advisory>>>;

/// How many fixed, and how many introduced, advisories per image update are
/// read from OSV.dev in full. The rest are listed by ID.
pub const SHOWN: usize = 5;

/// Advisory IDs, most severe first, judged from the ID alone, since OSV's batch
/// answer carries no severity: a distro's own security notice (DSA, DLA, USN
/// and the like) ranks above a per-CVE tracker entry, then newer numbers rank
/// above older ones.
pub fn most_severe<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut ids: Vec<_> = ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ids.sort_by_cached_key(|id| {
        let numbers: Vec<u64> = id
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse().ok())
            .collect();
        (id.contains("CVE-"), std::cmp::Reverse(numbers), *id)
    });
    ids
}
/// One digest's SBOM per platform, or why it could not be read.
pub type Side = BTreeMap<String, Result<ImageSbom, String>>;

/// States from worst to best; a comparison takes the worst of its platforms.
const STATES: [&str; 8] = [
    "unreadable",
    "previous-unavailable",
    "no-sbom",
    "sbom-without-os",
    "os-not-covered",
    "partial-sources",
    "checked",
    "no-os-packages",
];

/// Whether a state means the Count can't tell, so it is a warning.
pub fn warns(state: &str) -> bool {
    matches!(
        state,
        "previous-unavailable" | "no-sbom" | "sbom-without-os" | "os-not-covered"
    )
}

fn rank(state: &str) -> usize {
    STATES.iter().position(|s| *s == state).unwrap_or(0)
}

fn sbom_state(sbom: &Sbom) -> &'static str {
    if sbom.packages == 0 {
        if sbom.cataloger {
            "no-os-packages"
        } else {
            "sbom-without-os"
        }
    } else if sbom.ecosystem.is_none() {
        "os-not-covered"
    } else if !sbom.sources_known {
        "partial-sources"
    } else {
        "checked"
    }
}

/// The platform to read instead when a digest has none of the configured
/// platforms: the first one its index lists.
pub fn fallback(side: &Side) -> Option<String> {
    let mut first = None;
    for sbom in side.values() {
        let Ok(ImageSbom::NoPlatform(listed)) = sbom else {
            return None;
        };
        first = first.or(listed.clone());
    }
    first
}

/// The queries one side needs answered, for the batched OSV call.
pub fn image_queries(side: &Side) -> BTreeSet<AdvisoryQuery> {
    side.values()
        .filter_map(|s| match s {
            Ok(ImageSbom::Found(sbom)) => Some(sbom.queries()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[derive(Default)]
struct Check {
    state: Option<&'static str>,
    detail: Option<String>,
}
impl Check {
    fn worse(&mut self, state: &'static str, detail: Option<String>) {
        if self.state.is_none_or(|s| rank(state) < rank(s)) {
            self.state = Some(state);
            self.detail = detail;
        }
    }
}

/// The current digest on its own, for the dashboard: its warning state, if
/// any. A digest that could not be read is not a warning.
pub fn image_state(side: &Side) -> Option<ImageCheck> {
    let mut check = Check::default();
    let mut platforms = Vec::new();
    let mut os = None;
    for (platform, sbom) in side {
        match sbom {
            Ok(ImageSbom::Found(sbom)) => {
                check.worse(sbom_state(sbom), Some(sbom.generator.clone()));
                os = os.or(sbom.os.clone());
            }
            Ok(ImageSbom::Missing) => check.worse("no-sbom", None),
            Ok(ImageSbom::NoPlatform(_)) => continue,
            Ok(ImageSbom::Gone) | Err(_) => return None,
        }
        platforms.push(platform.clone());
    }
    let state = check.state.filter(|s| warns(s))?;
    Some(ImageCheck {
        state: state.into(),
        platforms,
        os,
        detail: check.detail,
        ..ImageCheck::default()
    })
}

/// Compare an old and a new digest. None when they share no compared
/// platform.
pub fn compare(
    (old_digest, old): (&str, &Side),
    (new_digest, new): (&str, &Side),
    listing: &AdvisoryListing,
) -> Option<ImageCheck> {
    let mut check = Check::default();
    let mut result = ImageCheck::default();
    let mut fixed = BTreeMap::<(String, String, String, Option<String>), Vec<String>>::new();
    let mut introduced = BTreeSet::new();
    let mut sboms = BTreeSet::new();
    for (platform, after) in new {
        let Some(before) = old.get(platform) else {
            continue;
        };
        let (before, after) = match (before, after) {
            (Err(e), _) | (_, Err(e)) => {
                check.worse("unreadable", Some(e.clone()));
                result.platforms.push(platform.clone());
                continue;
            }
            (Ok(ImageSbom::NoPlatform(_)), _) | (_, Ok(ImageSbom::NoPlatform(_))) => continue,
            (_, Ok(ImageSbom::Gone)) => {
                check.worse(
                    "unreadable",
                    Some("the new digest is not in its registry".into()),
                );
                result.platforms.push(platform.clone());
                continue;
            }
            (Ok(ImageSbom::Gone), _) | (Ok(ImageSbom::Missing), Ok(ImageSbom::Found(_))) => {
                check.worse("previous-unavailable", None);
                result.platforms.push(platform.clone());
                continue;
            }
            (Ok(ImageSbom::Found(before)), Ok(ImageSbom::Found(after))) => (before, after),
            _ => {
                check.worse("no-sbom", None);
                result.platforms.push(platform.clone());
                continue;
            }
        };
        result.platforms.push(platform.clone());
        for (digest, sbom) in [(old_digest, before), (new_digest, after)] {
            check.worse(sbom_state(sbom), Some(sbom.generator.clone()));
            sboms.insert(patchula_report::ImageSbom {
                digest: digest.into(),
                source: sbom.source.word().into(),
                generator: sbom.generator.clone(),
            });
        }
        result.os = result.os.or(after.os.clone());
        result.packages = result.packages.max(after.packages);
        if before.ecosystem.is_none() || after.ecosystem.is_none() {
            continue;
        }
        let ids = |sbom: &Sbom| {
            let mut ids = BTreeMap::<String, Vec<AdvisoryQuery>>::new();
            for query in sbom.queries() {
                for (id, _) in listing.get(&query).into_iter().flatten() {
                    ids.entry(id.clone()).or_default().push(query.clone());
                }
            }
            ids
        };
        let (was, now) = (ids(before), ids(after));
        let versions: BTreeMap<_, _> = after.sources.iter().cloned().collect();
        // An advisory OSV lists for several source packages is fixed in each.
        for (id, queries) in &was {
            if now.contains_key(id) {
                continue;
            }
            for query in queries {
                let key = (
                    id.clone(),
                    query.package.clone(),
                    query.version.clone(),
                    versions.get(&query.package).cloned(),
                );
                fixed.entry(key).or_default().push(platform.clone());
            }
        }
        introduced.extend(now.into_keys().filter(|id| !was.contains_key(id)));
    }
    result.state = check.state?.into();
    result.detail = check
        .detail
        .filter(|_| matches!(result.state.as_str(), "unreadable" | "sbom-without-os"));
    result.sboms = sboms.into_iter().collect();
    result.fixed = fixed
        .into_iter()
        .map(|((id, package, old, new), platforms)| ImageAdvisory {
            id,
            package,
            old,
            new,
            platforms,
        })
        .collect();
    result.introduced = most_severe(introduced.iter().map(String::as_str))
        .into_iter()
        .map(str::to_owned)
        .collect();
    Some(result)
}

/// Report entries for the fixed and the introduced advisories whose records
/// were read.
pub fn advisories(
    check: &ImageCheck,
    found: &Found,
) -> (
    Vec<patchula_report::Advisory>,
    Vec<patchula_report::Advisory>,
) {
    (
        records(check.fixed.iter().map(|f| f.id.as_str()), found),
        records(check.introduced.iter().map(String::as_str), found),
    )
}

fn records<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    found: &Found,
) -> Vec<patchula_report::Advisory> {
    let ids: BTreeSet<_> = ids.into_iter().collect();
    let mut seen = BTreeSet::new();
    let mut entries: Vec<_> = found
        .values()
        .flatten()
        .filter(|a| ids.contains(a.id.as_str()) && seen.insert(a.id.clone()))
        .map(|advisory| patchula_report::Advisory {
            id: advisory.id.clone(),
            aliases: advisory.aliases.clone(),
            severity: advisory.severity.clone(),
            summary: advisory.summary.clone(),
            url: format!("https://osv.dev/vulnerability/{}", advisory.id),
            fixed_in: None,
        })
        .collect();
    entries.sort();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_datasource::SbomSource;

    fn sbom(sources: &[(&str, &str)]) -> ImageSbom {
        ImageSbom::Found(Sbom {
            source: SbomSource::Attestation,
            generator: "docker-scout-1.18.1".into(),
            cataloger: true,
            packages: sources.len(),
            ecosystem: Some("Debian:12".into()),
            os: Some("debian 12".into()),
            sources: sources
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            sources_known: true,
        })
    }
    fn side(platforms: &[(&str, ImageSbom)]) -> Side {
        platforms
            .iter()
            .map(|(p, s)| ((*p).to_owned(), Ok(s.clone())))
            .collect()
    }
    fn advisory(id: &str) -> Arc<Advisory> {
        Arc::new(Advisory {
            id: id.into(),
            modified: String::new(),
            aliases: Vec::new(),
            summary: None,
            severity: None,
            affected: Vec::new(),
        })
    }
    fn listing(found: &Found) -> AdvisoryListing {
        found
            .iter()
            .map(|(q, a)| {
                (
                    q.clone(),
                    a.iter()
                        .map(|a| (a.id.clone(), a.modified.clone()))
                        .collect(),
                )
            })
            .collect()
    }
    fn query(package: &str, version: &str) -> AdvisoryQuery {
        AdvisoryQuery {
            ecosystem: "Debian:12".into(),
            package: package.into(),
            version: version.into(),
        }
    }

    #[test]
    fn fixed_removed_and_introduced_advisories_are_told_apart() {
        let old = side(&[
            (
                "linux/amd64",
                sbom(&[("openssl", "1"), ("glibc", "1"), ("perl", "1")]),
            ),
            (
                "linux/arm64",
                sbom(&[("openssl", "1"), ("glibc", "1"), ("perl", "1")]),
            ),
        ]);
        let new = side(&[
            ("linux/amd64", sbom(&[("openssl", "2"), ("glibc", "1")])),
            ("linux/arm64", sbom(&[("openssl", "1"), ("glibc", "1")])),
        ]);
        let found = Found::from([
            (query("openssl", "1"), vec![advisory("DSA-1")]),
            (query("glibc", "1"), vec![advisory("DSA-2")]),
            (query("perl", "1"), vec![advisory("DSA-3")]),
            (query("openssl", "2"), vec![advisory("DSA-4")]),
        ]);
        let check = compare(("sha256:a", &old), ("sha256:b", &new), &listing(&found)).unwrap();
        assert_eq!(check.state, "checked");
        assert_eq!(check.platforms, ["linux/amd64", "linux/arm64"]);
        let fixed: Vec<_> = check
            .fixed
            .iter()
            .map(|f| (f.id.as_str(), f.new.as_deref(), f.platforms.join(",")))
            .collect();
        assert_eq!(
            fixed,
            [
                ("DSA-1", Some("2"), "linux/amd64".to_owned()),
                ("DSA-3", None, "linux/amd64,linux/arm64".to_owned()),
            ]
        );
        assert_eq!(check.introduced, ["DSA-4"]);
        let entries = advisories(&check, &found).0;
        assert_eq!(
            entries.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["DSA-1", "DSA-3"]
        );
    }

    #[test]
    fn an_advisory_listed_for_two_packages_is_fixed_in_each() {
        let old = side(&[("linux/amd64", sbom(&[("openssl", "1"), ("libssl", "1")]))]);
        let new = side(&[("linux/amd64", sbom(&[("openssl", "2"), ("libssl", "3")]))]);
        let found = Found::from([
            (query("openssl", "1"), vec![advisory("DSA-1")]),
            (query("libssl", "1"), vec![advisory("DSA-1")]),
        ]);
        let check = compare(("sha256:a", &old), ("sha256:b", &new), &listing(&found)).unwrap();
        let fixed: Vec<_> = check
            .fixed
            .iter()
            .map(|f| (f.id.as_str(), f.package.as_str(), f.new.as_deref()))
            .collect();
        assert_eq!(
            fixed,
            [
                ("DSA-1", "libssl", Some("3")),
                ("DSA-1", "openssl", Some("2")),
            ]
        );
    }

    #[test]
    fn distro_notices_rank_above_tracker_entries_and_newer_above_older() {
        assert_eq!(
            most_severe([
                "DEBIAN-CVE-2023-4039",
                "DSA-5678-1",
                "DEBIAN-CVE-2025-26519",
                "DSA-5912-1",
                "DEBIAN-CVE-2025-9143",
                "DSA-5678-1",
            ]),
            [
                "DSA-5912-1",
                "DSA-5678-1",
                "DEBIAN-CVE-2025-26519",
                "DEBIAN-CVE-2025-9143",
                "DEBIAN-CVE-2023-4039",
            ]
        );
    }

    #[test]
    fn the_worst_platform_decides_the_state() {
        let ok = sbom(&[("openssl", "1")]);
        let ko = ImageSbom::Found(Sbom {
            generator: "ko".into(),
            cataloger: false,
            packages: 0,
            sources: BTreeSet::new(),
            ..match sbom(&[]) {
                ImageSbom::Found(s) => s,
                _ => unreachable!(),
            }
        });
        let state = |old: Side, new: Side| {
            compare(
                ("sha256:a", &old),
                ("sha256:b", &new),
                &AdvisoryListing::new(),
            )
            .map(|c| c.state)
        };
        let one = |s: &ImageSbom| side(&[("linux/amd64", s.clone())]);
        assert_eq!(
            state(one(&ok), one(&ko)).as_deref(),
            Some("sbom-without-os")
        );
        assert_eq!(
            state(one(&ImageSbom::Missing), one(&ok)).as_deref(),
            Some("previous-unavailable")
        );
        assert_eq!(
            state(one(&ImageSbom::Gone), one(&ok)).as_deref(),
            Some("previous-unavailable")
        );
        assert_eq!(
            state(one(&ok), one(&ImageSbom::Missing)).as_deref(),
            Some("no-sbom")
        );
        assert_eq!(state(one(&ok), one(&ImageSbom::NoPlatform(None))), None);
        let elsewhere = ImageSbom::NoPlatform(Some("linux/arm64".into()));
        assert_eq!(fallback(&one(&elsewhere)).as_deref(), Some("linux/arm64"));
        let mut partly = one(&elsewhere);
        partly.insert("linux/s390x".into(), Ok(ok.clone()));
        assert_eq!(fallback(&partly), None);
        assert_eq!(fallback(&one(&ImageSbom::NoPlatform(None))), None);
        let mut unreadable = one(&ok);
        unreadable.insert(
            "linux/arm64".into(),
            Err("registry rate limit reached".into()),
        );
        let mut both = one(&ImageSbom::Gone);
        both.insert("linux/arm64".into(), Ok(ok.clone()));
        let check = compare(
            ("sha256:a", &both),
            ("sha256:b", &unreadable),
            &AdvisoryListing::new(),
        )
        .unwrap();
        assert_eq!(check.state, "unreadable");
        assert_eq!(check.detail.as_deref(), Some("registry rate limit reached"));
        let ImageSbom::Found(mut partial) = ok.clone() else {
            unreachable!()
        };
        partial.sources_known = false;
        let partial = ImageSbom::Found(partial);
        assert_eq!(
            state(one(&ok), one(&partial)).as_deref(),
            Some("partial-sources")
        );
        let ImageSbom::Found(mut oracle) = ok.clone() else {
            unreachable!()
        };
        oracle.ecosystem = None;
        oracle.os = Some("ol 9".into());
        let oracle = ImageSbom::Found(oracle);
        let check = compare(
            ("sha256:a", &one(&oracle)),
            ("sha256:b", &one(&oracle)),
            &AdvisoryListing::new(),
        );
        assert_eq!(
            check.as_ref().map(|c| c.state.as_str()),
            Some("os-not-covered")
        );
        assert_eq!(check.unwrap().os.as_deref(), Some("ol 9"));
    }

    #[test]
    fn only_a_warning_marks_the_current_image() {
        let one = |s: ImageSbom| side(&[("linux/amd64", s)]);
        assert_eq!(image_state(&one(sbom(&[("openssl", "1")]))), None);
        assert_eq!(
            image_state(&one(ImageSbom::Missing)).map(|c| c.state),
            Some("no-sbom".to_owned())
        );
        assert_eq!(image_state(&one(ImageSbom::Gone)), None);
        assert_eq!(
            image_state(&Side::from([("linux/amd64".into(), Err("429".into()))])),
            None
        );
    }
}
