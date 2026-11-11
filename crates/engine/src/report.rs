//! Report assembly: one report row per dependency of each repository, with
//! digest-pinned proposals, image checks, advisories and lookup evidence.
use super::*;
use chrono::{DateTime, Utc};
use std::sync::Mutex;

/// What report assembly reads for every dependency of a repository.
#[derive(Clone, Copy)]
struct ReportCtx<'a> {
    eval: Option<&'a Evaluator>,
    config: Option<&'a Config>,
    lookups: &'a Lookups,
    sent: &'a Mutex<BTreeSet<Sent>>,
    now: DateTime<Utc>,
}

impl Engine {
    /// Stage: report — one report row per dependency of each repository.
    pub(super) async fn assemble_reports(
        &self,
        work: &mut [Work],
        lookups: &Lookups,
        sent: &Mutex<BTreeSet<Sent>>,
        now: DateTime<Utc>,
    ) {
        for repo in work {
            self.assemble_repository(repo, lookups, sent, now).await;
        }
    }

    async fn assemble_repository(
        &self,
        repo: &mut Work,
        lookups: &Lookups,
        sent: &Mutex<BTreeSet<Sent>>,
        now: DateTime<Utc>,
    ) {
        let mut files: BTreeMap<String, Vec<ReportDependency>> = BTreeMap::new();
        let ctx = ReportCtx {
            eval: repo.evaluator.as_ref(),
            config: repo.config.as_ref(),
            lookups,
            sent,
            now,
        };
        for dep in repo.dependencies.iter_mut() {
            let value = self.report_dependency(&mut repo.report, dep, &ctx).await;
            files.entry(dep.file.clone()).or_default().push(value);
        }
        repo.report.package_files = files
            .into_iter()
            .map(|(path, dependencies)| PackageFile {
                path,
                dependencies: patchula_report::normalize_dependencies(dependencies),
            })
            .collect();
        repo.report.warnings.sort();
        repo.report.warnings.dedup();
        repo.report.lookup_failures.sort();
        repo.report.lookup_failures.dedup();
    }

