//! Ordered repository policy evaluated over looked-up, batch-shared catalogs.
#![forbid(unsafe_code)]
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use regex::Regex;
use rustovate_config::{
    Config, DependencyKind, Manager, Patterns, Policy, Rule, Scheme, Source, Update, VersionPolicy,
};
use rustovate_datasource::{Catalog, Version};
use rustovate_manager::Dependency;
use rustovate_report::{HeldBack, ReleaseAge, Update as Proposal, UpdatePolicy, Window};
use rustovate_versioning::{Ecosystem, Parsed, Versioning};
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
}
#[derive(Clone)]
struct Effective<'a> {
    policy: Policy,
    digest: bool,
    permitted: Option<&'a Filter>,
}
impl Evaluator {
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
        })
    }
    pub fn ignored(&self, dep: &Dependency) -> bool {
        (!self.config.ignore.is_empty() && self.ignore.matches_any(&dep.name, &dep.package))
            || (!self.config.exclude.is_empty() && self.exclude.matches(&dep.file))
    }
    pub fn needs_lookup(&self, dep: &Dependency) -> bool {
        if dep.kind == DependencyKind::Runner && dep.current == "latest" {
            return false;
        }
        if self.ignored(dep)
            || ![Update::Major, Update::Minor, Update::Patch, Update::Pin]
                .into_iter()
                .any(|k| self.effective(dep, k).policy.active)
        {
            return false;
        }
        if dep.manager == Manager::GithubActions && dep.source == Source::GithubTags {
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
        for compiled in &self.rules {
            let s = &compiled.rule.select;
            if !compiled.packages.matches(&dep.package)
                || !compiled.dependencies.matches(&dep.name)
                || !compiled.files.matches(&dep.file)
                || !any(&s.managers, dep.manager)
                || !any(&s.sources, dep.source)
                || !any(&s.kinds, dep.kind)
                || !any(&s.updates, update)
                || compiled
                    .current
                    .as_ref()
                    .is_some_and(|f| !f.matches(dep.resolved.as_deref().unwrap_or(&dep.current)))
            {
                continue;
            }
            let o = &compiled.rule.apply;
            let p = &mut effective.policy;
            macro_rules! replace {($($field:ident),*)=>{$(if let Some(v)=&o.$field {p.$field=v.clone();})*};}
            replace!(
                active,
                versions,
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
                effective.digest = digest;
            }
            if o.permitted.is_some() {
                effective.permitted = compiled.permitted.as_ref();
            }
        }
        effective
    }
    pub fn needs_digest(&self, dep: &Dependency, proposal: &Proposal) -> bool {
        let update = parse_update(&proposal.update_type);
        dep.manager == Manager::GithubActions
            && matches!(
                dep.kind,
                DependencyKind::Action | DependencyKind::ReusableWorkflow
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
        if self.ignored(dep) {
            return Ok(Evaluation::default());
        }
        let mut held_back = Vec::new();
        let mut eligible =
            |version: &Version, kind: Update, effective: &Effective<'_>| -> Result<bool> {
                // The gate chooses proposals; it never removes extracted/current releases.
                if effective.policy.wait_hours == 0 || version.published_at.is_none() {
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
                    version: version.version.clone(),
                    update_type: rustovate_manager::word(&kind),
                    published_at: timestamp(published),
                    eligible_at: timestamp(until),
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
        let current = dep.resolved.as_deref().unwrap_or(&dep.current);
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
                    .max_by(|(a, _), (b, _)| scheme.order(a, b))
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
                candidates.sort_by(|a, b| scheme.order(&b.2, &a.2));
                let mut selected = None;
                for (version, value, _) in candidates {
                    if eligible(version, Update::Pin, &effective)? {
                        selected = Some((version, value));
                        break;
                    }
                }
                let Some((version, target)) = selected else {
                    return Ok(Evaluation {
                        updates: Vec::new(),
                        held_back,
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
                    held_back,
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
        if baseline.is_none() && dep.manager == Manager::GithubActions {
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
        let policies = [Update::Major, Update::Minor, Update::Patch]
            .map(|kind| (kind, self.effective(dep, kind)));
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
            let kind = scheme.classify(&baseline, &parsed);
            let effective = &policies
                .iter()
                .find(|(update, _)| *update == kind)
                .expect("version classification")
                .1;
            if !effective.policy.active
                || effective.permitted.is_some_and(|p| !p.matches(&normalized))
            {
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
            if !eligible(version, kind, effective)? {
                continue;
            }
            if let Some(existing) = best.iter_mut().find(|(_, _, _, k, _)| *k == kind) {
                if scheme.order(&parsed, &existing.0) == Ordering::Greater {
                    *existing = (parsed, version, normalized, kind, effective);
                }
            } else {
                best.push((parsed, version, normalized, kind, effective));
            }
        }
        let mut result = Vec::with_capacity(best.len() + 1);
        for (_, version, normalized, kind, effective) in best {
            let new_value = if dep.manager == Manager::GithubActions {
                version.version.clone()
            } else {
                let value = scheme
                    .new_range(&dep.current, &normalized, effective.policy.versions)
                    .map_err(|e| e.to_string())?;
                // Preserve aliases and packageManager identity via extraction
                // location metadata, while current/newValue describe the version.
                value
            };
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
            result.push(update);
        }
        if scheme == Ecosystem::Npm
            && self.effective(dep, Update::Pin).policy.versions == VersionPolicy::Exact
            && scheme.parse(&dep.current).is_err()
        {
            // Pin a range even when its highest satisfying release is unchanged.
            let effective = self.effective(dep, Update::Pin);
            let range = nodejs_semver::Range::parse(&dep.current).map_err(|e| e.to_string())?;
            let mut selected: Option<(&Version, Parsed)> = None;
            for v in &catalog.versions {
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
                    && !eligible(v, Update::Pin, &effective)?
                {
                    continue;
                }
                if selected
                    .as_ref()
                    .is_none_or(|(_, p)| scheme.order(&parsed, p).is_gt())
                {
                    selected = Some((v, parsed));
                }
            }
            if let Some((v, _)) = selected {
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
                    result.push(update);
                }
            }
        }
        result.sort();
        result.dedup();
        // Only report releases newer than the eligible proposal of their class.
        held_back.retain(|held| {
            let held_version = normalize(&held.version).and_then(|v| scheme.parse(&v).ok());
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
            held_back,
        })
    }
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
        update_type: rustovate_manager::word(&update),
        group: policy.group.clone().filter(|g| !g.is_empty()),
        policy: Some(UpdatePolicy {
            versions: Some(rustovate_manager::word(&policy.versions)),
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
/// Resolve an extracted mise version to its source identity without applying
/// proposal filters. Aqua/Bun/Go extraction strips backend-specific tag prefixes.
pub fn resolved_tool_version(dep: &Dependency, catalog: &Catalog) -> Option<String> {
    if dep.manager != Manager::Mise {
        return None;
    }
    let scheme = scheme(dep).ok()?;
    let current = scheme.parse(dep.resolved.as_deref()?).ok()?;
    let extract = dep
        .extract
        .as_ref()
        .map(|s| Regex::new(s))
        .transpose()
        .ok()?;
    catalog.versions.iter().find_map(|version| {
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
        scheme.order(&parsed, &current).is_eq().then_some(value)
    })
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
        scheme => Err(format!(
            "version scheme {} is not implemented (TODO)",
            rustovate_manager::word(&scheme)
        )),
    }
}
