//! Ordered repository policy evaluated over looked-up, batch-shared catalogs.
#![forbid(unsafe_code)]
mod advisories;
pub use advisories::{advisory_query, affects, fixes, report_advisories};
pub mod images;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use patchula_config::{Config, Filter, Patterns, Policy, Rule};
use patchula_datasource::{Catalog, Version};
use patchula_manager::{Dependency, Range, ShortTags, Written};
use patchula_report::{HeldBack, ReleaseAge, Update as Proposal, UpdatePolicy};
use patchula_versioning::{Ecosystem, Parsed, Versioning};
use patchula_words::{DependencyKind, ReleaseChannel, Source, Update, VersionPolicy};
use regex::Regex;
use std::cmp::Ordering;

pub type Result<T> = std::result::Result<T, String>;
/// When a release published at `published` clears a `wait_hours` release-age
/// gate. An absurd wait holds the release forever rather than erroring.
pub fn eligible_at(published: DateTime<Utc>, wait_hours: u32) -> DateTime<Utc> {
    published
        .checked_add_signed(Duration::hours(i64::from(wait_hours)))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}
#[derive(Default)]
pub struct Evaluation {
    pub updates: Vec<Proposal>,
    pub held_back: Vec<HeldBack>,
    /// Releases that were found and eligible, but whose declaration cannot be
    /// rewritten to allow them without guessing.
    pub not_proposed: Vec<NotProposed>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NotProposed {
    pub version: String,
    pub reason: String,
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
    /// Workflow paths the repository's `actions.lock` lists, when it has
    /// one: the pinning decision reads them.
    locked_workflows: std::collections::BTreeSet<String>,
    rules: Vec<CompiledRule>,
    ignore: Patterns,
    exclude: Patterns,
    permitted: Option<Filter>,
    security_permitted: Option<Filter>,
    /// `dep.extract` patterns, compiled once per distinct pattern.
    extractors: std::sync::Mutex<std::collections::BTreeMap<String, Result<Regex>>>,
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
    /// The committed Cargo cooldown folded into `policy.wait_hours`, beside
    /// the wait the rules asked for: a security fix answers to the rules'
    /// wait alone, and the dashboard's tick narrows the wait to the
    /// cooldown's, never below it.
    floor: Option<(u32, patchula_manager::Cooldown)>,
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
    /// A security fix answers to the wait the rules set, not the committed
    /// cooldown: the vulnerability does not wait the repository's age out.
    fn unfloor(&mut self) {
        if let Some((wait, _)) = self.floor.take() {
            self.policy.wait_hours = wait;
        }
    }
}
impl Evaluator {
    /// Shared catalogs use the shortest active update-class wait. A zero-age
    /// rule keeps normal freshness even if another class waits longer.
    pub fn catalog_wait_hours(&self, dep: &Dependency) -> u32 {
        Update::ALL
            .iter()
            .copied()
            .filter_map(|kind| self.effective(dep, kind).ok())
            .filter(|effective| effective.policy.active)
            .map(|effective| effective.policy.wait_hours)
            .min()
            .unwrap_or(0)
    }
    /// Effective class exclusions, including digest policy, are report evidence.
    pub fn disabled_updates(&self, dep: &Dependency) -> Result<Vec<Update>> {
        let mut disabled = Vec::new();
        for kind in Update::ALL.iter().copied() {
            let effective = self.effective(dep, kind)?;
            if self.ignored(dep)
                || !effective.policy.active
                || (kind == Update::Digest && !effective.digest)
            {
                disabled.push(kind);
            }
        }
        Ok(disabled)
    }
    /// The compiled `extract` pattern for a dependency's `extract` field,
    /// compiled once per distinct pattern. A pattern that fails to compile is
    /// an evaluation error, not a non-match.
    fn extractor(&self, pattern: &str) -> Result<Regex> {
        let mut cache = self.extractors.lock().expect("extractor cache");
        if let Some(compiled) = cache.get(pattern) {
            return compiled.clone();
        }
        let compiled = Regex::new(pattern).map_err(|e| e.to_string());
        cache.insert(pattern.to_owned(), compiled.clone());
        compiled
    }
    pub fn lookup_evidence(
        &self,
        dep: &Dependency,
        catalog: Option<&Catalog>,
        now: DateTime<Utc>,
    ) -> Result<patchula_report::LookupEvidence> {
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        let current = dep.resolved.as_deref().unwrap_or(&dep.current);
        let baseline = scheme.parse(current).ok();
        let extract = dep
            .extract
            .as_ref()
            .map(|pattern| self.extractor(pattern))
            .transpose()?;
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
            if let Some(baseline) = &baseline
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
        Ok(patchula_report::LookupEvidence {
            evaluated_at: timestamp(now),
            disabled_updates: self.disabled_updates(dep)?,
            rejected,
            catalog: None,
        })
    }
    pub fn new(
        config: &Config,
        locked_workflows: std::collections::BTreeSet<String>,
    ) -> Result<Self> {
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
            locked_workflows,
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
            extractors: Default::default(),
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
        if dep.source == Source::Nix {
            // A policy that cannot be evaluated still gets looked up, so the
            // evaluation reports the configuration error.
            return !self.ignored(dep)
                && self
                    .effective(dep, Update::Digest)
                    .map(|effective| effective.policy.active)
                    .unwrap_or(true);
        }
        if self.ignored(dep)
            || ![Update::Major, Update::Minor, Update::Patch, Update::Pin]
                .into_iter()
                .any(|k| {
                    self.effective(dep, k)
                        .map(|effective| effective.policy.active)
                        .unwrap_or(true)
                })
        {
            return false;
        }
        // A branch ref has no version to compare; only tags are looked up.
        if dep.proposing().unversioned.looked_up {
            return Ecosystem::Loose
                .parse(dep.resolved.as_deref().unwrap_or(&dep.current))
                .is_ok();
        }
        true
    }
    /// A forge action's digest comes only from its tag catalog, so the catalog
    /// is needed whenever its tag pin can change, even with version updates off.
    pub fn needs_forge_catalog(&self, dep: &Dependency) -> bool {
        dep.proposing().pin.commit
            && dep.source == Source::ForgeTags
            && !(dep.digest.is_some() && dep.resolved.is_none())
            && Ecosystem::Loose
                .parse(dep.resolved.as_deref().unwrap_or(&dep.current))
                .is_ok()
            && self.digest_policy_applies(dep)
    }
    fn effective(&self, dep: &Dependency, update: Update) -> Result<Effective<'_>> {
        let mut effective = Effective {
            policy: self.config.defaults.clone(),
            digest: if matches!(
                dep.kind,
                DependencyKind::ContainerImage | DependencyKind::Chart
            ) {
                self.config.defaults.image_digests
            } else {
                self.config.defaults.action_digests
            },
            permitted: self.permitted.as_ref(),
            floor: None,
        };
        // Runtime constraints describe supported versions, rather than an
        // installed package version. Explicit rules can still
        // request exact pinning for a repository that wants it.
        if dep.proposing().range == Range::Support {
            effective.policy.versions = VersionPolicy::Preserve;
        }
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        let current = dep.resolved.as_deref().unwrap_or(&dep.current);
        for compiled in &self.rules {
            let s = &compiled.rule.select;
            if !compiled.packages.matches(&dep.package)
                || !compiled.dependencies.matches(&dep.name)
                || !compiled.files.matches(&dep.file)
                || !any(&s.managers, dep.manager)
                || !any(&s.sources, dep.source)
                || !(s.kinds.is_empty() || dep.kinds().iter().any(|kind| s.kinds.contains(kind)))
                || !any(&s.updates, update)
            {
                continue;
            }
            if let Some(filter) = &compiled.current
                && !filter.matches(scheme, current)?
            {
                continue;
            }
            effective.apply(&compiled.rule.apply, compiled.permitted.as_ref());
        }
        // Peers express consumer compatibility, even when installed packages
        // use exact versions. Exact peer declarations still bump in place.
        if dep.proposing().range == Range::Peer {
            effective.policy.versions = if Ecosystem::Npm.parse(&dep.current).is_ok() {
                VersionPolicy::Exact
            } else {
                VersionPolicy::Widen
            };
        }
        // A committed Cargo publish-age cooldown is a floor on the wait the
        // rules set: the effective wait is the larger of the two, and the
        // rules' wait is kept beside it so a security fix or a dashboard
        // tick still answers to it.
        effective.floor = dep.cooldown.clone().map(|cooldown| {
            let wait = effective.policy.wait_hours;
            effective.policy.wait_hours = wait.max(cooldown.hours);
            (wait, cooldown)
        });
        Ok(effective)
    }
    /// Whether a step-level action gets no new SHA pin: its workflow is
    /// one `actions.lock` lists, so the owner's plain `gh actions-lock`
    /// would unpin what the Count pinned. An existing pin still moves
    /// with its update, and the lock does not cover `jobs.<id>.uses`,
    /// so reusable workflows keep pinning.
    fn pin_suppressed(&self, dep: &Dependency) -> bool {
        dep.digest.is_none()
            && dep.kind == DependencyKind::Action
            && self.locked_workflows.contains(&dep.file)
    }
    pub fn needs_digest(&self, dep: &Dependency, proposal: &Proposal) -> bool {
        let update = proposal.update_type;
        dep.proposing().pin.needs_digest
            && !self.pin_suppressed(dep)
            && self
                .effective(dep, update)
                .map(|effective| effective.digest)
                // A policy that cannot be evaluated still gets its digest
                // looked up, so the evaluation reports the error.
                .unwrap_or(true)
    }
    /// Put an image rebuild under `security.apply` once its SBOMs show it
    /// fixes an advisory. Like a package fix, it is refused, and the proposal
    /// left as it was, when `security.apply` turns off updates or digests for
    /// the image or does not permit the tag it moves to.
    pub fn secure(&self, dep: &Dependency, update: &mut Proposal) -> Result<bool> {
        let kind = update.update_type;
        let mut effective = self.effective(dep, kind)?;
        effective.unfloor();
        effective.apply(
            &self.config.security.apply,
            self.security_permitted.as_ref(),
        );
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        let tag = update
            .new_version
            .as_deref()
            .filter(|_| !matches!(kind, Update::Digest | Update::Pin));
        if !effective.policy.active || !effective.digest {
            return Ok(false);
        }
        if let Some(tag) = tag
            && !permitted_matches(effective.permitted, scheme, tag)?
        {
            return Ok(false);
        }
        let secured = proposal(None, None, None, Update::Digest, &effective.policy);
        update.policy = secured.policy;
        update.group = secured.group;
        Ok(true)
    }
    /// The effective policy a digest change answers to, when digest updates
    /// are on for it: `Digest` when the dependency already pins one, `Pin`
    /// otherwise.
    fn digest_effective(&self, dep: &Dependency) -> Result<Option<(Update, Effective<'_>)>> {
        if self.pin_suppressed(dep) {
            return Ok(None);
        }
        let update = if dep.digest.is_some() {
            Update::Digest
        } else {
            Update::Pin
        };
        let effective = self.effective(dep, update)?;
        Ok((effective.policy.active && effective.digest).then_some((update, effective)))
    }
    /// Whether proposing a digest change for `dep` would pass policy: the
    /// dependency is not ignored, is not a runner, and its digest update class
    /// is active with digests enabled.
    pub fn digest_policy_applies(&self, dep: &Dependency) -> bool {
        !self.ignored(dep)
            && dep.kind != DependencyKind::Runner
            && self
                .digest_effective(dep)
                .map(|effective| effective.is_some())
                // A policy that cannot be evaluated still gets its digest
                // looked up, so the evaluation reports the error.
                .unwrap_or(true)
    }
    pub fn digest_proposal(&self, dep: &Dependency, new_digest: &str) -> Result<Option<Proposal>> {
        if self.ignored(dep)
            || dep.kind == DependencyKind::Runner
            || dep.digest.as_deref() == Some(new_digest)
        {
            return Ok(None);
        }
        let Some((update, effective)) = self.digest_effective(dep)? else {
            return Ok(None);
        };
        Ok(Some(proposal(
            dep.resolved.clone().or_else(|| Some(dep.current.clone())),
            dep.resolved.clone(),
            Some(new_digest.into()),
            update,
            &effective.policy,
        )))
    }
    pub fn digest_proposal_with_catalog(
        &self,
        dep: &Dependency,
        new_digest: &str,
        catalog: Option<&Catalog>,
    ) -> Result<Option<Proposal>> {
        let Some(mut proposal) = self.digest_proposal(dep, new_digest)? else {
            return Ok(None);
        };
        if dep.proposing().pin.commit
            && let Some(catalog) = catalog
        {
            let mut target = dep.clone();
            target.digest = Some(new_digest.into());
            if let Some(version) = self.resolved_current_version(&target, catalog)? {
                proposal.new_version = Some(version);
            }
        }
        Ok(Some(proposal))
    }
    /// The newest non-prerelease release a floating `latest` tool tag resolves
    /// to, under the Pin policy's `permitted` filter.
    fn latest_target(
        &self,
        dep: &Dependency,
        catalog: &Catalog,
        scheme: Ecosystem,
        normalize: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<String>> {
        let effective = self.effective(dep, Update::Pin)?;
        let mut best: Option<(Parsed, String)> = None;
        for version in &catalog.versions {
            if version.yanked || version.deprecated {
                continue;
            }
            let Some(value) = normalize(&version.version) else {
                continue;
            };
            let Ok(parsed) = scheme.parse(&value) else {
                continue;
            };
            if !parsed.0.pre.is_empty() || !permitted_matches(effective.permitted, scheme, &value)?
            {
                continue;
            }
            let newer = best
                .as_ref()
                .is_none_or(|(b, bv)| scheme.order(&parsed, b).then_with(|| value.cmp(bv)).is_gt());
            if newer {
                best = Some((parsed, value));
            }
        }
        Ok(best.map(|(_, value)| value))
    }
    /// The release-age wait a Cargo lockfile job holds crates it newly locks
    /// to when no update names them — the crates a `--precise` update
    /// resolves past, and everything lockfile maintenance re-resolves: the
    /// strictest effective wait the rules give the repository's crates.io
    /// dependencies, over every update class.
    pub fn lockfile_wait_hours(&self, deps: &[Dependency]) -> u32 {
        deps.iter()
            .filter(|dep| dep.source == Source::Crates)
            .flat_map(|dep| Update::ALL.iter().copied().map(move |kind| (dep, kind)))
            .filter_map(|(dep, kind)| self.effective(dep, kind).ok())
            .filter(|effective| effective.policy.active)
            .map(|effective| effective.policy.wait_hours)
            .max()
            .unwrap_or(0)
    }
    /// The wait a rebuilt routine Cargo job holds the crates it newly locks
    /// to: the effective wait the rules give `dep` for the update's own
    /// kind — a batched minor waits as a minor, not the strictest wait any
    /// kind carries — as `security_wait_hours` answers for a fix.
    pub fn routine_wait_hours(&self, dep: &Dependency, kind: Update) -> u32 {
        self.effective(dep, kind)
            .map(|effective| effective.policy.wait_hours)
            .unwrap_or(0)
    }
    /// The wait a security fix of `kind` for `dep` runs under in Cargo:
    /// `security.apply` over the rules, with the Cargo cooldown floor lifted
    /// as it is for every fix. A batch's rebuilt update records no policy,
    /// so its fix job takes this rather than the strictest routine wait,
    /// which Cargo would hold a fresh fix against.
    pub fn security_wait_hours(&self, dep: &Dependency, kind: Update) -> u32 {
        self.effective(dep, kind)
            .map(|mut effective| {
                effective.unfloor();
                effective.apply(
                    &self.config.security.apply,
                    self.security_permitted.as_ref(),
                );
                effective.policy.wait_hours
            })
            .unwrap_or(0)
    }
    /// A lockfile-only package's one update: the lowest fixed release that
    /// fixes the most of its advisories without a breaking change, under
    /// `security.apply`. Only Cargo locks can be moved to an exact version of
    /// one package. A dated target still inside `security.apply`'s wait is
    /// held back, as the direct path holds its targets, rather than proposed
    /// so Cargo refuses it every run.
    pub fn transitive_fix(
        &self,
        dep: &Dependency,
        advisories: Advisories<'_>,
        catalog: Option<&Catalog>,
        now: DateTime<Utc>,
    ) -> Result<Evaluation> {
        let (query, found) = advisories;
        if !dep.proposing().lock.transitive_movable || self.ignored(dep) {
            return Ok(Evaluation::default());
        }
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        let Ok(current) = scheme.parse(&dep.current) else {
            return Ok(Evaluation::default());
        };
        let fixed = found
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
            .map(|(_, v, fixes)| (v, fixes));
        let Some((target, fixes)) = fixed else {
            return Ok(Evaluation::default());
        };
        let Ok(target_parsed) = scheme.parse(&target) else {
            return Ok(Evaluation::default());
        };
        let kind = scheme.classify(&current, &target_parsed);
        let mut effective = self.effective(dep, kind)?;
        effective.unfloor();
        effective.apply(
            &self.config.security.apply,
            self.security_permitted.as_ref(),
        );
        if !effective.policy.active {
            return Ok(Evaluation::default());
        }
        let released = catalog.and_then(|catalog| {
            catalog
                .versions
                .iter()
                .find(|version| version.version == target)
        });
        // The fix answers to `security.apply`'s wait as a direct target does
        // to its own, so the proposal and Cargo's `deny` agree instead of the
        // branch failing each run until the fix ages.
        let wait = effective.policy.wait_hours;
        if wait > 0
            && let Some(version) = released.filter(|version| version.published_at.is_some())
        {
            let published = published(version)?;
            let until = eligible_at(published, wait);
            if until > now {
                return Ok(Evaluation {
                    updates: Vec::new(),
                    held_back: vec![HeldBack {
                        version: target.clone(),
                        versions: vec![target],
                        count: 1,
                        update_type: kind,
                        published_at: timestamp(published),
                        eligible_at: timestamp(until),
                        target_eligible_at: Some(timestamp(until)),
                        cooldown: None,
                    }],
                    not_proposed: Vec::new(),
                });
            }
        }
        let mut update = proposal(None, Some(target.clone()), None, kind, &effective.policy);
        update.fixes = fixes;
        // The runner moves the transitive package by Cargo's own resolve:
        // `allow` waives the check for every crate the resolve touches, so
        // the fix is marked only when the catalog dates its target inside
        // the repository's cooldown, like the direct-fix paths. A target
        // the catalog cannot date is denied like any other release.
        update.cooldown = inside_cooldown(
            dep,
            released.and_then(|version| version.published_at.as_deref()),
            now,
        );
        Ok(Evaluation {
            updates: vec![update],
            ..Default::default()
        })
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
        // A lockfile-only package never takes part in routine evaluation
        // (`needs_lookup` is false for it): the catalog its advisories fetch
        // exists to date `transitive_fix`'s target, nothing else.
        if dep.kind == DependencyKind::Transitive {
            return Ok(Evaluation::default());
        }
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        let mut held_back = Vec::new();
        let mut not_proposed = Vec::new();
        let mut eligible = |version: &Version,
                            selected: &str,
                            kind: Update,
                            effective: &Effective<'_>|
         -> Result<bool> {
            if effective.policy.channel == ReleaseChannel::Lts {
                let lts = catalog
                    .dist_tags
                    .get("lts")
                    .and_then(|v| scheme.parse(v).ok())
                    .ok_or_else(|| "catalog has no LTS channel evidence".to_owned())?;
                if !scheme
                    .parse(selected)
                    .is_ok_and(|v| v.0.major == lts.0.major)
                {
                    return Ok(false);
                }
            }
            // The gate chooses proposals; it never removes extracted/current releases.
            // A dashboard tick narrows the wait to a committed cooldown's:
            // what the repository commits, the Count cannot waive.
            let gated = overrides.contains(&(selected.into(), kind.to_string()));
            let wait = if gated {
                effective.floor.as_ref().map_or(0, |(_, c)| c.hours)
            } else {
                effective.policy.wait_hours
            };
            if wait == 0 || version.published_at.is_none() {
                return Ok(true);
            }
            let published = published(version)?;
            let until = eligible_at(published, wait);
            if until <= now {
                return Ok(true);
            }
            held_back.push(HeldBack {
                version: selected.into(),
                versions: vec![selected.into()],
                count: 1,
                update_type: kind,
                published_at: timestamp(published),
                eligible_at: timestamp(until),
                target_eligible_at: Some(timestamp(until)),
                // A hold reports the cooldown only when the floor decides
                // the wait: always once ticked, otherwise when the cooldown
                // is the longer of the two.
                cooldown: effective
                    .floor
                    .as_ref()
                    .filter(|(rules, cooldown)| gated || cooldown.hours >= *rules)
                    .map(|(_, cooldown)| patchula_report::Cooldown {
                        hours: cooldown.hours,
                        file: cooldown.file.clone(),
                    }),
            });
            Ok(false)
        };
        // A flake input follows whatever its reference resolves to now, so its
        // only update is a new locked revision, dated by that commit.
        if dep.source == Source::Nix {
            let effective = self.effective(dep, Update::Digest)?;
            let Some((version, digest)) = catalog
                .versions
                .first()
                .and_then(|v| Some((v, v.digest.clone()?)))
                .filter(|(_, digest)| {
                    effective.policy.active && dep.digest.as_deref() != Some(digest)
                })
            else {
                return Ok(Evaluation::default());
            };
            if !eligible(version, &digest, Update::Digest, &effective)? {
                return Ok(Evaluation {
                    updates: Vec::new(),
                    held_back,
                    ..Default::default()
                });
            }
            let mut update = proposal(None, None, Some(digest), Update::Digest, &effective.policy);
            update.release_age = release_age(version, now);
            return Ok(Evaluation {
                updates: vec![update],
                held_back,
                ..Default::default()
            });
        }
        let extractor = dep
            .extract
            .as_ref()
            .map(|pattern| self.extractor(pattern))
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
        let resolved = self.resolved_current_version(dep, catalog)?;
        let current = resolved
            .as_deref()
            .or(dep.resolved.as_deref())
            .unwrap_or(&dep.current);
        let current = normalize(current).unwrap_or_else(|| current.into());
        // Dist-tags and mise's latest are intentionally floating values. Exact
        // policy can pin them without pretending a resolved version was known.
        let latest = if dep.kind == DependencyKind::Tool && dep.current == "latest" {
            self.latest_target(dep, catalog, scheme, &normalize)?
        } else {
            None
        };
        let tag_target = catalog
            .dist_tags
            .get(&dep.current)
            .and_then(|v| normalize(v))
            .or(latest);
        if let Some(target) = tag_target {
            let effective = self.effective(dep, Update::Pin)?;
            if effective.policy.active
                && effective.policy.versions == VersionPolicy::Exact
                && permitted_matches(effective.permitted, scheme, &target)?
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
                    .filter(|(_, _, p)| {
                        scheme.order(p, &ceiling) != Ordering::Greater
                            && (p.0.pre.is_empty() || !ceiling.0.pre.is_empty())
                    })
                    .collect();
                candidates.sort_by(|a, b| candidate_order(scheme, &b.2, b.0, &a.2, a.0));
                let mut selected = None;
                for (version, value, _) in candidates {
                    if !permitted_matches(effective.permitted, scheme, &value)? {
                        continue;
                    }
                    if eligible(version, &value, Update::Pin, &effective)? {
                        selected = Some((version, value));
                        break;
                    }
                }
                let Some((version, target)) = selected else {
                    return Ok(Evaluation {
                        updates: Vec::new(),
                        held_back: summarize(held_back, scheme),
                        ..Default::default()
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
                    ..Default::default()
                });
            }
            return Ok(Evaluation::default());
        }
        let baseline = scheme
            .parse(&current)
            .ok()
            .or_else(|| scheme.floor(&dep.current, &current));
        // Branch and opaque refs are preserved. Their digest is proposed through
        // digest_proposal after the engine resolves that same ref.
        if baseline.is_none() && (dep.proposing().unversioned.opaque || scheme == Ecosystem::Docker)
        {
            return Ok(Evaluation::default());
        }
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
            .into_iter()
            .map(|kind| self.effective(dep, kind).map(|effective| (kind, effective)))
            .collect::<Result<Vec<_>>>()?;
        let secured = policies
            .iter()
            .cloned()
            .map(|(kind, mut effective)| {
                effective.unfloor();
                effective.apply(
                    &self.config.security.apply,
                    self.security_permitted.as_ref(),
                );
                (kind, effective)
            })
            .collect::<Vec<_>>();
        // The lowest fix answers to neither `permitted` nor `channel`.
        let unfiltered = secured
            .iter()
            .cloned()
            .map(|(kind, mut effective)| {
                effective.permitted = None;
                effective.policy.channel = patchula_words::ReleaseChannel::Stable;
                (kind, effective)
            })
            .collect::<Vec<_>>();
        let mut fix_candidates: Vec<(Parsed, &Version, String, Update, usize)> = Vec::new();
        let mut best: Vec<(Parsed, &Version, String, Update, &Effective<'_>)> =
            Vec::with_capacity(3);
        // Eligible releases a track's best displaced, kept in case its declaration
        // cannot be rewritten for the best one.
        let mut alternates: Vec<(Parsed, &Version, String, Update, &Effective<'_>)> = Vec::new();
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
            // Only Actions float a short tag such as v4 along its release line;
            // an APM tag names one release.
            if dep.proposing().short_tags == ShortTags::Floating
                && numeric_fields(&dep.current).is_some_and(|n| n < 3)
                && numeric_prefix(&dep.current, &normalized)
            {
                // The same floating alias changes only its digest. Emitting a
                // patch/minor as well duplicates that update and can invent a
                // zero-filled baseline when no full tag matches the pinned SHA.
                continue;
            }
            if !scheme.same_release_line(
                &baseline,
                baseline.2.as_deref().unwrap_or(&dep.current),
                &parsed,
                &normalized,
            ) {
                continue;
            }
            let kind = if scheme == Ecosystem::Npm
                && scheme.parse(&dep.current).is_err()
                && self.effective(dep, Update::Pin)?.policy.versions == VersionPolicy::Exact
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
                || !permitted_matches(effective.permitted, scheme, &normalized)?
            {
                continue;
            }
            if dep.proposing().range == Range::Support
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
                let candidate = (parsed, version, normalized, kind, effective);
                if candidate_order(scheme, &candidate.0, version, &existing.0, existing.1).is_gt() {
                    alternates.push(std::mem::replace(existing, candidate));
                } else {
                    alternates.push(candidate);
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
                    || (dep.proposing().range == Range::Support
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
        // Each track's best release, then the lower ones it displaced, newest first.
        let chains: Vec<Vec<_>> = best
            .into_iter()
            .map(|top| {
                let mut lower: Vec<_> = alternates
                    .iter()
                    .filter(|alt| {
                        if alt.3 == Update::Pin {
                            top.3 == Update::Pin
                        } else {
                            top.3 != Update::Pin
                                && Ecosystem::Npm.classify(&baseline, &top.0)
                                    == Ecosystem::Npm.classify(&baseline, &alt.0)
                        }
                    })
                    .filter(|alt| candidate_order(scheme, &alt.0, alt.1, &top.0, top.1).is_lt())
                    .cloned()
                    .collect();
                lower.sort_by(|a, b| candidate_order(scheme, &b.0, b.1, &a.0, a.1));
                std::iter::once(top).chain(lower).collect()
            })
            .collect();
        'tracks: for chain in chains {
            let mut reported = false;
            for (_, version, normalized, kind, effective) in chain {
                let new_value = match dep.proposing().written {
                    Written::TagAtPrecision => action_value(&dep.current, &version.version),
                    Written::VersionAtPrecision => action_value(&dep.current, &normalized),
                    Written::Tag => version.version.clone(),
                    Written::Range => {
                        let value = if dep.proposing().range == Range::Peer
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
                                let target =
                                    scheme.parse(&normalized).map_err(|e| e.to_string())?;
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
                                format!(
                                    "{} || ^{}.{}.{}",
                                    dep.current, target.0.major, minor, patch
                                )
                            }
                        } else if dep.proposing().keeps_satisfied
                            && scheme
                                .matches(
                                    &dep.current,
                                    &scheme.parse(&normalized).map_err(|e| e.to_string())?,
                                )
                                .unwrap_or(false)
                        {
                            dep.current.clone()
                        } else {
                            match scheme.new_range(
                                &dep.current,
                                &normalized,
                                effective.policy.versions,
                            ) {
                                Ok(value) => value,
                                // The lookup worked; the declaration is what cannot be edited.
                                Err(patchula_versioning::Error::Compound(_)) => {
                                    // Report the newest release once, then try the older
                                    // ones the same track displaced.
                                    if !reported {
                                        reported = true;
                                        not_proposed.push(NotProposed {
                                    version: normalized,
                                    reason: format!(
                                        "the constraint `{}` cannot be rewritten to allow it without guessing",
                                        dep.current
                                    ),
                                });
                                    }
                                    continue;
                                }
                                Err(e) => return Err(e.to_string()),
                            }
                        };
                        // Preserve aliases and packageManager identity via extraction
                        // location metadata, while current/newValue describe the version.
                        value
                    }
                };
                // A release the declared range already allows leaves the manifest
                // alone. With a committed lockfile it still moves the lock to the
                // target; without one there is nothing to change.
                let in_range = new_value == dep.current;
                let lock_only = in_range
                    && dep
                        .proposing()
                        .lock
                        .locked(dep.resolved.as_deref())
                        .is_some()
                    && effective.policy.versions != VersionPolicy::Exact;
                if in_range
                    && !lock_only
                    && (dep.proposing().lock.keeps_ranges
                        || matches!(scheme, Ecosystem::Maven | Ecosystem::Nuget)
                        || dep.proposing().runtime_toolchain)
                {
                    // Lockfile maintenance is a separate, explicitly enabled operation.
                    continue 'tracks;
                }
                let mut update = proposal(
                    (!lock_only).then_some(new_value),
                    Some(normalized),
                    if effective.digest && dep.proposing().pin.from_catalog {
                        version.digest.clone()
                    } else {
                        None
                    },
                    kind,
                    &effective.policy,
                );
                update.release_age = release_age(version, now);
                update.fixes = fixes(update.new_version.as_deref().unwrap_or_default());
                // A fix inside the repository's cooldown was held by nothing;
                // the report names the hold it bypassed so the lockfile run
                // lets Cargo take it.
                update.cooldown = inside_cooldown(dep, version.published_at.as_deref(), now);
                result.push(update);
                break;
            }
        }
        if scheme == Ecosystem::Npm
            && self.effective(dep, Update::Pin)?.policy.versions == VersionPolicy::Exact
            && scheme.parse(&dep.current).is_err()
        {
            // Pin a range even when its highest satisfying release is unchanged.
            let plain = self.effective(dep, Update::Pin)?;
            let mut secure = plain.clone();
            secure.unfloor();
            secure.apply(
                &self.config.security.apply,
                self.security_permitted.as_ref(),
            );
            // A declared value that is not an npm range at all is an
            // evaluation error, before the candidates are looked at.
            scheme
                .satisfies(&dep.current, "0.0.0")
                .map_err(|e| e.to_string())?;
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
                    || !permitted_matches(effective.permitted, scheme, &v.version)?
                {
                    continue;
                }
                let Ok(parsed) = scheme.parse(&v.version) else {
                    continue;
                };
                if !scheme
                    .satisfies(&dep.current, &v.version)
                    .map_err(|e| e.to_string())?
                {
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
                    update.cooldown = inside_cooldown(dep, v.published_at.as_deref(), now);
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
        not_proposed.sort();
        not_proposed.dedup();
        Ok(Evaluation {
            updates: result,
            held_back: summarize(held_back, scheme),
            not_proposed,
        })
    }
}
/// The committed cooldown a proposed target still sits inside, when `dep`
/// carries one: only a security fix the floor did not hold can be marked —
/// anything else inside the cooldown is held back, not proposed.
fn inside_cooldown(
    dep: &Dependency,
    published_at: Option<&str>,
    now: DateTime<Utc>,
) -> Option<patchula_report::Cooldown> {
    let cooldown = dep.cooldown.as_ref()?;
    let inside = published_at
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .is_some_and(|published| eligible_at(published.with_timezone(&Utc), cooldown.hours) > now);
    inside.then(|| patchula_report::Cooldown {
        hours: cooldown.hours,
        file: cooldown.file.clone(),
    })
}
fn summarize(mut releases: Vec<HeldBack>, scheme: Ecosystem) -> Vec<HeldBack> {
    releases.sort();
    releases.dedup();
    // Keyed by word, not the enum, so classes come out in the order reports
    // have always written them.
    let mut groups = std::collections::BTreeMap::<&'static str, HeldBack>::new();
    for release in releases {
        if let Some(summary) = groups.get_mut(release.update_type.as_str()) {
            if scheme
                .parse(&release.version)
                .ok()
                .zip(scheme.parse(&summary.version).ok())
                .is_some_and(|(a, b)| scheme.order(&a, &b).is_gt())
            {
                summary.version.clone_from(&release.version);
                summary
                    .target_eligible_at
                    .clone_from(&release.target_eligible_at);
            }
            if release.eligible_at < summary.eligible_at {
                summary.eligible_at = release.eligible_at;
                summary.published_at = release.published_at;
            }
            if summary.cooldown.is_none() {
                summary.cooldown = release.cooldown;
            }
            summary.versions.push(release.version);
            summary.count = summary.versions.len();
        } else {
            groups.insert(release.update_type.as_str(), release);
        }
    }
    groups.into_values().collect()
}
fn any<T: PartialEq>(values: &[T], value: T) -> bool {
    values.is_empty() || values.contains(&value)
}
/// Whether `text` passes the `permitted` filter, when one is set.
fn permitted_matches(permitted: Option<&Filter>, scheme: Ecosystem, text: &str) -> Result<bool> {
    match permitted {
        Some(filter) => filter.matches(scheme, text),
        None => Ok(true),
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
        update_type: update,
        group: policy.group.clone().filter(|g| !g.is_empty()),
        policy: Some(UpdatePolicy {
            versions: Some(policy.versions),
            channel: Some(policy.channel),
            wait_hours: policy.wait_hours,
            windows: policy.windows.clone(),
            merge: policy.merge,
            labels: policy.labels.clone(),
            commit_prefix: policy.commit_prefix.clone(),
            release_notes: policy.release_notes,
            body_notes: policy.body_notes.clone(),
        }),
        release_age: None,
        fixes: Vec::new(),
        image: None,
        cooldown: None,
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
impl Evaluator {
    /// Resolve an extracted version to its source identity without applying
    /// proposal filters. mise lists versions without tag prefixes; the
    /// extraction pattern strips them for sources that list raw tags, such as
    /// GitHub Actions.
    pub fn resolved_tool_version(
        &self,
        dep: &Dependency,
        catalog: &Catalog,
    ) -> Result<Option<String>> {
        if !dep.proposing().selector_resolved {
            return Ok(None);
        }
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        let Some(current_text) = dep.resolved.as_deref() else {
            return Ok(None);
        };
        let Ok(current) = scheme.parse(current_text) else {
            return Ok(None);
        };
        let extract = dep
            .extract
            .as_ref()
            .map(|pattern| self.extractor(pattern))
            .transpose()?;
        Ok(catalog
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
            .map(|(_, value)| value))
    }
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

impl Evaluator {
    /// Resolve floating action aliases by their commit and mise numeric
    /// selectors by their release line. Never infer a pinned action's version
    /// from a different commit, or use release-age policy to rewrite the
    /// observed current version.
    pub fn resolved_current_version(
        &self,
        dep: &Dependency,
        catalog: &Catalog,
    ) -> Result<Option<String>> {
        if dep.proposing().runtime_toolchain {
            let scheme = Ecosystem::of(dep.scheme, dep.source);
            return Ok(catalog
                .versions
                .iter()
                .filter(|v| !v.yanked && !v.deprecated)
                .filter_map(|v| Some((scheme.parse(&v.version).ok()?, &v.version)))
                .filter(|(parsed, _)| {
                    parsed.0.pre.is_empty() && scheme.matches(&dep.current, parsed).unwrap_or(false)
                })
                .max_by(|(a, av), (b, bv)| scheme.order(a, b).then_with(|| av.cmp(bv)))
                .map(|(_, version)| version.trim_start_matches('v').into()));
        }
        if dep.proposing().resolved_via_listing {
            return self.resolved_tool_version(dep, catalog);
        }
        if dep.proposing().short_tags != ShortTags::Floating {
            return Ok(None);
        }
        let current = dep.resolved.as_deref().unwrap_or(&dep.current);
        let Some(n) = numeric_fields(current) else {
            return Ok(None);
        };
        if n >= 3 {
            return Ok(Some(current.into()));
        }
        let digest = dep.digest.as_ref().or_else(|| {
            catalog
                .versions
                .iter()
                .find(|v| v.version == current)
                .and_then(|v| v.digest.as_ref())
        });
        Ok(catalog
            .versions
            .iter()
            .filter(|v| {
                numeric_fields(&v.version).is_some_and(|n| n == 3)
                    && numeric_prefix(current, &v.version)
                    && digest.is_some_and(|d| v.digest.as_ref() == Some(d))
            })
            .filter_map(|v| Some((Ecosystem::Loose.parse(&v.version).ok()?, &v.version)))
            .max_by(|(a, av), (b, bv)| Ecosystem::Loose.order(a, b).then_with(|| av.cmp(bv)))
            .map(|(_, v)| v.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_words::Manager;

    /// `extractor` caches the compiled pattern per distinct `extract` field:
    /// two lookups with the same pattern compile it once.
    #[test]
    fn an_extract_pattern_is_compiled_once_across_lookups() {
        let config = patchula_config::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/benchmark-public.pkl"),
        )
        .unwrap();
        let eval = Evaluator::new(&config, Default::default()).unwrap();
        let dep = |extract: &str| patchula_manager::Dependency {
            manager: Manager::Npm,
            file: "package.json".into(),
            location: patchula_manager::Location {
                start: 0,
                end: 1,
                line: 1,
                column: 1,
                pointer: String::new(),
            },
            form: patchula_manager::EditForm::Scalar(patchula_manager::Syntax::Json),
            name: "lib".into(),
            package: "lib".into(),
            kind: DependencyKind::Production,
            current: "1.0.0".into(),
            resolved: Some("1.0.0".into()),
            digest: None,
            source: Source::Npm,
            scheme: patchula_words::Scheme::Semver,
            extract: Some(extract.into()),
            included_by: None,
            registry: None,
            cooldown: None,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-12T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for _ in 0..2 {
            assert!(
                eval.lookup_evidence(&dep("^v(?P<version>.+)$"), None, now)
                    .is_ok()
            );
        }
        assert_eq!(eval.extractors.lock().unwrap().len(), 1);
        assert!(
            eval.lookup_evidence(&dep("^w(?P<version>.+)$"), None, now)
                .is_ok()
        );
        assert_eq!(eval.extractors.lock().unwrap().len(), 2);
    }

    /// A step action gets no new SHA pin when its workflow is one
    /// `actions.lock` lists: the owner's `gh actions-lock` would unpin it.
    /// An existing pin still moves with its update, and reusable workflows
    /// (`jobs.<id>.uses`), which the lock does not cover, keep pinning, as
    /// does a step action in a workflow the lock does not list.
    #[test]
    fn locked_workflows_suppress_only_new_step_action_pins() {
        let config = patchula_config::load_files(
            "/repo/patchula.pkl",
            std::collections::BTreeMap::from([(
                "/repo/patchula.pkl".into(),
                "amends \"PatchulaConfig.pkl\"\n".to_owned(),
            )]),
        )
        .unwrap();
        let dep =
            |file: &str, kind: DependencyKind, digest: Option<&str>| patchula_manager::Dependency {
                manager: Manager::GithubActions,
                file: file.into(),
                location: patchula_manager::Location {
                    start: 0,
                    end: 1,
                    line: 1,
                    column: 1,
                    pointer: String::new(),
                },
                form: patchula_manager::EditForm::Scalar(patchula_manager::Syntax::Yaml),
                name: "actions/checkout".into(),
                package: "actions/checkout".into(),
                kind,
                current: "v7.0.0".into(),
                resolved: Some("v7.0.0".into()),
                digest: digest.map(|d| d.into()),
                source: Source::GithubTags,
                scheme: patchula_words::Scheme::Semver,
                extract: None,
                included_by: None,
                registry: None,
                cooldown: None,
            };
        let locked = std::collections::BTreeSet::from([".github/workflows/ci.yml".to_owned()]);
        let eval = Evaluator::new(&config, locked).unwrap();
        // Suppressed: new pin on a step action in a listed workflow.
        let action = dep(".github/workflows/ci.yml", DependencyKind::Action, None);
        assert!(!eval.digest_policy_applies(&action));
        assert!(eval.digest_proposal(&action, "sha1-x").unwrap().is_none());
        // Kept: an existing pin still gets digest updates.
        let pinned = dep(
            ".github/workflows/ci.yml",
            DependencyKind::Action,
            Some("sha1-old"),
        );
        assert!(eval.digest_policy_applies(&pinned));
        // Kept: a reusable workflow in the same file still pins.
        let reusable = dep(
            ".github/workflows/ci.yml",
            DependencyKind::ReusableWorkflow,
            None,
        );
        assert!(eval.digest_policy_applies(&reusable));
        // Kept: a step action in an unlisted workflow still pins.
        let other = dep(".github/workflows/other.yml", DependencyKind::Action, None);
        assert!(eval.digest_policy_applies(&other));
    }
}
