//! Output-only adapter rules. Every discarded inventory row is audited; an
//! unsupported or changed proposal must remain actionable.
use crate::Exclusion;
use patchula_report::{Dependency, Report};
use std::collections::{BTreeMap, BTreeSet};

fn source(dep: &Dependency) -> Option<&str> {
    dep.metadata.as_ref().map(|m| m.source.as_str())
}

fn normalize(report: &mut Report, notes: &mut BTreeMap<String, usize>) {
    for repo in &mut report.repositories {
        for file in &mut repo.package_files {
            for dep in &mut file.dependencies {
                if dep.manager == "mise"
                    && matches!(source(dep), Some("github-tags" | "git-tags"))
                    && dep.name.starts_with("cargo:https://github.com/")
                    && dep.package == dep.name.trim_start_matches("cargo:")
                    && let Some(package) = dep.package.strip_prefix("https://github.com/")
                    && package.split('/').count() == 2
                    && !package.contains(['?', '#', '@'])
                {
                    dep.package = package.into();
                    *notes.entry("mise-cargo-git-identity".into()).or_default() += 1;
                }
                if dep.manager == "bun"
                    && file.path.rsplit('/').next() == Some("package.json")
                    && source(dep).is_none_or(|s| s == "npm")
                {
                    dep.manager = "npm".into();
                    *notes.entry("bun-npm-manifest-manager".into()).or_default() += 1;
                }
                let docker = source(dep) == Some("docker");
                let mise = dep.manager == "mise"
                    || source(dep) == Some("node-version")
                    || (matches!(dep.manager.as_str(), "regex" | "flux")
                        && matches!(
                            source(dep),
                            Some("github-releases" | "github-tags" | "helm")
                        ));
                for value in std::iter::once(&mut dep.current.version)
                    .chain(dep.updates.iter_mut().map(|u| &mut u.new_version))
                    .flatten()
                {
                    let normalized = if mise {
                        value
                            .strip_prefix('v')
                            .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
                    } else if docker {
                        value
                            .split_once('-')
                            .map(|(number, _)| number)
                            .filter(|number| {
                                !number.is_empty()
                                    && number.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                            })
                    } else {
                        None
                    };
                    if let Some(normalized) = normalized {
                        *notes
                            .entry(
                                if mise {
                                    "mise-version-prefix"
                                } else {
                                    "docker-numeric-version"
                                }
                                .into(),
                            )
                            .or_default() += 1;
                        *value = normalized.into();
                    }
                }
            }
            file.dependencies =
                patchula_report::normalize_dependencies(std::mem::take(&mut file.dependencies));
        }
    }
}

pub(super) fn reports(
    baseline: &mut Report,
    actual: &mut Report,
    notes: &mut BTreeMap<String, usize>,
) -> Vec<Exclusion> {
    normalize(baseline, notes);
    normalize(actual, notes);
    let mut exclusions = Vec::new();
    for repo in &mut baseline.repositories {
        let mut actual_repo = actual
            .repositories
            .iter_mut()
            .find(|r| r.repository == repo.repository);
        let has_actual_repo = actual_repo.is_some();
        let mut counts = BTreeMap::<(String, String, u32), (BTreeSet<String>, usize, usize)>::new();
        for file in &mut repo.package_files {
            let owners = file.dependencies.clone();
            let mut actual_file = actual_repo
                .as_deref_mut()
                .and_then(|r| r.package_files.iter_mut().find(|f| f.path == file.path));
            file.dependencies.retain(|dep| {
                let represented = actual_file.as_ref().is_some_and(|f| {
                    f.dependencies.iter().any(|a| {
                        a.manager == dep.manager && a.name == dep.name && a.package == dep.package
                    })
                });
                let exclusion = if has_actual_repo
                    && !represented
                    && matches!(
                        dep.manager.as_str(),
                        "terraform" | "github-actions" | "flux" | "npm"
                    )
                    && dep.updates.is_empty()
                    && dep.current.version.is_none()
                    && dep.current.digest.is_none()
                    && dep.current.value.as_deref().is_none_or(|v| {
                        ["file:", "./", "../"].iter().any(|p| v.starts_with(p)) || v.contains("${{")
                    }) {
                    Some((dep.manager.as_str(), "unversioned-inventory", 32))
                } else if dep.manager == "kubernetes"
                    && source(dep) == Some("kubernetes-api")
                    && dep.updates.is_empty()
                {
                    Some(("kubernetes", "api-version-inventory", 26))
                } else if matches!(dep.manager.as_str(), "helm-values" | "kubernetes")
                    && source(dep) == Some("docker")
                    && owners.iter().any(|owner| {
                        (owner.manager == "flux"
                            || (dep.manager == "helm-values" && owner.manager == "kubernetes"))
                            && owner.package == dep.package
                            && owner.current == dep.current
                            && owner.updates == dep.updates
                            && actual_file.as_ref().is_some_and(|f| {
                                f.dependencies.iter().any(|actual| {
                                    actual.manager == owner.manager
                                        && actual.name == owner.name
                                        && actual.package == owner.package
                                })
                            })
                    })
                {
                    Some((dep.manager.as_str(), "yaml-image-single-owner", 27))
                } else {
                    None
                };
                if let Some(key) = exclusion {
                    let (files, deps, updates) = counts
                        .entry((key.0.to_owned(), key.1.to_owned(), key.2))
                        .or_default();
                    files.insert(file.path.clone());
                    *deps += 1;
                    *updates += dep.updates.len();
                    return false;
                }
                if source(dep) == Some("docker")
                    && dep.current.value.is_none()
                    && dep.current.version.is_none()
                    && let Some(file) = &mut actual_file
                    && let Some(actual) = file.dependencies.iter_mut().find(|a| {
                        a.manager == dep.manager
                            && a.name == dep.name
                            && a.package == dep.package
                            && a.current.value.as_deref() == Some("latest")
                            && a.current.version.is_none()
                            && a.current.digest == dep.current.digest
                    })
                {
                    actual.current.value = None;
                    *notes.entry("implicit-latest-image".into()).or_default() += 1;
                }
                true
            });
        }
        for ((manager, rule, departure), (files, dependencies, updates)) in counts {
            exclusions.push(Exclusion {
                repository: repo.repository.clone(),
                manager,
                package_files: files.len(),
                dependencies,
                updates,
                rule,
                departure,
                link: format!("docs/departures.md#departure-{departure}"),
            });
        }
    }
    exclusions
}
