use crate::{Error, Exclusion, Result, Warning};
use patchula_report::{Dependency, PackageFile, Report, Status, Update};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// Empirical comparison ceiling, not Renovate's configured minimum release age.
// unocss 66.11.0 was still held at 72 h 17 min; effect 4.0.3 was proposed at
// about 84 h. See departure 23 for the black-box observations behind 96 h.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    PatchulaMissing,
    PatchulaExtra,
    FieldMismatch,
    DeliberateDeparture,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Difference {
    pub classification: Classification,
    pub original_classification: Classification,
    pub repository: String,
    pub package_file: Option<String>,
    pub manager: Option<String>,
    pub dependency: Option<String>,
    pub field: String,
    pub renovate: Value,
    pub patchula: Value,
    pub departure: Option<Departure>,
    /// Rate-limited lookup evidence is independent of the comparison class.
    pub rate_limited: bool,
    #[serde(skip)]
    target_version: Option<String>,
    #[serde(skip)]
    paired: Option<(Update, Update)>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Departure {
    pub rule: String,
    pub row: u32,
    pub link: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rules {
    schema_version: u32,
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rule {
    id: String,
    departure: u32,
    classification: Classification,
    managers: Vec<String>,
    field: String,
    predicate: Predicate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Predicate {
    DigestAdded,
    DigestPinOnly,
    CargoPre1Breaking,
    ReleaseAgeWindow,
    NpmExactMajor,
    FloatingPin,
    IndirectGo,
    ComposeTestFile,
    NpmExactRange,
    NpmRangePin,
    PatchAlternative,
    GoModulePath,
    CargoManifestNoop,
    DisabledUpdate,
    RejectedRelease,
    ObservedResolution,
    ReleaseAgeMissing,
    NpmSatisfyingPin,
    ReleaseAgeDigest,
    StableNodeChannel,
    DeclaredGoFloor,
    MiseBackendIdentity,
    ActionAliasPrecision,
    CargoCompatibleTarget,
}

impl Rules {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let rules: Self = serde_json::from_slice(bytes)?;
        if rules.schema_version != 1 {
            return Err(Error::Input("unsupported rules schemaVersion".into()));
        }
        let mut ids = BTreeSet::new();
        for rule in &rules.rules {
            if rule.id.is_empty()
                || !ids.insert(&rule.id)
                || rule.managers.is_empty()
                || !include_str!("../../docs/departures.md")
                    .lines()
                    .any(|line| line.starts_with(&format!("| {} |", rule.departure)))
                || !matches!(
                    (&rule.predicate, rule.classification, rule.field.as_str()),
                    (
                        Predicate::StableNodeChannel,
                        Classification::PatchulaExtra,
                        "updates"
                    ) | (
                        Predicate::StableNodeChannel,
                        Classification::FieldMismatch,
                        "updates.newValue" | "updates.newVersion"
                    ) | (
                        Predicate::DeclaredGoFloor,
                        Classification::FieldMismatch,
                        "current.version"
                    ) | (
                        Predicate::DeclaredGoFloor,
                        Classification::PatchulaExtra,
                        "updates"
                    ) | (
                        Predicate::MiseBackendIdentity,
                        Classification::FieldMismatch,
                        "package"
                    ) | (
                        Predicate::ActionAliasPrecision,
                        Classification::FieldMismatch,
                        "updates.newValue" | "updates.newVersion" | "updates.newDigest"
                    ) | (
                        Predicate::CargoCompatibleTarget,
                        Classification::PatchulaMissing | Classification::PatchulaExtra,
                        "updates"
                    ) | (
                        Predicate::DisabledUpdate
                            | Predicate::RejectedRelease
                            | Predicate::ReleaseAgeMissing
                            | Predicate::NpmSatisfyingPin,
                        Classification::PatchulaMissing,
                        "updates"
                    ) | (
                        Predicate::ObservedResolution,
                        Classification::FieldMismatch,
                        "current.version"
                    ) | (
                        Predicate::CargoManifestNoop,
                        Classification::PatchulaMissing,
                        "updates"
                    ) | (
                        Predicate::DigestAdded,
                        Classification::FieldMismatch,
                        "updates.newDigest"
                    ) | (
                        Predicate::ReleaseAgeDigest,
                        Classification::FieldMismatch,
                        "updates.newDigest"
                    ) | (
                        Predicate::DigestPinOnly,
                        Classification::PatchulaExtra,
                        "updates"
                    ) | (
                        Predicate::CargoPre1Breaking,
                        Classification::FieldMismatch,
                        "updates.updateType"
                    ) | (
                        Predicate::ReleaseAgeWindow,
                        Classification::FieldMismatch,
                        "updates.newValue" | "updates.newVersion"
                    ) | (
                        Predicate::NpmExactMajor | Predicate::NpmExactRange,
                        Classification::FieldMismatch,
                        "updates.newValue"
                    ) | (
                        Predicate::FloatingPin
                            | Predicate::IndirectGo
                            | Predicate::NpmRangePin
                            | Predicate::PatchAlternative,
                        Classification::PatchulaExtra,
                        "updates"
                    ) | (
                        Predicate::ComposeTestFile,
                        Classification::PatchulaExtra,
                        "dependency"
                    ) | (
                        Predicate::GoModulePath,
                        Classification::PatchulaMissing,
                        "updates"
                    )
                )
            {
                return Err(Error::Input(
                    "invalid departure rule, duplicate ID or missing docs/departures.md row".into(),
                ));
            }
        }
        Ok(rules)
    }

    fn classify(
        &self,
        difference: &mut Difference,
        dependency: Option<&Dependency>,
        patchula: &Dependency,
    ) {
        for rule in &self.rules {
            if difference.classification != rule.classification
                || difference.field != rule.field
                || !difference
                    .manager
                    .as_ref()
                    .is_some_and(|m| rule.managers.contains(m))
            {
                continue;
            }
            let matches = match rule.predicate {
                Predicate::StableNodeChannel => dependency.is_some_and(|dep| stable_node_channel(dep, patchula, difference)),
                Predicate::DeclaredGoFloor => dependency.is_some_and(|dep| declared_go_floor(dep, patchula, difference)),
                Predicate::MiseBackendIdentity => dependency.is_some_and(|dep| mise_backend_identity(dep, patchula)),
                Predicate::ActionAliasPrecision => dependency.is_some_and(|dep| action_alias_precision(dep, patchula, difference)),
                Predicate::CargoCompatibleTarget => dependency.is_some_and(|dep| cargo_compatible_target(dep, patchula, difference)),
                Predicate::ReleaseAgeDigest => dependency.is_some_and(|dep| {
                    dep.package == patchula.package && dep.current.value == patchula.current.value
                        && dep.current.digest == patchula.current.digest
                        && patchula.metadata.as_ref().is_some_and(|m| m.source == "github-tags")
                        && difference.paired.as_ref().is_some_and(|(left, right)| {
                            left.update_type == right.update_type
                                && matches!(right.update_type.as_str(), "major" | "minor" | "patch")
                                && left.new_value == right.new_value
                                && [&left.new_digest, &right.new_digest].iter().all(|d| d.as_ref().is_some_and(|s|
                                    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())))
                                && left.new_version.as_deref().and_then(parse_version)
                                    .zip(right.new_version.as_deref().and_then(parse_version))
                                    .is_some_and(|(a,b)| b.cmp_precedence(&a).is_gt())
                                && right.policy.as_ref().is_some_and(|p| p.wait_hours == 24)
                                && right.release_age.as_ref().is_some_and(|age|
                                    chrono::DateTime::parse_from_rfc3339(&age.published_at).ok()
                                        .zip(chrono::DateTime::parse_from_rfc3339(&age.evaluated_at).ok())
                                        .is_some_and(|(published,evaluated)| (chrono::Duration::hours(24)..chrono::Duration::hours(96))
                                            .contains(&(evaluated-published))))
                        })
                }),
                Predicate::NpmSatisfyingPin => dependency.is_some_and(|left| {
                    !is_peer(patchula)
                        &&
                    left.package == patchula.package && left.current.value == patchula.current.value
                        && patchula.metadata.as_ref().is_some_and(|m| m.source == "npm")
                        && serde_json::from_value::<Update>(difference.renovate.clone()).is_ok_and(|u| {
                            matches!(u.update_type.as_str(), "minor" | "patch") && u.new_digest.is_none()
                                && u.new_value == left.current.value
                                && left.current.value.as_deref().is_some_and(|range|
                                    simple_range_version(range).is_some()
                                    && semver::VersionReq::parse(range).is_ok_and(|r|
                                        u.new_version.as_deref().and_then(parse_version).is_some_and(|v| r.matches(&v))))
                                && patchula.updates.iter().any(|actual| actual.update_type == "pin"
                                    && actual.new_version == u.new_version && actual.new_value == actual.new_version
                                    && actual.new_digest.is_none()
                                    && actual.policy.as_ref().is_some_and(|p| p.versions.as_deref() == Some("exact")))
                        })
                }),
                Predicate::DisabledUpdate | Predicate::RejectedRelease | Predicate::ReleaseAgeMissing => dependency.is_some_and(|left| {
                    left.package == patchula.package && left.current.value == patchula.current.value
                        && patchula.metadata.as_ref().and_then(|m| m.lookup.as_ref()).is_some_and(|evidence| {
                            let evaluated = chrono::DateTime::parse_from_rfc3339(&evidence.evaluated_at).ok();
                            evaluated.is_some() && serde_json::from_value::<Update>(difference.renovate.clone()).is_ok_and(|u| {
                                match rule.predicate {
                                    Predicate::DisabledUpdate => evidence.disabled_updates.contains(&u.update_type),
                                    Predicate::RejectedRelease => {
                                        u.new_digest.is_none() && matches!(u.update_type.as_str(), "major" | "minor" | "patch")
                                            && u.new_version.as_deref().is_some_and(|target| evidence.rejected.iter().any(|v|
                                                v.version == target && matches!(v.reason.as_str(), "deprecated" | "yanked")))
                                            && matches!(patchula.metadata.as_ref().map(|m| m.source.as_str()), Some("npm" | "crates" | "pypi"))
                                    },
                                    Predicate::ReleaseAgeMissing => {
                                        u.new_digest.is_none() && matches!(u.update_type.as_str(), "major" | "minor" | "patch")
                                            && patchula.metadata.as_ref().is_some_and(|m| m.held_back.iter().any(|held|
                                                held.update_type == u.update_type && u.new_version.as_deref() == Some(&held.version)
                                                    && chrono::DateTime::parse_from_rfc3339(&held.published_at).ok()
                                                        .zip(chrono::DateTime::parse_from_rfc3339(&held.eligible_at).ok())
                                                        .zip(evaluated).is_some_and(|((published, eligible), evaluated)|
                                                            eligible - published == chrono::Duration::hours(24) && published <= evaluated && evaluated < eligible)))
                                    },
                                    _ => false,
                                }
                            })
                        })
                }),
                Predicate::ObservedResolution => dependency.is_some_and(|left| {
                    left.package == patchula.package && left.current.value == patchula.current.value
                        && left.current.digest == patchula.current.digest
                        && patchula.metadata.as_ref().and_then(|m| m.resolution.as_ref()).is_some_and(|resolution| {
                            patchula.current.version.as_deref() == Some(&resolution.version)
                                && left.current.version.as_deref().and_then(parse_version)
                                    .zip(parse_version(&resolution.version)).is_some_and(|(new, old)| new > old)
                                && match resolution.kind.as_str() {
                                    "lockfile" => patchula.manager == "npm" && patchula.metadata.as_ref().is_some_and(|m| m.source == "npm")
                                        && left.current.value.as_deref().is_some_and(|range| semver::VersionReq::parse(range).is_ok_and(|r|
                                            parse_version(&resolution.version).is_some_and(|v| r.matches(&v))
                                                && left.current.version.as_deref().and_then(parse_version).is_some_and(|v| r.matches(&v)))),
                                    "pinned-commit" => patchula.manager == "github-actions"
                                        && patchula.metadata.as_ref().is_some_and(|m| m.source == "github-tags")
                                        && patchula.current.digest.as_ref().is_some_and(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
                                        && left.current.value.as_deref().is_some_and(|alias| {
                                            let alias = alias.trim_start_matches('v');
                                            !alias.is_empty() && alias.split('.').count() < 3 && alias.split('.').all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                                                && resolution.version.trim_start_matches('v').starts_with(&format!("{alias}."))
                                                && left.current.version.as_deref().is_some_and(|v| v.trim_start_matches('v').starts_with(&format!("{alias}.")))
                                        }),
                                    _ => false,
                                }
                        })
                }),
                Predicate::CargoManifestNoop => dependency.is_some_and(|left| {
                    left.manager == "cargo" && left.package == patchula.package
                        && left.current.value == patchula.current.value
                        && left.metadata.as_ref().is_some_and(|m| m.source == "crate")
                        && patchula.metadata.as_ref().is_some_and(|m| m.source == "crates")
                        && serde_json::from_value::<Update>(difference.renovate.clone()).is_ok_and(|u| {
                            matches!(u.update_type.as_str(), "major" | "minor" | "patch")
                                && u.new_value == left.current.value && u.new_digest.is_none()
                                && left.current.value.as_deref().is_some_and(|range| {
                                    !range.starts_with('=') && semver::VersionReq::parse(range).is_ok_and(|r|
                                        u.new_version.as_deref().and_then(parse_version).is_some_and(|v|
                                            r.matches(&v) && v.pre.is_empty()))
                                })
                        })
                }),
                Predicate::GoModulePath => dependency.is_some_and(|left| {
                    left.manager == "go" && left.package == patchula.package
                        && left.current.value == patchula.current.value
                        && left.metadata.as_ref().is_some_and(|m| m.source == "go")
                        && patchula.metadata.as_ref().is_some_and(|m| m.source == "go")
                        && left.package.starts_with("github.com/")
                        && serde_json::from_value::<Update>(difference.renovate.clone()).is_ok_and(|u| {
                            u.update_type == "major" && u.new_value == u.new_version && u.new_digest.is_none()
                                && left.current.value.as_deref().and_then(parse_version)
                                    .zip(u.new_version.as_deref().and_then(parse_version))
                                    .is_some_and(|(a,b)| b.major >= 2 && b.major > a.major && b.pre.is_empty()
                                        && b.build.is_empty() && !left.package.ends_with(&format!("/v{}", b.major)))
                        })
                }),
                Predicate::NpmExactRange => difference.paired.as_ref().is_some_and(|(left, right)| {
                    if is_peer(patchula) { return false; }
                    let current = dependency.and_then(|d| d.current.value.as_deref())
                        .and_then(simple_range_version);
                    let target = right.new_version.as_deref().and_then(parse_version);
                    left.update_type == right.update_type
                        && matches!(right.update_type.as_str(), "minor" | "patch")
                        && left.new_version == right.new_version && right.new_value == right.new_version
                        && right.policy.as_ref().is_some_and(|p| p.versions.as_deref() == Some("exact"))
                        && left.new_value.as_deref().is_some_and(|range| simple_range_version(range).is_some()
                            && semver::VersionReq::parse(range).is_ok_and(|r| target.as_ref().is_some_and(|v| r.matches(v))))
                        && current.zip(target).is_some_and(|(old, target)| old.major == target.major
                            && target.cmp_precedence(&old).is_gt()
                            && if right.update_type == "minor" { target.minor > old.minor } else { target.minor == old.minor })
                }),
                Predicate::NpmRangePin => dependency.is_some_and(|left| {
                    !is_peer(patchula) &&
                    left.manager == "npm" && left.current.value == patchula.current.value
                        && left.updates.iter().all(|u| u.update_type != "pin")
                        && patchula.metadata.as_ref().is_some_and(|m| m.source == "npm")
                        && serde_json::from_value::<Update>(difference.patchula.clone()).is_ok_and(|u| {
                            u.update_type == "pin" && u.new_value == u.new_version && u.new_digest.is_none()
                                && u.policy.as_ref().is_some_and(|p| p.versions.as_deref() == Some("exact"))
                                && left.current.value.as_deref().is_some_and(|range| simple_range_version(range).is_some()
                                    && semver::VersionReq::parse(range).is_ok_and(|r| u.new_version.as_deref()
                                        .and_then(parse_version).is_some_and(|v| r.matches(&v))))
                        })
                }),
                Predicate::PatchAlternative => dependency.is_some_and(|left| {
                    let current = left.current.version.as_deref().and_then(parse_version);
                    serde_json::from_value::<Update>(difference.patchula.clone()).is_ok_and(|u| {
                        u.update_type == "patch" && u.new_value != left.current.value
                            && u.policy.is_some()
                            && current.zip(u.new_version.as_deref().and_then(parse_version)).is_some_and(|(old,target)| {
                                target.major == old.major && target.minor == old.minor
                                    && target.cmp_precedence(&old).is_gt()
                                    && left.updates.iter().any(|minor| {
                                        minor.update_type == "minor"
                                            && (patchula.updates.iter().any(|actual| actual.update_type == "minor"
                                                && ((actual.new_version == minor.new_version
                                                    && actual.new_value == minor.new_value)
                                                    || release_age_difference(minor, actual, patchula))
                                                && (actual.new_digest == minor.new_digest
                                                    || left.metadata.as_ref().is_some_and(|m| matches!(m.source.as_str(), "npm" | "helm"))))
                                                || held_target(minor, patchula))
                                            && minor.new_version.as_deref().and_then(parse_version).is_some_and(|v|
                                                v.major == target.major && v.minor > target.minor)
                                    })
                            })
                    })
                }),
                Predicate::FloatingPin => dependency.is_some_and(|left| {
                    left.package == patchula.package
                        && left.current.digest == patchula.current.digest
                        && left.current.value.as_deref() == Some("latest")
                        && left.updates.is_empty()
                        && patchula.current.value == left.current.value
                        && (matches!(patchula.manager.as_str(), "mise" | "npm")
                            || (patchula.manager == "github-actions" && patchula.metadata.as_ref().is_some_and(|m|
                                matches!(m.source.as_str(), "github-releases" | "npm") && m.kinds.iter().any(|k| k == "tool"))))
                        && serde_json::from_value::<Update>(difference.patchula.clone()).is_ok_and(|u| {
                            u.update_type == "pin" && u.new_digest.is_none()
                                && u.policy.as_ref().is_some_and(|p| p.versions.as_deref() == Some("exact"))
                                && u.new_version.as_deref().and_then(parse_version).is_some()
                                && u.new_value.as_deref().and_then(parse_version)
                                    == u.new_version.as_deref().and_then(parse_version)
                        })
                }),
                Predicate::IndirectGo => dependency.is_some_and(|left| {
                    left.manager == "go" && left.updates.is_empty()
                        && left.metadata.as_ref().is_some_and(|m| m.source == "go" && m.kinds.iter().any(|k| k == "indirect"))
                        && left.current.value == patchula.current.value
                        && serde_json::from_value::<Update>(difference.patchula.clone()).is_ok_and(|u| {
                            u.new_digest.is_none() && u.new_value == u.new_version
                                && matches!(u.update_type.as_str(), "major" | "minor" | "patch")
                                && left.current.value.as_deref().and_then(parse_version)
                                    .zip(u.new_version.as_deref().and_then(parse_version))
                                    .is_some_and(|(a,b)| b.cmp_precedence(&a).is_gt())
                        })
                }),
                Predicate::ComposeTestFile => dependency.is_none()
                    && patchula.manager == "compose"
                    && patchula.metadata.as_ref().is_some_and(|m| m.source == "docker"
                        && m.kinds.iter().any(|k| k == "container-image"))
                    && difference.package_file.as_deref().is_some_and(|path| {
                        path.starts_with("test/")
                            && path.rsplit('/').next().is_some_and(|leaf| leaf.starts_with("docker-compose.")
                                && matches!(leaf.rsplit('.').next(), Some("yml" | "yaml")))
                    }),
                Predicate::ReleaseAgeWindow => difference.paired.as_ref().is_some_and(|(left, right)| {
                    let versions = left.new_version.as_deref().and_then(parse_version)
                        .zip(right.new_version.as_deref().and_then(parse_version));
                    let age = right.release_age.as_ref().and_then(|age| {
                        let published = chrono::DateTime::parse_from_rfc3339(&age.published_at).ok()?;
                        let evaluated = dependency.and_then(|dep| dep.metadata.as_ref())
                            .and_then(|m| m.lookup.as_ref())
                            .and_then(|e| chrono::DateTime::parse_from_rfc3339(&e.evaluated_at).ok())?;
                        Some(evaluated.signed_duration_since(published))
                    });
                    let held_back = patchula.metadata.as_ref().is_some_and(|metadata| metadata.held_back.iter().any(|held| {
                        let times = chrono::DateTime::parse_from_rfc3339(&held.published_at).ok()
                            .zip(chrono::DateTime::parse_from_rfc3339(&held.eligible_at).ok())
                            .zip(right.release_age.as_ref().and_then(|age| chrono::DateTime::parse_from_rfc3339(&age.evaluated_at).ok()));
                        left.new_version.as_deref().is_some_and(|v| v == held.version || held.versions.iter().any(|blocked| blocked == v))
                            && held.update_type == left.update_type
                            && times.is_some_and(|((published, eligible), evaluated)| {
                                eligible - published == chrono::Duration::hours(24)
                                    && published <= evaluated && evaluated < eligible
                            })
                    }));
                    right.policy.as_ref().is_some_and(|p| p.wait_hours == 24)
                        && versions.is_some_and(|(a,b)| {
                            (b.cmp_precedence(&a).is_gt()
                                && age.is_some_and(|age| age >= chrono::Duration::hours(24) && age < chrono::Duration::hours(96)))
                                || (a.cmp_precedence(&b).is_gt() && held_back)
                        })
                        && left.update_type == right.update_type
                        && matches!(right.update_type.as_str(), "major" | "minor" | "patch")
                        // Range-policy differences need their own explanation.
                        && if difference.field == "updates.newValue" {
                            left.new_value == left.new_version && right.new_value == right.new_version
                        } else { true }
                }),
                Predicate::NpmExactMajor => difference.paired.as_ref().is_some_and(|(left, right)| {
                    if is_peer(patchula) { return false; }
                    let current = dependency.and_then(|d| d.current.value.as_deref());
                    let target = right.new_version.as_deref().and_then(parse_version);
                    let range = left.new_value.as_deref();
                    let target_range = range.filter(|range| !range.contains(" || "));
                    let lower = target_range.and_then(simple_range_version);
                    let baseline = current.and_then(simple_range_version);
                    left.update_type == "major" && right.update_type == "major"
                        && (left.new_version == right.new_version || release_age_difference(left, right, patchula))
                        && right.new_value == right.new_version
                        && right.policy.as_ref().is_some_and(|p| p.versions.as_deref() == Some("exact"))
                        && baseline.zip(lower).zip(target).is_some_and(|((old,lower), target)| {
                            target.major > old.major && lower.major == target.major
                                && lower.cmp_precedence(&target).is_le()
                                && target_range.is_some_and(|range| semver::VersionReq::parse(range).is_ok_and(|r| r.matches(&target)))
                                && lower.pre.is_empty() && target.pre.is_empty()
                        })
                }),
                Predicate::CargoPre1Breaking => {
                    difference.manager.as_deref() == Some("cargo")
                        && matches!(difference.renovate.as_str(), Some("minor" | "patch"))
                        && difference.patchula == "major"
                        && dependency
                            .and_then(|d| d.current.version.as_deref())
                            .and_then(numeric_version)
                            .zip(
                                difference
                                    .target_version
                                    .as_deref()
                                    .and_then(numeric_version),
                            )
                            .is_some_and(|(a, b)| {
                                a.0 == 0
                                    && b.0 == 0
                                    && b > a
                                    && (a.1 != b.1 || (a.1 == 0 && a.2 != b.2))
                            })
                }
                Predicate::DigestAdded => {
                    difference.renovate.is_null()
                        && difference
                            .patchula
                            .as_str()
                            .is_some_and(|digest| !digest.is_empty())
                }
                Predicate::DigestPinOnly => dependency.is_some_and(|dep| {
                    serde_json::from_value::<Update>(difference.patchula.clone()).is_ok_and(
                        |update| {
                            update.update_type == "pin"
                                && dep.current.digest.is_none()
                                && update.new_digest.as_ref().is_some_and(|s| !s.is_empty())
                                && update
                                    .new_value
                                    .as_ref()
                                    .is_none_or(|v| Some(v) == dep.current.value.as_ref())
                                && update
                                    .new_version
                                    .as_ref()
                                    .is_none_or(|v| Some(v) == dep.current.version.as_ref())
                                && update.group.is_none()
                        },
                    )
                }),
            };
            if matches {
                difference.classification = Classification::DeliberateDeparture;
                difference.departure = Some(Departure {
                    rule: rule.id.clone(),
                    row: rule.departure,
                    link: format!("docs/departures.md#departure-{}", rule.departure),
                });
                return;
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Triage {
    pub schema_version: u32,
    pub renovate: Report,
    pub patchula: Report,
    pub differences: Vec<Difference>,
    pub warnings: Vec<Warning>,
    pub exclusions: Vec<Exclusion>,
    pub normalizations: BTreeMap<String, usize>,
    pub no_baseline: Vec<NoBaseline>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoBaseline {
    pub repository: String,
    pub reason: String,
}

fn label(class: Classification) -> &'static str {
    match class {
        Classification::PatchulaMissing => "patchula-missing",
        Classification::PatchulaExtra => "patchula-extra",
        Classification::FieldMismatch => "field-mismatch",
        Classification::DeliberateDeparture => "deliberate-departure",
    }
}

fn cell(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "&#124;")
        .replace('`', "&#96;")
        .replace('\n', "<br>")
        .replace('\r', "")
}

impl Triage {
    pub fn markdown(&self) -> String {
        let mut out = String::from("# Dependency comparison triage\n\n");
        for class in [
            Classification::PatchulaMissing,
            Classification::PatchulaExtra,
            Classification::FieldMismatch,
            Classification::DeliberateDeparture,
        ] {
            let count = self
                .differences
                .iter()
                .filter(|d| d.classification == class)
                .count();
            out.push_str(&format!("- {}: {count}\n", label(class)));
        }
        out.push_str("\nRepository coverage and normalized inputs are in the accompanying JSON. Departure links are relative to the repository root.\n\n");
        out.push_str(&format!(
            "- Rate-limited digest differences (included above): {}\n\n",
            self.differences.iter().filter(|d| d.rate_limited).count()
        ));
        for missing in &self.no_baseline {
            out.push_str(&format!(
                "- No baseline: {} — {}\n",
                cell(&missing.repository),
                cell(&missing.reason)
            ));
        }
        if !self.no_baseline.is_empty() {
            out.push('\n');
        }
        for (rule, count) in &self.normalizations {
            out.push_str(&format!("- Normalization {}: {count}\n", cell(rule)));
        }
        if !self.normalizations.is_empty() {
            out.push('\n');
        }
        if !self.exclusions.is_empty() {
            out.push_str("Excluded Renovate output (outside patchula's manager scope):\n\n");
            for e in &self.exclusions {
                out.push_str(&format!(
                    "- {} / {}: {} files, {} dependencies, {} updates ([departure {}]({})).\n",
                    cell(&e.repository),
                    cell(&e.manager),
                    e.package_files,
                    e.dependencies,
                    e.updates,
                    e.departure,
                    e.link
                ));
            }
            out.push('\n');
        }
        out.push_str(&format!("Parser warnings: {}\n\n", self.warnings.len()));
        for w in &self.warnings {
            out.push_str(&format!(
                "- Line {} / {} / {}: {}\n",
                w.line,
                cell(w.repository.as_deref().unwrap_or("")),
                cell(&w.field),
                cell(&w.message)
            ));
        }
        if !self.warnings.is_empty() {
            out.push('\n');
        }
        if self.differences.is_empty() {
            out.push_str("No differences.\n");
            return out;
        }
        out.push_str("| Classification | Repository / file / manager / dependency | Field | Renovate | patchula | Departure |\n|---|---|---|---|---|---|\n");
        for d in &self.differences {
            let location = [
                &d.repository,
                d.package_file.as_deref().unwrap_or(""),
                d.manager.as_deref().unwrap_or(""),
                d.dependency.as_deref().unwrap_or(""),
            ]
            .join(" / ");
            let departure = d
                .departure
                .as_ref()
                .map(|p| format!("[row {}]({}) ({})", p.row, p.link, cell(&p.rule)))
                .unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                label(d.classification),
                cell(&location),
                cell(&d.field),
                cell(&d.renovate.to_string()),
                cell(&d.patchula.to_string()),
                departure
            ));
        }
        out
    }
}

#[derive(Clone)]
struct Location<'a> {
    repository: &'a str,
    path: Option<&'a str>,
    dep: Option<&'a Dependency>,
}

fn difference(
    location: &Location<'_>,
    field: &str,
    a: Value,
    b: Value,
    class: Classification,
) -> Difference {
    Difference {
        classification: class,
        original_classification: class,
        repository: location.repository.into(),
        package_file: location.path.map(str::to_owned),
        manager: location.dep.map(|d| d.manager.clone()),
        dependency: location.dep.map(|d| d.name.clone()),
        field: field.into(),
        renovate: a,
        patchula: b,
        departure: None,
        rate_limited: false,
        target_version: None,
        paired: None,
    }
}

fn mismatch(out: &mut Vec<Difference>, location: &Location<'_>, field: &str, a: Value, b: Value) {
    if a != b {
        out.push(difference(
            location,
            field,
            a,
            b,
            Classification::FieldMismatch,
        ));
    }
}

fn updates(
    out: &mut Vec<Difference>,
    location: &Location<'_>,
    a: &[Update],
    b: &[Update],
    checksum_source: Option<&str>,
    notes: &mut BTreeMap<String, usize>,
) {
    let normalize = |u: &Update| {
        let mut u = u.clone();
        if checksum_source.is_some() {
            u.new_digest = None;
        }
        u
    };
    let a: Vec<_> = a.iter().map(normalize).collect();
    let b: Vec<_> = b.iter().map(normalize).collect();
    if let Some(source) = checksum_source {
        *notes
            .entry(format!("{source}-newDigest-ignored"))
            .or_default() += a.len();
    }
    let mut b = b;
    // Digest updates omit a redundant newVersion in the upstream output. It is
    // unknown metadata, and only comparable when the baseline actually has it.
    for right in &mut b {
        if matches!(right.update_type.as_str(), "pin" | "digest")
            && right.new_digest.is_some()
            && a.iter().any(|left| {
                left.update_type == right.update_type
                    && left.new_value == right.new_value
                    && left.new_digest == right.new_digest
                    && left.new_version.is_none()
            })
            && right.new_version.take().is_some()
        {
            *notes
                .entry("digest-update-version-unavailable".into())
                .or_default() += 1;
        }
    }
    *notes
        .entry("groups-unavailable-skipped".into())
        .or_default() += a.iter().filter(|u| u.group.is_none()).count();
    let mut a: Vec<_> = a.iter().collect();
    let mut b: Vec<_> = b.iter().collect();
    let same_target = |left: &Update, right: &Update| {
        left.new_value == right.new_value
            && left.new_version == right.new_version
            && left.new_digest == right.new_digest
    };
    // Policy/age are evidence, not comparison fields. Missing upstream group
    // metadata is unknown, rather than an assertion of ungrouped updates.
    for left in a.clone() {
        if let Some(index) = b.iter().position(|right| {
            same_target(left, right)
                && left.update_type == right.update_type
                && left
                    .group
                    .as_ref()
                    .is_none_or(|g| right.group.as_ref() == Some(g))
        }) {
            a.retain(|u| u != &left);
            b.remove(index);
        }
    }
    // A unique identical target can expose a taxonomy mismatch independently
    // of the update type used to pair changed targets below.
    for left in a.clone() {
        if a.iter().filter(|u| same_target(left, u)).count() != 1 {
            continue;
        }
        let candidates: Vec<_> = b.iter().filter(|u| same_target(left, u)).copied().collect();
        if let [right] = candidates.as_slice() {
            let start = out.len();
            mismatch(
                out,
                location,
                "updates.updateType",
                json!(left.update_type),
                json!(right.update_type),
            );
            for difference in &mut out[start..] {
                difference.target_version = right.new_version.clone();
            }
            if left.group.is_some() {
                mismatch(
                    out,
                    location,
                    "updates.group",
                    json!(left.group),
                    json!(right.group),
                );
            }
            a.retain(|u| u != &left);
            b.retain(|u| u != right);
        }
    }
    // Pair only when a type identifies one remaining proposal on each side.
    // Multiple proposals of one type are extra/missing, never position-paired.
    let types: BTreeSet<_> = a.iter().chain(&b).map(|u| u.update_type.clone()).collect();
    for kind in types {
        let left: Vec<_> = a
            .iter()
            .filter(|u| u.update_type == kind)
            .copied()
            .collect();
        let right: Vec<_> = b
            .iter()
            .filter(|u| u.update_type == kind)
            .copied()
            .collect();
        if let ([left], [right]) = (left.as_slice(), right.as_slice()) {
            let start = out.len();
            for (field, av, bv) in [
                (
                    "updates.newValue",
                    json!(left.new_value),
                    json!(right.new_value),
                ),
                (
                    "updates.newVersion",
                    json!(left.new_version),
                    json!(right.new_version),
                ),
                (
                    "updates.newDigest",
                    json!(left.new_digest),
                    json!(right.new_digest),
                ),
                ("updates.group", json!(left.group), json!(right.group)),
            ] {
                if field == "updates.group" && left.group.is_none() {
                    continue;
                }
                mismatch(out, location, field, av, bv);
            }
            // Attach the exact pair as evidence for narrow departure rules.
            for d in &mut out[start..] {
                d.paired = Some(((*left).clone(), (*right).clone()));
            }
            a.retain(|u| u != left);
            b.retain(|u| u != right);
        }
    }
    for u in a {
        out.push(difference(
            location,
            "updates",
            json!(u),
            Value::Null,
            Classification::PatchulaMissing,
        ));
    }
    for u in b {
        out.push(difference(
            location,
            "updates",
            Value::Null,
            json!(u),
            Classification::PatchulaExtra,
        ));
    }
}

// Emit dependency differences even when the containing repository/file is
// absent. Keep coverage differences for empty files and repositories too.
fn missing_file(
    out: &mut Vec<Difference>,
    location: &Location<'_>,
    file: &PackageFile,
    class: Classification,
    rules: &Rules,
) {
    let location = Location {
        path: Some(&file.path),
        ..location.clone()
    };
    let values = |value| {
        if class == Classification::PatchulaMissing {
            (value, Value::Null)
        } else {
            (Value::Null, value)
        }
    };
    for dep in &file.dependencies {
        let location = Location {
            dep: Some(dep),
            ..location.clone()
        };
        let (a, b) = values(json!(dep));
        out.push(difference(&location, "dependency", a, b, class));
        if class == Classification::PatchulaExtra {
            rules.classify(out.last_mut().expect("just inserted"), None, dep);
        }
    }
}

pub fn compare(renovate: &Report, patchula: &Report, rules: &Rules) -> Result<Triage> {
    renovate.validate()?;
    patchula.validate()?;
    for repo in &patchula.repositories {
        if repo.status != Status::Complete {
            return Err(Error::Input(format!(
                "{} is {:?}; only complete repositories can be compared",
                repo.repository, repo.status
            )));
        }
    }
    for repo in &renovate.repositories {
        if repo.status == Status::Error {
            return Err(Error::Input(format!("{} baseline failed", repo.repository)));
        }
    }
    let mut renovate = renovate.clone();
    let mut patchula = patchula.clone();
    let mut normalizations = BTreeMap::new();
    let exclusions = crate::normalize::reports(&mut renovate, &mut patchula, &mut normalizations);
    let a: BTreeMap<_, _> = renovate
        .repositories
        .iter()
        .map(|r| (&r.repository, r))
        .collect();
    let b: BTreeMap<_, _> = patchula
        .repositories
        .iter()
        .map(|r| (&r.repository, r))
        .collect();
    let mut differences = Vec::new();
    let mut no_baseline = Vec::new();
    let repos: BTreeSet<_> = a.keys().chain(b.keys()).copied().collect();
    for repo in repos {
        let location = Location {
            repository: repo,
            path: None,
            dep: None,
        };
        if b.contains_key(repo) && a.get(repo).is_none_or(|r| r.status == Status::Skipped) {
            no_baseline.push(NoBaseline {
                repository: repo.clone(),
                reason: a
                    .get(repo)
                    .and_then(|r| r.reason.clone())
                    .unwrap_or_else(|| "no Renovate lookup snapshot in supplied output".into()),
            });
            continue;
        }
        let (Some(left), Some(right)) = (a.get(repo), b.get(repo)) else {
            let class = if a.contains_key(repo) {
                Classification::PatchulaMissing
            } else {
                Classification::PatchulaExtra
            };
            let present = a
                .get(repo)
                .or_else(|| b.get(repo))
                .expect("repository exists");
            if present
                .package_files
                .iter()
                .all(|f| f.dependencies.is_empty())
            {
                differences.push(difference(
                    &location,
                    "repository",
                    json!(a.get(repo)),
                    json!(b.get(repo)),
                    class,
                ));
            } else {
                for file in &present.package_files {
                    missing_file(&mut differences, &location, file, class, rules);
                }
            }
            continue;
        };
        let a: BTreeMap<_, _> = left.package_files.iter().map(|f| (&f.path, f)).collect();
        let b: BTreeMap<_, _> = right.package_files.iter().map(|f| (&f.path, f)).collect();
        for path in a.keys().chain(b.keys()).copied().collect::<BTreeSet<_>>() {
            let location = Location {
                path: Some(path),
                ..location.clone()
            };
            let (Some(left), Some(right)) = (a.get(path), b.get(path)) else {
                let class = if a.contains_key(path) {
                    Classification::PatchulaMissing
                } else {
                    Classification::PatchulaExtra
                };
                if a.get(path)
                    .or_else(|| b.get(path))
                    .expect("file exists")
                    .dependencies
                    .is_empty()
                {
                    *normalizations
                        .entry("empty-package-file-ignored".into())
                        .or_default() += 1;
                }
                missing_file(
                    &mut differences,
                    &location,
                    a.get(path).or_else(|| b.get(path)).expect("file exists"),
                    class,
                    rules,
                );
                continue;
            };
            let a: BTreeMap<_, _> = left
                .dependencies
                .iter()
                .map(|d| ((&d.manager, &d.name, &d.occurrence), d))
                .collect();
            let b: BTreeMap<_, _> = right
                .dependencies
                .iter()
                .map(|d| ((&d.manager, &d.name, &d.occurrence), d))
                .collect();
            for key in a.keys().chain(b.keys()).copied().collect::<BTreeSet<_>>() {
                let dep = a
                    .get(&key)
                    .or_else(|| b.get(&key))
                    .copied()
                    .expect("key exists");
                let location = Location {
                    dep: Some(dep),
                    ..location.clone()
                };
                let (Some(left), Some(right)) = (a.get(&key), b.get(&key)) else {
                    let class = if a.contains_key(&key) {
                        Classification::PatchulaMissing
                    } else {
                        Classification::PatchulaExtra
                    };
                    differences.push(difference(
                        &location,
                        "dependency",
                        json!(a.get(&key)),
                        json!(b.get(&key)),
                        class,
                    ));
                    if class == Classification::PatchulaExtra {
                        rules.classify(differences.last_mut().expect("just inserted"), None, dep);
                    }
                    continue;
                };
                let start = differences.len();
                for (field, av, bv) in [
                    ("package", json!(left.package), json!(right.package)),
                    (
                        "current.value",
                        json!(left.current.value),
                        json!(right.current.value),
                    ),
                    (
                        "current.version",
                        json!(left.current.version),
                        json!(right.current.version),
                    ),
                    (
                        "current.digest",
                        json!(left.current.digest),
                        json!(right.current.digest),
                    ),
                ] {
                    if field == "current.version" && av.is_null() {
                        if !bv.is_null() {
                            *normalizations
                                .entry("current-version-unavailable".into())
                                .or_default() += 1;
                        }
                        continue;
                    }
                    mismatch(&mut differences, &location, field, av, bv);
                }
                let source = left
                    .metadata
                    .as_ref()
                    .or(right.metadata.as_ref())
                    .map(|m| m.source.as_str());
                let checksum_source = match source {
                    Some("npm") => Some("npm"),
                    Some("helm") => Some("helm"),
                    None if matches!(left.manager.as_str(), "npm" | "bun") => Some("npm"),
                    _ => None,
                };
                updates(
                    &mut differences,
                    &location,
                    &left.updates,
                    &right.updates,
                    checksum_source,
                    &mut normalizations,
                );
                for d in &mut differences[start..] {
                    rules.classify(d, Some(left), right);
                }
            }
        }
    }
    for d in &mut differences {
        if let Some(repo) = patchula
            .repositories
            .iter()
            .find(|r| r.repository == d.repository)
            && let Some(dep) = repo
                .package_files
                .iter()
                .find(|f| Some(&f.path) == d.package_file.as_ref())
                .and_then(|f| {
                    f.dependencies.iter().find(|dep| {
                        Some(&dep.name) == d.dependency.as_ref()
                            && Some(&dep.manager) == d.manager.as_ref()
                    })
                })
        {
            let digest_difference =
                (d.field == "updates.newDigest" && d.patchula.is_null() && !d.renovate.is_null())
                    || (d.classification == Classification::PatchulaMissing
                        && d.field == "updates"
                        && serde_json::from_value::<Update>(d.renovate.clone()).is_ok_and(|u| {
                            matches!(u.update_type.as_str(), "digest" | "pin")
                                && u.new_digest.is_some()
                                && u.new_value == dep.current.value
                        }));
            let reference = if d.field == "updates.newDigest" {
                d.paired
                    .as_ref()
                    .and_then(|(_, right)| right.new_value.as_ref().or(right.new_version.as_ref()))
            } else {
                dep.current.value.as_ref()
            };
            let rate_warning = reference.is_some_and(|reference| {
                repo.warnings.iter().any(|w| {
                    ["registry rate limit reached", "registry HTTP 429"]
                        .iter()
                        .any(|condition| {
                            w.starts_with(&format!("{}@{reference}: {condition}", dep.name))
                                || Some(reference) == dep.current.value.as_ref()
                                    && w.starts_with(&format!("{}: {condition}", dep.name))
                        })
                })
            });
            d.rate_limited = digest_difference
                && dep.metadata.as_ref().is_some_and(|m| {
                    matches!(m.source.as_str(), "docker" | "oci")
                        && (rate_warning
                            || reference.is_some_and(|tag| {
                                m.stale
                                    .contains_key(&format!("digest:{}", tag.replace('+', "_")))
                            }))
                });
        }
    }
    normalizations.retain(|_, count| *count > 0);
    Ok(Triage {
        schema_version: 1,
        renovate,
        patchula,
        differences,
        warnings: Vec::new(),
        exclusions,
        normalizations,
        no_baseline,
    })
}
fn parse_version(value: &str) -> Option<semver::Version> {
    semver::Version::parse(value.trim_start_matches('v')).ok()
}

fn held_target(update: &Update, dependency: &Dependency) -> bool {
    dependency.metadata.as_ref().is_some_and(|m| {
        let evaluated = m
            .lookup
            .as_ref()
            .and_then(|e| chrono::DateTime::parse_from_rfc3339(&e.evaluated_at).ok());
        m.held_back.iter().any(|held| {
            held.update_type == update.update_type
                && update
                    .new_version
                    .as_ref()
                    .is_some_and(|v| &held.version == v || held.versions.contains(v))
                && chrono::DateTime::parse_from_rfc3339(&held.published_at)
                    .ok()
                    .zip(chrono::DateTime::parse_from_rfc3339(&held.eligible_at).ok())
                    .zip(evaluated)
                    .is_some_and(|((published, eligible), evaluated)| {
                        eligible - published == chrono::Duration::hours(24)
                            && published <= evaluated
                            && evaluated < eligible
                    })
        })
    })
}

fn release_age_difference(left: &Update, right: &Update, dependency: &Dependency) -> bool {
    if left.update_type != right.update_type
        || !right.policy.as_ref().is_some_and(|p| p.wait_hours == 24)
    {
        return false;
    }
    let Some((a, b)) = left
        .new_version
        .as_deref()
        .and_then(parse_version)
        .zip(right.new_version.as_deref().and_then(parse_version))
    else {
        return false;
    };
    (a.cmp_precedence(&b).is_gt() && held_target(left, dependency))
        || (b.cmp_precedence(&a).is_gt()
            && right.release_age.as_ref().is_some_and(|age| {
                chrono::DateTime::parse_from_rfc3339(&age.published_at)
                    .ok()
                    .zip(chrono::DateTime::parse_from_rfc3339(&age.evaluated_at).ok())
                    .is_some_and(|(published, evaluated)| {
                        (chrono::Duration::hours(24)..chrono::Duration::hours(96))
                            .contains(&(evaluated - published))
                    })
            }))
}

fn simple_range_version(value: &str) -> Option<semver::Version> {
    let value = value
        .strip_prefix('^')
        .or_else(|| value.strip_prefix('~'))?;
    let parts: Vec<_> = value.split('.').collect();
    if !(1..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    parse_version(&format!(
        "{}.{}.{}",
        parts[0],
        parts.get(1).unwrap_or(&"0"),
        parts.get(2).unwrap_or(&"0")
    ))
}

fn is_peer(dep: &Dependency) -> bool {
    dep.metadata
        .as_ref()
        .is_some_and(|m| m.kinds.iter().any(|k| k == "peer"))
}

fn same_declaration(left: &Dependency, right: &Dependency) -> bool {
    left.package == right.package
        && left.current.value == right.current.value
        && left.current.digest == right.current.digest
}

fn stable_node_channel(left: &Dependency, right: &Dependency, difference: &Difference) -> bool {
    if !same_declaration(left, right) {
        return false;
    }
    let Some(m) = &right.metadata else {
        return false;
    };
    let node = (right.manager == "mise"
        && right.name == "node"
        && right.package == "node"
        && m.source == "mise"
        && m.kinds == ["tool"]
        && left
            .metadata
            .as_ref()
            .is_some_and(|m| m.source == "node-version"))
        || (right.manager == "github-actions"
            && right.name == "node"
            && right.package == "actions/node-versions"
            && m.source == "github-releases"
            && m.kinds == ["tool"]
            && left
                .metadata
                .as_ref()
                .is_some_and(|m| m.source == "github-releases"))
        || (right.manager == "npm"
            && right.name == "@types/node"
            && right.package == "@types/node"
            && m.source == "npm"
            && !is_peer(right)
            && left.metadata.as_ref().is_some_and(|m| m.source == "npm"));
    if !node {
        return false;
    }
    let pair = difference.paired.clone().or_else(|| {
        (difference.original_classification == Classification::PatchulaExtra
            && left
                .updates
                .iter()
                .all(|u| u.update_type != "major" && u.new_digest.is_none()))
        .then(|| serde_json::from_value::<Update>(difference.patchula.clone()).ok())
        .flatten()
        .map(|u| {
            let mut current = u.clone();
            current.new_version = left.current.version.clone();
            (current, u)
        })
    });
    pair.is_some_and(|(a, b)| {
        b.update_type == "major"
            && b.new_digest.is_none()
            && a.new_digest.is_none()
            && b.policy
                .as_ref()
                .is_some_and(|p| p.channel.as_deref().is_none_or(|c| c == "stable"))
            && a.new_version
                .as_deref()
                .and_then(parse_version)
                .zip(b.new_version.as_deref().and_then(parse_version))
                .is_some_and(|(a, b)| a.pre.is_empty() && b.pre.is_empty() && b.major > a.major)
            && b.new_value
                .as_deref()
                .zip(b.new_version.as_deref())
                .is_some_and(|(value, target)| {
                    let components = value.trim_start_matches('v').split('.').collect::<Vec<_>>();
                    !components.is_empty()
                        && components.len() <= 3
                        && components
                            .iter()
                            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                        && target
                            .trim_start_matches('v')
                            .strip_prefix(value.trim_start_matches('v'))
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
                })
    })
}

fn declared_go_floor(left: &Dependency, right: &Dependency, difference: &Difference) -> bool {
    if !same_declaration(left, right)
        || right.manager != "go"
        || right.name != "go"
        || right.package != "go"
        || !left.updates.is_empty()
    {
        return false;
    }
    let Some(m) = &right.metadata else {
        return false;
    };
    if m.source != "mise"
        || m.kinds != ["go-directive"]
        || !m.resolution.as_ref().is_some_and(|r| {
            r.kind == "declared"
                && Some(&r.version) == right.current.value.as_ref()
                && Some(&r.version) == right.current.version.as_ref()
        })
        || !left
            .metadata
            .as_ref()
            .is_some_and(|m| m.source == "golang-version" && m.kinds == ["golang"])
    {
        return false;
    }
    let Some((floor, compiler)) = right
        .current
        .version
        .as_deref()
        .and_then(|v| numeric_version(v).or_else(|| numeric_version(&format!("{v}.0"))))
        .zip(left.current.version.as_deref().and_then(numeric_version))
    else {
        return false;
    };
    if compiler <= floor {
        return false;
    }
    if difference.field == "current.version" {
        return true;
    }
    serde_json::from_value::<Update>(difference.patchula.clone()).is_ok_and(|u| {
        matches!(u.update_type.as_str(), "minor" | "patch")
            && u.new_value == u.new_version
            && u.new_digest.is_none()
            && u.policy
                .as_ref()
                .is_some_and(|p| p.versions.as_deref() == Some("exact"))
            && u.new_version
                .as_deref()
                .and_then(numeric_version)
                .is_some_and(|v| v > floor && v <= compiler)
    })
}

fn mise_backend_identity(left: &Dependency, right: &Dependency) -> bool {
    right.manager == "mise"
        && left.current == right.current
        && left.current.value.as_deref() == Some("latest")
        && left.updates.is_empty()
        && right.updates.is_empty()
        && left
            .metadata
            .as_ref()
            .is_some_and(|m| m.source == "github-releases" && m.kinds == ["tools"])
        && right.metadata.as_ref().is_some_and(|m| {
            m.kinds == ["tool"]
                && match right.name.as_str() {
                    "prettier" => {
                        left.package == "prettier/prettier"
                            && right.package == "prettier"
                            && m.source == "npm"
                    }
                    "bitwarden-secrets-manager" => {
                        left.package == "bitwarden/sdk"
                            && right.package == "bitwarden/sdk-sm"
                            && m.source == "github-releases"
                    }
                    _ => false,
                }
        })
}

fn action_alias_precision(left: &Dependency, right: &Dependency, difference: &Difference) -> bool {
    same_declaration(left, right)
        && right.manager == "github-actions"
        && right.metadata.as_ref().is_some_and(|m| {
            m.source == "github-tags"
                && m.kinds
                    .iter()
                    .any(|k| matches!(k.as_str(), "action" | "reusable-workflow"))
        })
        && (floating_action_target(left, right, difference)
            || difference.paired.as_ref().is_some_and(|(a, b)| {
                a.update_type == b.update_type
                    && matches!(b.update_type.as_str(), "pin" | "digest")
                    && a.new_version == b.new_version
                    && a.new_value == b.new_version
                    && a.new_digest
                        .as_ref()
                        .is_none_or(|d| Some(d) == b.new_digest.as_ref())
                    && b.new_digest
                        .as_ref()
                        .is_some_and(|d| d.len() == 40 && d.bytes().all(|b| b.is_ascii_hexdigit()))
                    && b.new_value == right.current.value
                    && b.new_value
                        .as_deref()
                        .zip(b.new_version.as_deref())
                        .is_some_and(|(alias, full)| {
                            let alias = alias.trim_start_matches('v');
                            let parts = alias.split('.').collect::<Vec<_>>();
                            (1..=2).contains(&parts.len())
                                && parts
                                    .iter()
                                    .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                                && full
                                    .trim_start_matches('v')
                                    .strip_prefix(alias)
                                    .is_some_and(|s| s.starts_with('.'))
                                && parse_version(full).is_some()
                        })
            }))
}

fn floating_action_target(left: &Dependency, right: &Dependency, difference: &Difference) -> bool {
    let alias_parts = |value: &str| {
        let parts: Vec<_> = value.trim_start_matches('v').split('.').collect();
        (1..=2).contains(&parts.len())
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    difference.paired.as_ref().is_some_and(|(a, b)| {
        let on_line = |alias: &str, version: &str| {
            version
                .trim_start_matches('v')
                .strip_prefix(alias.trim_start_matches('v'))
                .is_some_and(|tail| tail.starts_with('.'))
        };
        left.current.value.as_deref().is_some_and(alias_parts)
            && left.current.digest.is_some()
            && a.update_type == b.update_type
            && matches!(
                b.update_type.as_str(),
                "major" | "minor" | "patch" | "digest" | "pin"
            )
            && a.new_value == b.new_value
            && b.new_value.as_deref().is_some_and(|alias| {
                alias_parts(alias)
                    && left.current.value.as_deref().is_some_and(|current| {
                        current.split('.').count() == alias.split('.').count()
                    })
                    && a.new_version.as_deref().is_some_and(|v| on_line(alias, v))
                    && b.new_version.as_deref().is_some_and(|v| on_line(alias, v))
                    && right
                        .metadata
                        .as_ref()
                        .and_then(|m| m.tag_digests.get(alias))
                        .is_some_and(|digest| {
                            digest.len() == 40
                                && digest.bytes().all(|b| b.is_ascii_hexdigit())
                                && b.new_digest.as_ref() == Some(digest)
                        })
            })
            && a.new_version
                .as_deref()
                .and_then(parse_version)
                .zip(b.new_version.as_deref().and_then(parse_version))
                .is_some_and(|(a, b)| {
                    a.pre.is_empty() && b.pre.is_empty() && b.cmp_precedence(&a).is_gt()
                })
    })
}

fn cargo_compatible_target(left: &Dependency, right: &Dependency, difference: &Difference) -> bool {
    if !same_declaration(left, right)
        || left.current.version != right.current.version
        || right.manager != "cargo"
        || !right
            .metadata
            .as_ref()
            .is_some_and(|m| m.source == "crates" && m.stale.is_empty())
        || !left.metadata.as_ref().is_some_and(|m| m.source == "crate")
    {
        return false;
    }
    let compatible = |a: &Update, b: &Update| {
        a.new_value == b.new_value
            && a.new_digest.is_none()
            && b.new_digest.is_none()
            && a.update_type == "minor"
            && b.update_type == "major"
            && b.policy
                .as_ref()
                .is_some_and(|p| p.versions.as_deref() == Some("preserve"))
            && a.new_version
                .as_deref()
                .and_then(parse_version)
                .zip(b.new_version.as_deref().and_then(parse_version))
                .is_some_and(|(av, bv)| {
                    av.major == 0
                        && bv.major == 0
                        && av.minor > 0
                        && av.minor == bv.minor
                        && av.pre.is_empty()
                        && bv.pre.is_empty()
                        && bv.cmp_precedence(&av).is_gt()
                        && a.new_value.as_deref().is_some_and(|range| {
                            range == format!("0.{}", av.minor)
                                && semver::VersionReq::parse(range)
                                    .is_ok_and(|r| r.matches(&av) && r.matches(&bv))
                        })
                        && left
                            .current
                            .version
                            .as_deref()
                            .and_then(parse_version)
                            .is_some_and(|v| v.major == 0 && v.minor < av.minor)
                })
    };
    match difference.original_classification {
        Classification::PatchulaMissing => {
            serde_json::from_value::<Update>(difference.renovate.clone())
                .is_ok_and(|a| right.updates.iter().any(|b| compatible(&a, b)))
        }
        Classification::PatchulaExtra => {
            serde_json::from_value::<Update>(difference.patchula.clone())
                .is_ok_and(|b| left.updates.iter().any(|a| compatible(a, &b)))
        }
        _ => false,
    }
}

fn numeric_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut components = value.trim_start_matches('v').split('.');
    let version = (
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
        components.next()?.parse().ok()?,
    );
    components.next().is_none().then_some(version)
}
