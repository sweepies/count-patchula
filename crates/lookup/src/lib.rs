//! Ordered repository policy evaluated over looked-up, batch-shared catalogs.
#![forbid(unsafe_code)]
mod advisories;
pub use advisories::{advisory_query, affects, fixes, report_advisories};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use patchula_config::{
    Config, DependencyKind, Manager, Patterns, Policy, Rule, Scheme, Source, Update, VersionPolicy,
};
use patchula_datasource::{Catalog, Version};
use patchula_manager::Dependency;
use patchula_report::{HeldBack, ReleaseAge, Update as Proposal, UpdatePolicy, Window};
use patchula_versioning::{Ecosystem, Parsed, Versioning};
use regex::Regex;
use std::cmp::Ordering;

pub type Result<T> = std::result::Result<T, String>;
#[derive(Default)]
pub struct Evaluation {
    pub updates: Vec<Proposal>,
    pub held_back: Vec<HeldBack>,
}
enum FilterKind {
    Regex(Regex),
    Semver(semver::VersionReq),
}
struct Filter {
    negated: bool,
    kind: FilterKind,
}
impl Filter {
    fn compile(text: &str) -> Result<Self> {
        let (negated, text) = text.strip_prefix('!').map_or((false, text), |v| (true, v));
        let kind = if let Some(value) = text.strip_prefix("regex:") {
            FilterKind::Regex(Regex::new(value).map_err(|e| e.to_string())?)
        } else if let Some(value) = text.strip_prefix("semver:") {
            FilterKind::Semver(semver::VersionReq::parse(value).map_err(|e| e.to_string())?)
        } else {
            return Err("invalid version filter".into());
        };
        Ok(Self { negated, kind })
    }
    fn matches(&self, text: &str) -> bool {
        let yes = match &self.kind {
            FilterKind::Regex(r) => r.is_match(text),
            FilterKind::Semver(r) => {
                semver::Version::parse(text.trim_start_matches('v')).is_ok_and(|v| r.matches(&v))
            }
        };
        yes != self.negated
    }
}
struct CompiledRule {
    rule: Rule,
    packages: Patterns,
    dependencies: Patterns,
    files: Patterns,
    current: Option<Filter>,
    permitted: Option<Filter>,
}
pub struct Evaluator {
    config: Config,
    rules: Vec<CompiledRule>,
    ignore: Patterns,
    exclude: Patterns,
    permitted: Option<Filter>,
    security_permitted: Option<Filter>,
}
/// The advisories affecting a dependency's current version, with the query
/// that found them.
pub type Advisories<'a> = (
    &'a patchula_datasource::AdvisoryQuery,
    &'a [std::sync::Arc<patchula_datasource::Advisory>],
);
#[derive(Clone)]
struct Effective<'a> {
    policy: Policy,
    digest: bool,
    permitted: Option<&'a Filter>,
}
impl<'a> Effective<'a> {
    fn apply(&mut self, o: &patchula_config::Overrides, permitted: Option<&'a Filter>) {
        let p = &mut self.policy;
        macro_rules! replace {($($field:ident),*)=>{$(if let Some(v)=&o.$field {p.$field=v.clone();})*};}
        replace!(
            active,
            versions,
            channel,
            merge,
            labels,
            commit_prefix,
            release_notes,
            body_notes,
            wait_hours,
            windows
        );
        if let Some(group) = &o.group {
            p.group = (!group.is_empty()).then(|| group.clone());
        }
        if let Some(digest) = o.digest {
            self.digest = digest;
        }
        if o.permitted.is_some() {
            self.permitted = permitted;
        }
    }
}
impl Evaluator {
    /// Shared catalogs use the shortest active update-class wait. A zero-age
    /// rule keeps normal freshness even if another class waits longer.
    pub fn catalog_wait_hours(&self, dep: &Dependency) -> u32 {
        [
            Update::Major,
            Update::Minor,
            Update::Patch,
            Update::Pin,
            Update::Digest,
        ]
        .into_iter()
        .map(|kind| self.effective(dep, kind))
        .filter(|effective| effective.policy.active)
        .map(|effective| effective.policy.wait_hours)
        .min()
        .unwrap_or(0)
    }
    /// Effective class exclusions, including digest policy, are report evidence.
    pub fn disabled_updates(&self, dep: &Dependency) -> Vec<String> {
        [
            Update::Major,
            Update::Minor,
            Update::Patch,
            Update::Pin,
            Update::Digest,
        ]
        .into_iter()
        .filter(|kind| {
            let effective = self.effective(dep, *kind);
            self.ignored(dep)
                || !effective.policy.active
                || (*kind == Update::Digest && !effective.digest)
        })
        .map(|kind| patchula_manager::word(&kind))
        .collect()
    }
    pub fn lookup_evidence(
        &self,
        dep: &Dependency,
        catalog: Option<&Catalog>,
        now: DateTime<Utc>,
    ) -> patchula_report::LookupEvidence {
        let scheme = scheme(dep).ok();
        let current = dep.resolved.as_deref().unwrap_or(&dep.current);
        let baseline = scheme.and_then(|s| s.parse(current).ok());
        let extract = dep.extract.as_ref().and_then(|v| Regex::new(v).ok());
        let mut rejected = Vec::new();
        for version in catalog.into_iter().flat_map(|c| &c.versions) {
            if !version.deprecated && !version.yanked {
                continue;
            }
            let value = if let Some(extract) = &extract {
                let Some(value) = extract
                    .captures(&version.version)
                    .and_then(|c| c.name("version"))
                else {
                    continue;
                };
                value.as_str()
            } else {
                &version.version
            };
            if let Some(scheme) = scheme
                && let Some(baseline) = &baseline
                && scheme
                    .parse(value)
                    .is_ok_and(|v| scheme.order(&v, baseline).is_gt())
            {
                rejected.push(patchula_report::RejectedVersion {
                    version: value.into(),
                    reason: if version.yanked {
                        "yanked"
                    } else {
                        "deprecated"
                    }
                    .into(),
                });
            }
        }
        rejected.sort_by(|a, b| a.version.cmp(&b.version).then(a.reason.cmp(&b.reason)));
        rejected.dedup();
        patchula_report::LookupEvidence {
            evaluated_at: timestamp(now),
            disabled_updates: self.disabled_updates(dep),
            rejected,
            catalog: None,
        }
    }
    pub fn new(config: &Config) -> Result<Self> {
        let rules = config
            .rules
            .iter()
            .map(|rule| {
                Ok(CompiledRule {
                    rule: rule.clone(),
                    packages: Patterns::compile(&rule.select.packages)?,
                    dependencies: Patterns::compile(&rule.select.dependencies)?,
                    files: Patterns::compile(&rule.select.files)?,
                    current: rule
                        .select
                        .current
                        .as_deref()
                        .map(Filter::compile)
                        .transpose()?,
                    permitted: rule
                        .apply
                        .permitted
                        .as_deref()
                        .map(Filter::compile)
                        .transpose()?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            config: config.clone(),
            rules,
            ignore: Patterns::compile(&config.ignore)?,
            exclude: Patterns::compile(&config.exclude)?,
            permitted: config
                .defaults
                .permitted
                .as_deref()
                .map(Filter::compile)
                .transpose()?,
            security_permitted: config
                .security
                .apply
                .permitted
                .as_deref()
                .map(Filter::compile)
                .transpose()?,
        })
    }
    pub fn ignored(&self, dep: &Dependency) -> bool {
        (!self.config.ignore.is_empty() && self.ignore.matches_any(&dep.name, &dep.package))
            || (!self.config.exclude.is_empty() && self.exclude.matches(&dep.file))
    }
    pub fn needs_lookup(&self, dep: &Dependency) -> bool {
        if (dep.kind == DependencyKind::Runner && dep.current == "latest")
            || dep.kind == DependencyKind::Transitive
        {
            return false;
        }
        if self.ignored(dep)
            || ![Update::Major, Update::Minor, Update::Patch, Update::Pin]
                .into_iter()
                .any(|k| self.effective(dep, k).policy.active)
        {
            return false;
        }
        if dep.tagged_action() {
            return Ecosystem::Loose
                .parse(dep.resolved.as_deref().unwrap_or(&dep.current))
                .is_ok();
        }
        true
    }
    fn effective(&self, dep: &Dependency, update: Update) -> Effective<'_> {
        let mut effective = Effective {
            policy: self.config.defaults.clone(),
            digest: if dep.kind == DependencyKind::ContainerImage {
                self.config.defaults.image_digests
            } else {
                self.config.defaults.action_digests
            },
            permitted: self.permitted.as_ref(),
        };
        // Runtime constraints describe supported versions, rather than an
        // installed package version. Explicit rules can still
        // request exact pinning for a repository that wants it.
        if (dep.manager == Manager::Terraform || dep.manager == Manager::Npm)
            && dep.kind == DependencyKind::Toolchain
        {
            effective.policy.versions = VersionPolicy::Preserve;
        }
        for compiled in &self.rules {
            let s = &compiled.rule.select;
            if !compiled.packages.matches(&dep.package)
                || !compiled.dependencies.matches(&dep.name)
                || !compiled.files.matches(&dep.file)
                || !any(&s.managers, dep.manager)
                || !any(&s.sources, dep.source)
                || !(s.kinds.is_empty() || dep.kinds().iter().any(|kind| s.kinds.contains(kind)))
                || !any(&s.updates, update)
                || compiled
                    .current
                    .as_ref()
                    .is_some_and(|f| !f.matches(dep.resolved.as_deref().unwrap_or(&dep.current)))
            {
                continue;
            }
            effective.apply(&compiled.rule.apply, compiled.permitted.as_ref());
        }
        // Peers express consumer compatibility, even when installed packages
        // use exact versions. Exact peer declarations still bump in place.
        if dep.manager == Manager::Npm && dep.kind == DependencyKind::Peer {
            effective.policy.versions = if Ecosystem::Npm.parse(&dep.current).is_ok() {
                VersionPolicy::Exact
            } else {
                VersionPolicy::Widen
            };
        }
        effective
    }
    pub fn needs_digest(&self, dep: &Dependency, proposal: &Proposal) -> bool {
        let update = parse_update(&proposal.update_type);
        (dep.source == Source::Docker
            || dep.source == Source::Oci
            || dep.manager == Manager::GithubActions)
            && matches!(
                dep.kind,
                DependencyKind::Action
                    | DependencyKind::ReusableWorkflow
                    | DependencyKind::ContainerImage
                    | DependencyKind::Chart
            )
            && self.effective(dep, update).digest
    }
    pub fn digest_proposal(&self, dep: &Dependency, new_digest: &str) -> Option<Proposal> {
        if self.ignored(dep)
            || dep.kind == DependencyKind::Runner
            || dep.digest.as_deref() == Some(new_digest)
        {
            return None;
        }
        let update = if dep.digest.is_some() {
            Update::Digest
        } else {
            Update::Pin
        };
        let effective = self.effective(dep, update);
        if !effective.policy.active || !effective.digest {
            return None;
        }
        Some(proposal(
            dep.resolved.clone().or_else(|| Some(dep.current.clone())),
            dep.resolved.clone(),
            Some(new_digest.into()),
            update,
            &effective.policy,
        ))
    }
    pub fn digest_proposal_with_catalog(
        &self,
        dep: &Dependency,
        new_digest: &str,
        catalog: Option<&Catalog>,
    ) -> Option<Proposal> {
        let mut proposal = self.digest_proposal(dep, new_digest)?;
        if dep.tagged_action() {
            let mut target = dep.clone();
            target.digest = Some(new_digest.into());
            if let Some(version) = catalog.and_then(|c| resolved_current_version(&target, c)) {
                proposal.new_version = Some(version);
            }
        }
        Some(proposal)
    }
    /// A lockfile-only package's one update: the lowest fixed release that
    /// fixes the most of its advisories without a breaking change, under
    /// `security.apply`. Only Cargo locks can be moved to an exact version of
    /// one package.
    pub fn transitive_fix(&self, dep: &Dependency, advisories: Advisories<'_>) -> Option<Proposal> {
        let (query, found) = advisories;
        if dep.kind != DependencyKind::Transitive
            || dep.source != Source::Crates
            || self.ignored(dep)
        {
            return None;
        }
        let scheme = scheme(dep).ok()?;
        let current = scheme.parse(&dep.current).ok()?;
        let (target, fixes) = found
            .iter()
            .filter_map(|a| crate::advisories::fixed_in(a, query, scheme, &current))
            .filter_map(|v| Some((scheme.parse(&v).ok()?, v)))
            // The lock can only move within what the dependents already allow.
            .filter(|(parsed, _)| scheme.classify(&current, parsed) != Update::Major)
            .map(|(parsed, v)| {
                let fixes = crate::fixes(dep, query, found, &v);
                (parsed, v, fixes)
            })
            .filter(|(_, _, fixes)| !fixes.is_empty())
            .max_by(|a, b| {
                a.2.len()
                    .cmp(&b.2.len())
                    .then_with(|| scheme.order(&b.0, &a.0))
            })
            .map(|(_, v, fixes)| (v, fixes))?;
        let kind = scheme.classify(&current, &scheme.parse(&target).ok()?);
        let mut effective = self.effective(dep, kind);
        effective.apply(
            &self.config.security.apply,
            self.security_permitted.as_ref(),
        );
        if !effective.policy.active {
            return None;
        }
        let mut update = proposal(None, Some(target), None, kind, &effective.policy);
        update.fixes = fixes;
        Some(update)
    }
    pub fn propose(&self, dep: &Dependency, catalog: &Catalog) -> Result<Vec<Proposal>> {
        Ok(self.evaluate_at(dep, catalog, Utc::now())?.updates)
    }
    /// One fixed instant per batch makes release-age boundaries deterministic.
    pub fn evaluate_at(
        &self,
        dep: &Dependency,
        catalog: &Catalog,
        now: DateTime<Utc>,
    ) -> Result<Evaluation> {
        self.evaluate_with_age_overrides(dep, catalog, now, &Default::default())
    }
    /// An explicit dashboard click bypasses only the selected release-age gate.
    pub fn evaluate_with_age_overrides(
        &self,
        dep: &Dependency,
        catalog: &Catalog,
        now: DateTime<Utc>,
        overrides: &std::collections::BTreeSet<(String, String)>,
    ) -> Result<Evaluation> {
        self.evaluate_with(dep, catalog, now, overrides, None)
    }
    /// `security.apply` governs every candidate that fixes one of the current
    /// version's advisories, so a fix can skip the wait the rules would impose.
    pub fn evaluate_with(
        &self,
        dep: &Dependency,
        catalog: &Catalog,
        now: DateTime<Utc>,
        overrides: &std::collections::BTreeSet<(String, String)>,
        advisories: Option<Advisories<'_>>,
    ) -> Result<Evaluation> {
        let fixes = |version: &str| {
            advisories
                .map(|(query, found)| crate::fixes(dep, query, found, version))
                .unwrap_or_default()
        };
        if self.ignored(dep) {
            return Ok(Evaluation::default());
        }
        let mut held_back = Vec::new();
        let mut eligible = |version: &Version,
                            selected: &str,
                            kind: Update,
                            effective: &Effective<'_>|
         -> Result<bool> {
            if effective.policy.channel == patchula_config::ReleaseChannel::Lts {
                let node = (dep.source == Source::Mise
                    && matches!(dep.package.as_str(), "node" | "core:node"))
                    || (dep.source == Source::GithubReleases
                        && dep.package == "actions/node-versions");
                if !node {
                    return Err("LTS channel is supported only for Node distributions and setup-node inputs".into());
                }
                let lts = catalog
                    .dist_tags
                    .get("lts")
                    .and_then(|v| Ecosystem::Loose.parse(v).ok())
                    .ok_or_else(|| "Node catalog has no LTS channel evidence".to_owned())?;
                if !Ecosystem::Loose
                    .parse(selected)
                    .is_ok_and(|v| v.0.major == lts.0.major)
                {
                    return Ok(false);
                }
            }
            // The gate chooses proposals; it never removes extracted/current releases.
            if effective.policy.wait_hours == 0
                || version.published_at.is_none()
                || overrides.contains(&(selected.into(), patchula_manager::word(&kind)))
            {
                return Ok(true);
            }
            let published = published(version)?;
            let until = published
                .checked_add_signed(Duration::hours(i64::from(effective.policy.wait_hours)))
                .ok_or_else(|| "release eligibility timestamp overflow".to_owned())?;
            if until <= now {
                return Ok(true);
            }
            held_back.push(HeldBack {
                version: selected.into(),
                versions: vec![selected.into()],
                count: 1,
                update_type: patchula_manager::word(&kind),
                published_at: timestamp(published),
                eligible_at: timestamp(until),
                target_eligible_at: Some(timestamp(until)),
            });
            Ok(false)
        };
        let scheme = scheme(dep)?;
        let extractor = dep
            .extract
            .as_ref()
            .map(|s| Regex::new(s).map_err(|e| e.to_string()))
            .transpose()?;
        let normalize = |text: &str| -> Option<String> {
            if let Some(regex) = &extractor {
                regex
                    .captures(text)?
                    .name("version")
                    .map(|m| m.as_str().into())
            } else {
                Some(text.into())
            }
        };
        let resolved = resolved_current_version(dep, catalog);
        let current = resolved
            .as_deref()
            .or(dep.resolved.as_deref())
            .unwrap_or(&dep.current);
        let current = normalize(current).unwrap_or_else(|| current.into());
        // Dist-tags and mise's latest are intentionally floating values. Exact
        // policy can pin them without pretending a resolved version was known.
        let tag_target = catalog
            .dist_tags
            .get(&dep.current)
            .and_then(|v| normalize(v))
            .or_else(|| {
                if dep.kind != DependencyKind::Tool || dep.current != "latest" {
                    return None;
                }
                let effective = self.effective(dep, Update::Pin);
                catalog
                    .versions
                    .iter()
                    .filter(|v| !v.yanked && !v.deprecated)
                    .filter_map(|v| {
                        let value = normalize(&v.version)?;
                        let parsed = scheme.parse(&value).ok()?;
                        (parsed.0.pre.is_empty()
                            && effective.permitted.is_none_or(|f| f.matches(&value)))
                        .then_some((parsed, value))
                    })
                    .max_by(|(a, av), (b, bv)| scheme.order(a, b).then_with(|| av.cmp(bv)))
                    .map(|(_, v)| v)
            });
        if let Some(target) = tag_target {
            let effective = self.effective(dep, Update::Pin);
            if effective.policy.active
                && effective.policy.versions == VersionPolicy::Exact
                && effective.permitted.is_none_or(|f| f.matches(&target))
            {
                let ceiling = scheme.parse(&target).map_err(|e| e.to_string())?;
                let mut candidates: Vec<_> = catalog
                    .versions
                    .iter()
                    .filter(|v| !v.yanked && !v.deprecated)
                    .filter_map(|v| {
                        Some((
                            v,
                            normalize(&v.version)?,
                            scheme.parse(&normalize(&v.version)?).ok()?,
                        ))
                    })
                    .filter(|(_, value, p)| {
                        scheme.order(p, &ceiling) != Ordering::Greater
                            && (p.0.pre.is_empty() || !ceiling.0.pre.is_empty())
                            && effective.permitted.is_none_or(|f| f.matches(value))
                    })
                    .collect();
                candidates.sort_by(|a, b| candidate_order(scheme, &b.2, b.0, &a.2, a.0));
                let mut selected = None;
                for (version, value, _) in candidates {
                    if eligible(version, &value, Update::Pin, &effective)? {
                        selected = Some((version, value));
                        break;
                    }
                }
                let Some((version, target)) = selected else {
                    return Ok(Evaluation {
                        updates: Vec::new(),
                        held_back: summarize(held_back, scheme),
                    });
                };
                let mut update = proposal(
                    Some(
                        scheme
                            .new_range(&dep.current, &target, VersionPolicy::Exact)
                            .map_err(|e| e.to_string())?,
                    ),
                    Some(target),
                    None,
                    Update::Pin,
                    &effective.policy,
                );
                update.release_age = release_age(version, now);
                return Ok(Evaluation {
                    updates: vec![update],
                    held_back: summarize(held_back, scheme),
                });
            }
            return Ok(Evaluation::default());
        }
        let baseline = scheme.parse(&current).ok().or_else(|| {
            if scheme == Ecosystem::Npm {
                nodejs_semver::Range::parse(&dep.current)
                    .ok()?
                    .min_version()
                    .and_then(|v| scheme.parse(&v.to_string()).ok())
            } else {
                None
            }
        });
        // Branch and opaque refs are preserved. Their digest is proposed through
        // digest_proposal after the engine resolves that same ref.
        if baseline.is_none()
            && (dep.manager == Manager::GithubActions || scheme == Ecosystem::Docker)
        {
            return Ok(Evaluation::default());
        }
        let baseline = baseline.or_else(|| {
            if !matches!(
                scheme,
                Ecosystem::Cargo
                    | Ecosystem::Hashicorp
                    | Ecosystem::Pep440
                    | Ecosystem::Poetry
                    | Ecosystem::Composer
                    | Ecosystem::Rubygems
            ) {
                return None;
            }
            // A range's lowest version only classifies the update; it is never
            // reported as what is installed. `^5.4 || ^6.0` and `>=1.0 <2.0`
            // start at their first alternative's first bound.
            let first = current
                .split([',', '|'])
                .next()?
                .trim()
                .trim_start_matches(['=', '>', '<', '~', '^', '!', ' '])
                .split_whitespace()
                .next()?;
            scheme.parse(first).ok().or_else(|| {
                (scheme == Ecosystem::Cargo)
                    .then(|| Ecosystem::Loose.parse(first).ok())
                    .flatten()
            })
        });
        let baseline = baseline
            .or_else(|| {
                catalog
                    .dist_tags
                    .get(&dep.current)
                    .and_then(|v| scheme.parse(v).ok())
            })
            .ok_or_else(|| {
                format!(
                    "{}: cannot classify current value {}",
                    dep.name, dep.current
                )
            })?;
        let policies = [Update::Major, Update::Minor, Update::Patch, Update::Pin]
            .map(|kind| (kind, self.effective(dep, kind)));
        let secured = policies.clone().map(|(kind, mut effective)| {
            effective.apply(
                &self.config.security.apply,
                self.security_permitted.as_ref(),
            );
            (kind, effective)
        });
        // The lowest fix answers to neither `permitted` nor `channel`.
        let unfiltered = secured.clone().map(|(kind, mut effective)| {
            effective.permitted = None;
            effective.policy.channel = patchula_config::ReleaseChannel::Stable;
            (kind, effective)
        });
        let mut fix_candidates: Vec<(Parsed, &Version, String, Update, usize)> = Vec::new();
        let mut best: Vec<(Parsed, &Version, String, Update, &Effective<'_>)> =
            Vec::with_capacity(3);
        for version in &catalog.versions {
            if version.yanked || version.deprecated {
                continue;
            }
            let Some(normalized) = normalize(&version.version) else {
                continue;
            };
            let Ok(parsed) = scheme.parse(&normalized) else {
                continue;
            };
            if scheme.order(&parsed, &baseline) != Ordering::Greater {
                continue;
            }
            if dep.tagged_action()
                && numeric_fields(&dep.current).is_some_and(|n| n < 3)
                && numeric_prefix(&dep.current, &normalized)
            {
                // The same floating alias changes only its digest. Emitting a
                // patch/minor as well duplicates that update and can invent a
                // zero-filled baseline when no full tag matches the pinned SHA.
                continue;
            }
            if scheme == Ecosystem::Maven
                && patchula_versioning::maven_variant(&normalized)
                    != patchula_versioning::maven_variant(
                        baseline.2.as_deref().unwrap_or(&dep.current),
                    )
            {
                // Keep the published variant: 33.x-android never becomes 33.x-jre.
                continue;
            }
            if scheme == Ecosystem::Docker {
                // A floating 3.22 tag tracks the 3.22 line, not 3.22.0.
                // Mixing precisions also turns date tags into bogus majors.
                if parsed.2 != baseline.2
                    || parsed.1.as_ref().map(|v| v.release().len())
                        != baseline.1.as_ref().map(|v| v.release().len())
                {
                    continue;
                }
            }
            let kind = if scheme == Ecosystem::Npm
                && scheme.parse(&dep.current).is_err()
                && self.effective(dep, Update::Pin).policy.versions == VersionPolicy::Exact
                && scheme.matches(&dep.current, &parsed).unwrap_or(false)
            {
                Update::Pin
            } else {
                scheme.classify(&baseline, &parsed)
            };
            let fixed = fixes(&normalized).len();
            if fixed > 0 && self.config.security.lowest_fix {
                fix_candidates.push((parsed.clone(), version, normalized.clone(), kind, fixed));
            }
            let effective = &if fixed == 0 { &policies } else { &secured }
                .iter()
                .find(|(update, _)| *update == kind)
                .expect("version classification")
                .1;
            if !effective.policy.active
                || effective.permitted.is_some_and(|p| !p.matches(&normalized))
            {
                continue;
            }
            if matches!(dep.manager, Manager::Npm | Manager::Terraform)
                && dep.kind == DependencyKind::Toolchain
                && effective.policy.versions == VersionPolicy::Preserve
            {
                // A supported runtime interval is not an installed-version
                // pin. Widening its ceiling needs compatibility evidence.
                continue;
            }
            // Prereleases require a prerelease current or an explicit permitted
            // filter. This permits deliberate nightly rules without a global flag.
            if !parsed.0.pre.is_empty()
                && baseline.0.pre.is_empty()
                && effective.permitted.is_none()
            {
                continue;
            }
            if !eligible(version, &normalized, kind, effective)? {
                continue;
            }
            if let Some(existing) = best.iter_mut().find(|(p, _, _, k, _)| {
                // Retain both a next pre-1.0 compatibility boundary and a
                // numeric-major alternative, even though Cargo calls both major.
                if kind == Update::Pin {
                    *k == Update::Pin
                } else {
                    *k != Update::Pin
                        && Ecosystem::Npm.classify(&baseline, p)
                            == Ecosystem::Npm.classify(&baseline, &parsed)
                }
            }) {
                if candidate_order(scheme, &parsed, version, &existing.0, existing.1).is_gt() {
                    *existing = (parsed, version, normalized, kind, effective);
                }
            } else {
                best.push((parsed, version, normalized, kind, effective));
            }
        }
        // When no proposal fixes as much as some release could, also propose
        // the lowest release that fixes the most.
        let covered = best
            .iter()
            .map(|(_, _, normalized, _, _)| fixes(normalized).len())
            .max()
            .unwrap_or(0);
        if fix_candidates.iter().any(|c| c.4 > covered) {
            fix_candidates.sort_by(|a, b| b.4.cmp(&a.4).then_with(|| scheme.order(&a.0, &b.0)));
            for (parsed, version, normalized, kind, _) in fix_candidates {
                let effective = &unfiltered
                    .iter()
                    .find(|(update, _)| *update == kind)
                    .expect("version classification")
                    .1;
                if !effective.policy.active
                    || (!parsed.0.pre.is_empty() && baseline.0.pre.is_empty())
                    || (matches!(dep.manager, Manager::Npm | Manager::Terraform)
                        && dep.kind == DependencyKind::Toolchain
                        && effective.policy.versions == VersionPolicy::Preserve)
                {
                    continue;
                }
                if eligible(version, &normalized, kind, effective)? {
                    best.push((parsed, version, normalized, kind, effective));
                    break;
                }
            }
        }
        let mut result = Vec::with_capacity(best.len() + 1);
        for (_, version, normalized, kind, effective) in best {
            let new_value = if dep.manager == Manager::GithubActions {
                action_value(
                    &dep.current,
                    if dep.kind == DependencyKind::Tool {
                        &normalized
                    } else {
                        &version.version
                    },
                )
            } else {
                let value = if dep.manager == Manager::Npm
                    && dep.kind == DependencyKind::Peer
                    && effective.policy.versions == VersionPolicy::Widen
                {
                    if scheme
                        .matches(
                            &dep.current,
                            &scheme.parse(&normalized).map_err(|e| e.to_string())?,
                        )
                        .unwrap_or(false)
                    {
                        dep.current.clone()
                    } else {
                        let target = scheme.parse(&normalized).map_err(|e| e.to_string())?;
                        let minor = if target.0.major == 0 {
                            target.0.minor
                        } else {
                            0
                        };
                        let patch = if target.0.major == 0 && minor == 0 {
                            target.0.patch
                        } else {
                            0
                        };
                        format!("{} || ^{}.{}.{}", dep.current, target.0.major, minor, patch)
                    }
                } else if dep.manager == Manager::Terraform
                    && dep.kind == DependencyKind::Provider
                    && scheme
                        .matches(
                            &dep.current,
                            &scheme.parse(&normalized).map_err(|e| e.to_string())?,
                        )
                        .unwrap_or(false)
                {
                    dep.current.clone()
                } else {
                    scheme
                        .new_range(&dep.current, &normalized, effective.policy.versions)
                        .map_err(|e| e.to_string())?
                };
                // Preserve aliases and packageManager identity via extraction
                // location metadata, while current/newValue describe the version.
                value
            };
            if (matches!(dep.manager, Manager::Cargo | Manager::Pyproject)
                || (matches!(dep.manager, Manager::Terraform | Manager::Npm)
                    && dep.kind == DependencyKind::Toolchain))
                && new_value == dep.current
            {
                // A satisfying release does not edit the manifest. Lockfile
                // maintenance is a separate, explicitly enabled operation.
                continue;
            }
            let mut update = proposal(
                Some(new_value),
                Some(normalized),
                if effective.digest && dep.manager == Manager::GithubActions {
                    version.digest.clone()
                } else {
                    None
                },
                kind,
                &effective.policy,
            );
            update.release_age = release_age(version, now);
            update.fixes = fixes(update.new_version.as_deref().unwrap_or_default());
            result.push(update);
        }
        if scheme == Ecosystem::Npm
            && self.effective(dep, Update::Pin).policy.versions == VersionPolicy::Exact
            && scheme.parse(&dep.current).is_err()
        {
            // Pin a range even when its highest satisfying release is unchanged.
            let plain = self.effective(dep, Update::Pin);
            let mut secure = plain.clone();
            secure.apply(
                &self.config.security.apply,
                self.security_permitted.as_ref(),
            );
            let range = nodejs_semver::Range::parse(&dep.current).map_err(|e| e.to_string())?;
            let mut selected: Option<(&Version, Parsed, &Effective<'_>)> = None;
            for v in &catalog.versions {
                let effective = if fixes(&v.version).is_empty() {
                    &plain
                } else {
                    &secure
                };
                if !effective.policy.active
                    || v.deprecated
                    || v.yanked
                    || effective.permitted.is_some_and(|p| !p.matches(&v.version))
                {
                    continue;
                }
                let Ok(parsed) = scheme.parse(&v.version) else {
                    continue;
                };
                let Ok(npm_version) = nodejs_semver::Version::parse(&v.version) else {
                    continue;
                };
                if !range.satisfies(&npm_version) {
                    continue;
                }
                if dep.resolved.as_deref() != Some(v.version.as_str())
                    && !eligible(v, &v.version, Update::Pin, effective)?
                {
                    continue;
                }
                if selected.as_ref().is_none_or(|(previous, p, _)| {
                    candidate_order(scheme, &parsed, v, p, previous).is_gt()
                }) {
                    selected = Some((v, parsed, effective));
                }
            }
            if let Some((v, _, effective)) = selected {
                let value = scheme
                    .new_range(&dep.current, &v.version, VersionPolicy::Exact)
                    .map_err(|e| e.to_string())?;
                if !result
                    .iter()
                    .any(|p| p.new_version.as_deref() == Some(&v.version))
                {
                    let mut update = proposal(
                        Some(value),
                        Some(v.version.clone()),
                        None,
                        Update::Pin,
                        &effective.policy,
                    );
                    update.release_age = release_age(v, now);
                    update.fixes = fixes(&v.version);
                    result.push(update);
                }
            }
        }
        result.sort();
        result.dedup();
        // Only report releases newer than the eligible proposal of their class.
        held_back.retain(|held| {
            let held_version = scheme.parse(&held.version).ok();
            !result
                .iter()
                .filter(|u| u.update_type == held.update_type)
                .filter_map(|u| u.new_version.as_deref().and_then(|v| scheme.parse(v).ok()))
                .any(|v| {
                    held_version
                        .as_ref()
                        .is_some_and(|h| scheme.order(h, &v) != Ordering::Greater)
                })
        });
        held_back.sort();
        held_back.dedup();
        Ok(Evaluation {
            updates: result,
            held_back: summarize(held_back, scheme),
        })
    }
}
fn summarize(mut releases: Vec<HeldBack>, scheme: Ecosystem) -> Vec<HeldBack> {
    releases.sort();
    releases.dedup();
    let mut groups = std::collections::BTreeMap::<String, HeldBack>::new();
    for release in releases {
        if let Some(summary) = groups.get_mut(&release.update_type) {
            if scheme
                .parse(&release.version)
                .ok()
                .zip(scheme.parse(&summary.version).ok())
                .is_some_and(|(a, b)| scheme.order(&a, &b).is_gt())
            {
                summary.version = release.version.clone();
                summary.target_eligible_at = release.target_eligible_at.clone();
            }
            if release.eligible_at < summary.eligible_at {
                summary.eligible_at = release.eligible_at;
                summary.published_at = release.published_at;
            }
            summary.versions.push(release.version);
            summary.count = summary.versions.len();
        } else {
            groups.insert(release.update_type.clone(), release);
        }
    }
    groups.into_values().collect()
}
fn any<T: PartialEq>(values: &[T], value: T) -> bool {
    values.is_empty() || values.contains(&value)
}
fn parse_update(value: &str) -> Update {
    match value {
        "major" => Update::Major,
        "minor" => Update::Minor,
        "patch" => Update::Patch,
        "digest" => Update::Digest,
        _ => Update::Pin,
    }
}
fn proposal(
    value: Option<String>,
    version: Option<String>,
    digest: Option<String>,
    update: Update,
    policy: &Policy,
) -> Proposal {
    Proposal {
        new_value: value,
        new_version: version,
        new_digest: digest,
        update_type: patchula_manager::word(&update),
        group: policy.group.clone().filter(|g| !g.is_empty()),
        policy: Some(UpdatePolicy {
            versions: Some(patchula_manager::word(&policy.versions)),
            channel: Some(patchula_manager::word(&policy.channel)),
            wait_hours: policy.wait_hours,
            windows: policy
                .windows
                .iter()
                .map(|w| Window {
                    cron: w.cron.clone(),
                    zone: w.zone.clone(),
                })
                .collect(),
            merge: policy.merge,
            labels: policy.labels.clone(),
            commit_prefix: policy.commit_prefix.clone(),
            release_notes: policy.release_notes,
            body_notes: policy.body_notes.clone(),
        }),
        release_age: None,
        fixes: Vec::new(),
    }
}
fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}
fn published(version: &Version) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(version.published_at.as_deref().expect("dated release"))
        .map(|v| v.with_timezone(&Utc))
        .map_err(|_| format!("{}: invalid release timestamp", version.version))
}
fn release_age(version: &Version, now: DateTime<Utc>) -> Option<ReleaseAge> {
    version
        .published_at
        .as_ref()
        .map(|published_at| ReleaseAge {
            published_at: published_at.clone(),
            evaluated_at: timestamp(now),
        })
}
/// Resolve an extracted version to its source identity without applying
/// proposal filters. mise lists versions without tag prefixes; the extraction
/// pattern strips them for sources that list raw tags, such as GitHub Actions.
pub fn resolved_tool_version(dep: &Dependency, catalog: &Catalog) -> Option<String> {
    if dep.manager != Manager::Mise
        && !(dep.manager == Manager::GithubActions && dep.kind == DependencyKind::Tool)
    {
        return None;
    }
    let scheme = scheme(dep).ok()?;
    let current_text = dep.resolved.as_deref()?;
    let current = scheme.parse(current_text).ok()?;
    let extract = dep
        .extract
        .as_ref()
        .map(|s| Regex::new(s))
        .transpose()
        .ok()?;
    catalog
        .versions
        .iter()
        .filter_map(|version| {
            let value = if let Some(extract) = &extract {
                extract
                    .captures(&version.version)?
                    .name("version")?
                    .as_str()
                    .to_owned()
            } else {
                version.version.clone()
            };
            let parsed = scheme.parse(&value).ok()?;
            let matches = if numeric_fields(current_text).is_some_and(|n| n < 3) {
                numeric_prefix(current_text, &value) && parsed.0.pre.is_empty()
            } else {
                scheme.order(&parsed, &current).is_eq()
            };
            matches.then_some((parsed, value))
        })
        .max_by(|(a, av), (b, bv)| scheme.order(a, b).then_with(|| av.cmp(bv)))
        .map(|(_, value)| value)
}

// SemVer precedence deliberately ignores build metadata. Break equal-precedence
// ties by the complete source record so pagination/concurrency/cache order can
// never select a different concrete artifact at a fixed evaluation time.
fn candidate_order(
    scheme: Ecosystem,
    a: &Parsed,
    av: &Version,
    b: &Parsed,
    bv: &Version,
) -> Ordering {
    scheme.order(a, b).then_with(|| {
        (&av.version, &av.published_at, &av.digest).cmp(&(
            &bv.version,
            &bv.published_at,
            &bv.digest,
        ))
    })
}

fn numeric_fields(value: &str) -> Option<usize> {
    let fields: Vec<_> = value.trim_start_matches('v').split('.').collect();
    fields
        .iter()
        .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then_some(fields.len())
}

fn numeric_prefix(prefix: &str, version: &str) -> bool {
    let prefix = prefix.trim_start_matches('v');
    let version = version.trim_start_matches('v');
    version == prefix
        || version
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn action_value(current: &str, target: &str) -> String {
    if let Some(n @ 1..=2) = numeric_fields(current) {
        let prefix = if current.starts_with('v') { "v" } else { "" };
        format!(
            "{prefix}{}",
            target
                .trim_start_matches('v')
                .split('.')
                .take(n)
                .collect::<Vec<_>>()
                .join(".")
        )
    } else {
        target.into()
    }
}

/// Resolve floating action aliases by their commit and mise numeric selectors
/// by their release line. Never infer a pinned action's version from a different
/// commit, or use release-age policy to rewrite the observed current version.
pub fn resolved_current_version(dep: &Dependency, catalog: &Catalog) -> Option<String> {
    if matches!(dep.manager, Manager::Terraform | Manager::Npm)
        && dep.kind == DependencyKind::Toolchain
    {
        let scheme = scheme(dep).ok()?;
        return catalog
            .versions
            .iter()
            .filter(|v| !v.yanked && !v.deprecated)
            .filter_map(|v| Some((scheme.parse(&v.version).ok()?, &v.version)))
            .filter(|(parsed, _)| {
                parsed.0.pre.is_empty() && scheme.matches(&dep.current, parsed).unwrap_or(false)
            })
            .max_by(|(a, av), (b, bv)| scheme.order(a, b).then_with(|| av.cmp(bv)))
            .map(|(_, version)| version.trim_start_matches('v').into());
    }
    if dep.manager == Manager::Mise || dep.kind == DependencyKind::Tool {
        return resolved_tool_version(dep, catalog);
    }
    if !dep.tagged_action() {
        return None;
    }
    let current = dep.resolved.as_deref().unwrap_or(&dep.current);
    let n = numeric_fields(current)?;
    if n >= 3 {
        return Some(current.into());
    }
    let digest = dep.digest.as_ref().or_else(|| {
        catalog
            .versions
            .iter()
            .find(|v| v.version == current)
            .and_then(|v| v.digest.as_ref())
    });
    catalog
        .versions
        .iter()
        .filter(|v| {
            numeric_fields(&v.version).is_some_and(|n| n == 3)
                && numeric_prefix(current, &v.version)
                && digest.is_some_and(|d| v.digest.as_ref() == Some(d))
        })
        .filter_map(|v| Some((Ecosystem::Loose.parse(&v.version).ok()?, &v.version)))
        .max_by(|(a, av), (b, bv)| Ecosystem::Loose.order(a, b).then_with(|| av.cmp(bv)))
        .map(|(_, v)| v.clone())
}
pub fn scheme(dep: &Dependency) -> Result<Ecosystem> {
    match dep.scheme {
        Scheme::Semver => Ok(if dep.source == Source::Crates {
            Ecosystem::Cargo
        } else {
            Ecosystem::Npm
        }),
        Scheme::Go => Ok(Ecosystem::Go),
        Scheme::Loose => Ok(Ecosystem::Loose),
        Scheme::Docker => Ok(Ecosystem::Docker),
        Scheme::Pep440 => Ok(Ecosystem::Pep440),
        Scheme::Poetry => Ok(Ecosystem::Poetry),
        Scheme::Maven => Ok(Ecosystem::Maven),
        Scheme::Nuget => Ok(Ecosystem::Nuget),
        Scheme::Rubygems => Ok(Ecosystem::Rubygems),
        Scheme::Composer => Ok(Ecosystem::Composer),
        Scheme::Hashicorp => Ok(Ecosystem::Hashicorp),
    }
}