    /// One dependency's report row.
    async fn report_dependency(
        &self,
        report: &mut Repository,
        dep: &mut Dep,
        ctx: &ReportCtx<'_>,
    ) -> ReportDependency {
        let ReportCtx {
            eval,
            config,
            lookups,
            sent,
            now,
        } = *ctx;
        let mut proposals = std::mem::take(&mut dep.evaluation.updates);
        if let Some(eval) = eval
            && dep.needs.image
            && !dep.needs.ignored
        {
            for proposal in &mut proposals {
                if eval.needs_digest(&dep.dependency, proposal)
                    && let Some(tag) = &proposal.new_version
                {
                    match lookups
                        .image_digests
                        .get(&(dep.package.clone(), tag.clone()))
                    {
                        Some(Ok(digest)) => proposal.new_digest = Some(digest.clone()),
                        Some(Err(e)) => report.lookup_failures.push(lookup_failure(
                            &dep.dependency,
                            e,
                            format!("digest of {tag}: {e}"),
                        )),
                        None => {}
                    }
                }
            }
            if let Some(result) = lookups
                .image_digests
                .get(&(dep.package.clone(), dep.current.clone()))
            {
                match result {
                    Ok(digest) => match eval.digest_proposal(&dep.dependency, digest) {
                        Ok(Some(p)) => proposals.push(p),
                        Err(e) => report.warnings.push(format!("{}: {e}", dep.name)),
                        Ok(None) => {}
                    },
                    Err(e) => report.lookup_failures.push(lookup_failure(
                        &dep.dependency,
                        e,
                        format!("digest of {}: {e}", dep.current),
                    )),
                }
            }
        }
        if let Some(eval) = eval
            && dep.tagged_action()
            && !dep.needs.ignored
        {
            for proposal in &mut proposals {
                if eval.needs_digest(&dep.dependency, proposal)
                    && let Some(reference) = proposal
                        .new_version
                        .as_deref()
                        .or(proposal.new_value.as_deref())
                {
                    match lookups
                        .digests
                        .get(&(dep.package.clone(), reference.to_owned()))
                    {
                        Some(Ok(sha)) => proposal.new_digest = Some(sha.clone()),
                        Some(Err(e)) => {
                            proposal.new_digest = None;
                            report.lookup_failures.push(lookup_failure(
                                &dep.dependency,
                                e,
                                format!("cannot pin {reference} to a digest: {e}"),
                            ));
                        }
                        None => {}
                    }
                }
            }
            let reference = dep.resolved.as_deref().unwrap_or(&dep.current);
            if let Some(result) = lookups
                .digests
                .get(&(dep.package.clone(), reference.to_owned()))
            {
                match result {
                    Ok(sha) => match eval.digest_proposal_with_catalog(
                        &dep.dependency,
                        sha,
                        dep.needs
                            .catalog_key
                            .as_ref()
                            .and_then(|key| lookups.catalogs.get(key))
                            .and_then(|c| c.as_ref().ok()),
                    ) {
                        Ok(Some(proposal)) => proposals.push(proposal),
                        Err(e) => report.warnings.push(format!("{}: {e}", dep.name)),
                        Ok(None) => {}
                    },
                    Err(e) => report.lookup_failures.push(lookup_failure(
                        &dep.dependency,
                        e,
                        format!("cannot resolve the digest of {reference}: {e}"),
                    )),
                }
            }
        }
        let mut image_advisories = Vec::new();
        let mut current_image = None;
        if dep.needs.sboms
            && let (Some(eval), Some(config)) = (eval, config)
            && let Some(old) = dep.dependency.digest.as_ref()
        {
            let configured = &config.security.image_platforms;
            let mut platforms = configured.clone();
            let digests =
                std::iter::once(old).chain(proposals.iter().filter_map(|p| p.new_digest.as_ref()));
            for digest in digests {
                let side = image_side(&lookups.image_sboms, &dep.package, digest, configured);
                if let Some(platform) = patchula_lookup::images::fallback(&side)
                    && !platforms.contains(&platform)
                {
                    platforms.push(platform);
                }
            }
            let platforms = &platforms;
            let before = image_side(&lookups.image_sboms, &dep.package, old, platforms);
            let silenced = config
                .security
                .quiet_images
                .iter()
                .find(|(pattern, _)| {
                    patchula_config::Patterns::compile(std::slice::from_ref(*pattern))
                        .is_ok_and(|p| p.matches_any(&dep.package, &dep.name))
                })
                .map(|(_, reason)| reason.clone());
            let silence = |mut check: patchula_report::ImageCheck| {
                if check.state.warns() {
                    check.silenced.clone_from(&silenced);
                }
                check
            };
            for proposal in &mut proposals {
                let Some(new) = proposal.new_digest.clone().filter(|new| new != old) else {
                    continue;
                };
                let after = image_side(&lookups.image_sboms, &dep.package, &new, platforms);
                let Some(mut check) = patchula_lookup::images::compare(
                    (old, &before),
                    (&new, &after),
                    &lookups.image_listing,
                ) else {
                    continue;
                };
                if let Some(failure) = &lookups.osv_failure
                    && matches!(
                        check.state,
                        patchula_report::ImageState::Checked
                            | patchula_report::ImageState::PartialSources
                    )
                {
                    check.state = patchula_report::ImageState::Unreadable;
                    check.detail = Some(failure.clone());
                }
                let (verified, introduced) =
                    patchula_lookup::images::advisories(&check, &lookups.image_found);
                if check.state != patchula_report::ImageState::Unreadable && !verified.is_empty() {
                    let secured = match eval.secure(&dep.dependency, proposal) {
                        Ok(secured) => secured,
                        Err(e) => {
                            report.warnings.push(format!("{}: {e}", dep.name));
                            false
                        }
                    };
                    if secured {
                        proposal.fixes = check.fixed.iter().map(|f| f.id.clone()).collect();
                        proposal.fixes.dedup();
                        let label = &config.security.label;
                        if let Some(policy) = &mut proposal.policy
                            && !label.is_empty()
                            && !policy.labels.contains(label)
                        {
                            policy.labels.push(label.clone());
                        }
                    }
                    image_advisories.extend(verified);
                }
                if check.state != patchula_report::ImageState::Unreadable {
                    check.introduced_details = introduced;
                }
                proposal.image = Some(silence(check));
            }
            current_image = patchula_lookup::images::image_state(&before).map(silence);
        }
        image_advisories.sort();
        image_advisories.dedup();
        let advisory = dep
            .needs
            .advisory
            .clone()
            .and_then(|query| lookups.advisories.get(&query).map(|found| (query, found)))
            .filter(|(_, found)| !found.is_empty());
        if let (Some(eval), Some((query, found))) = (eval, &advisory) {
            match eval.transitive_fix(&dep.dependency, (query, found)) {
                Ok(Some(fix)) => proposals.push(fix),
                Err(e) => report.warnings.push(format!("{}: {e}", dep.name)),
                Ok(None) => {}
            }
        }
        if let Some((query, found)) = &advisory {
            let label = config
                .map(|c| c.security.label.as_str())
                .unwrap_or_default();
            for proposal in &mut proposals {
                let Some(target) = proposal
                    .new_version
                    .as_ref()
                    .or(proposal.new_value.as_ref())
                else {
                    continue;
                };
                proposal.fixes = patchula_lookup::fixes(&dep.dependency, query, found, target);
                if let Some(policy) = &mut proposal.policy
                    && !proposal.fixes.is_empty()
                    && !label.is_empty()
                    && !policy.labels.iter().any(|l| l == label)
                {
                    policy.labels.push(label.into());
                }
            }
        }
        proposals.sort();
        proposals.dedup();
        let mut held_back = std::mem::take(&mut dep.evaluation.held_back);
        // The lookup worked; only the declaration cannot be rewritten to allow
        // the release, so it is a notice rather than a failure.
        for missed in std::mem::take(&mut dep.evaluation.not_proposed) {
            report.notices.push(patchula_report::Notice {
                manager: dep.manager,
                path: dep.file.clone(),
                line: Some(dep.location.line),
                name: dep.name.clone(),
                package: dep.package.clone(),
                current: dep.current.clone(),
                compare_url: dep.resolved.as_deref().and_then(|resolved| {
                    patchula_notes::compare_url(dep.source, &dep.package, resolved, &missed.version)
                }),
                available: missed.version,
                reason: Some(missed.reason),
            });
        }
        if dep.report_only() {
            // Only copier update applies a template: report the
            // newest eligible tag and propose nothing to edit.
            let rank = |kind: UpdateKind| match kind {
                UpdateKind::Major => 0,
                UpdateKind::Minor => 1,
                UpdateKind::Patch => 2,
                UpdateKind::Digest | UpdateKind::Pin => 3,
            };
            if let Some(available) = proposals
                .iter()
                .min_by_key(|p| rank(p.update_type))
                .and_then(|p| p.new_version.clone())
            {
                report.notices.push(patchula_report::Notice {
                    manager: dep.manager,
                    path: dep.file.clone(),
                    line: Some(dep.location.line),
                    name: dep.name.clone(),
                    package: dep.package.clone(),
                    current: dep.current.clone(),
                    compare_url: patchula_notes::compare_url(
                        dep.source,
                        &dep.package,
                        &dep.current,
                        &available,
                    ),
                    available,
                    reason: None,
                });
            }
            proposals.clear();
            held_back.clear();
        }
        let stale: BTreeMap<String, i64> = if dep.needs.image {
            let mut stale = self.sources.registry_stale(&dep.package).await;
            stale.retain(|key, _| {
                *key == patchula_datasource::StaleKey::CATALOG
                    || key.is_manifest_of(&dep.current)
                    || proposals.iter().any(|p| {
                        p.new_version
                            .as_ref()
                            .is_some_and(|v| key.is_manifest_of(v))
                    })
            });
            stale
                .into_iter()
                .map(|(key, at)| (key.to_string(), at))
                .collect()
        } else {
            BTreeMap::new()
        };
        if !stale.is_empty() {
            report.warnings.push(format!(
                "{}: stale cached registry lookup after rate limiting ({})",
                dep.name,
                stale.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        let catalog = dep
            .needs
            .catalog_key
            .as_ref()
            .and_then(|key| lookups.catalogs.get(key))
            .and_then(|c| c.as_ref().ok());
        if dep.tagged_action() && catalog.is_some_and(|c| c.versions.is_empty()) {
            report.warnings.push(format!(
                "{}: registry returned an empty tag catalog; version lookup is unavailable",
                dep.name
            ));
        }
        let resolved = catalog.and_then(|c| {
            eval.and_then(|eval| {
                eval.resolved_current_version(&dep.dependency, c)
                    .ok()
                    .flatten()
            })
        });
        let resolution = resolved.as_ref().or(dep.resolved.as_ref()).map(|version| {
            let kind = if dep.tagged_action() {
                if dep.digest.is_some() {
                    ResolutionKind::PinnedCommit
                } else {
                    ResolutionKind::FloatingRef
                }
            } else if dep.resolved_from_lockfile() {
                ResolutionKind::Lockfile
            } else if resolved.is_some()
                && (matches!(dep.kind, DependencyKind::Tool)
                    || (dep.kind == DependencyKind::Toolchain
                        && !patchula_versioning::Ecosystem::of(dep.scheme, dep.source)
                            .supports_ranges()))
            {
                ResolutionKind::ReleaseLine
            } else if resolved.is_some() && dep.kind == DependencyKind::Toolchain {
                ResolutionKind::SupportedRange
            } else {
                ResolutionKind::Declared
            };
            patchula_report::Resolution {
                kind,
                version: version.clone(),
            }
        });
        let proposals_refs: BTreeSet<_> = proposals
            .iter()
            .filter_map(|p| p.new_value.clone())
            .collect();
        ReportDependency {
            manager: dep.manager,
            name: dep.name.clone(),
            occurrence: None,
            package: dep.package.clone(),
            registry: dep.registry.as_ref().map(|r| r.recorded().to_owned()),
            current: Current {
                value: Some(dep.current.clone()),
                version: catalog
                    .and_then(|c| {
                        eval.and_then(|eval| {
                            eval.resolved_current_version(&dep.dependency, c)
                                .ok()
                                .flatten()
                        })
                    })
                    .or_else(|| dep.resolved.clone())
                    .or_else(|| {
                        let scheme = patchula_versioning::Ecosystem::of(dep.scheme, dep.source);
                        (dep.kind != patchula_words::DependencyKind::Runner)
                            .then(|| {
                                patchula_versioning::Versioning::parse(&scheme, &dep.current).ok()
                            })
                            .flatten()
                            .map(|v| v.0.to_string())
                    }),
                digest: dep.digest.clone(),
            },
            updates: proposals,
            metadata: Some(patchula_report::DependencyMetadata {
                tag_digests: if dep.tagged_action() {
                    catalog
                        .into_iter()
                        .flat_map(|c| &c.versions)
                        .filter(|v| proposals_refs.contains(&v.version))
                        .filter_map(|v| {
                            v.digest
                                .as_ref()
                                .map(|digest| (v.version.clone(), digest.clone()))
                        })
                        .collect()
                } else {
                    BTreeMap::new()
                },
                source: dep.source,
                kinds: dep.kinds(),
                locations: vec![dep.location.clone().into()],
                held_back,
                stale,
                lookup: eval.and_then(|eval| {
                    match eval.lookup_evidence(&dep.dependency, catalog, now) {
                        Ok(mut evidence) => {
                            evidence.catalog = dep
                                .needs
                                .catalog_key
                                .as_ref()
                                .and_then(|key| lookups.evidence.get(key))
                                .cloned();
                            Some(evidence)
                        }
                        Err(e) => {
                            report.warnings.push(format!("{}: {e}", dep.name));
                            None
                        }
                    }
                }),
                resolution,
                advisories: advisory
                    .map(|(query, found)| {
                        patchula_lookup::report_advisories(&dep.dependency, &query, found)
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .chain(image_advisories)
                    .collect(),
                image: current_image,
                credentialed: dep
                    .needs
                    .catalog_key
                    .as_ref()
                    .is_some_and(|key| lookups.credentialed.contains(key))
                    || sent
                        .lock()
                        .expect("sent packages")
                        .contains(&Sent::of(&dep.dependency)),
            }),
        }
    }
}
