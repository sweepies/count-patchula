//! A deterministic issue, discovered by ownership and marker rather than title.
use crate::{
    Inventory, Plan, Recheck,
    plan::hash,
    text::{self, code},
    update_key,
};
use patchula_cache::{Cache, ExtractionKey};
use patchula_config::Config;
use patchula_github::Client;
use patchula_report::{Dependency, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MARKER: &str = "<!-- patchula-dashboard:v1 -->";
const TITLE: &str = "Count Patchula's dependency dashboard";
const LIMIT: usize = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "action",
    content = "target",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum Action {
    Recheck(String),
    Create(String),
    Recreate(String),
    Retry(String),
}
pub fn dependency_key(file: &str, dep: &Dependency) -> String {
    hash(
        serde_json::to_string(&(
            file,
            &dep.manager,
            &dep.name,
            &dep.package,
            dep.metadata
                .as_ref()
                .map(|m| m.source.as_str())
                .unwrap_or(""),
            &dep.current.value,
            &dep.current.digest,
        ))
        .expect("dependency identity")
        .as_bytes(),
    )
}
pub fn extracted_key(dep: &patchula_manager::Dependency) -> String {
    hash(
        serde_json::to_string(&(
            &dep.file,
            patchula_manager::word(&dep.manager),
            &dep.name,
            &dep.package,
            patchula_manager::word(&dep.source),
            Some(&dep.current),
            &dep.digest,
        ))
        .expect("dependency identity")
        .as_bytes(),
    )
}
/// The key of a retry box: one package from one source, whichever files
/// declare it. A lookup is shared by all of them.
pub fn retry_key(manager: &str, package: &str, source: &str) -> String {
    hash(
        serde_json::to_string(&("retry", manager, package, source))
            .expect("retry identity")
            .as_bytes(),
    )
}
pub fn extracted_retry_key(dep: &patchula_manager::Dependency) -> String {
    retry_key(
        &patchula_manager::word(&dep.manager),
        &dep.package,
        &patchula_manager::word(&dep.source),
    )
}
pub fn held_target(dependency: &str, version: &str, kind: &str) -> String {
    hash(
        serde_json::to_string(&(dependency, version, kind))
            .expect("held identity")
            .as_bytes(),
    )
}
pub fn held_key(file: &str, dep: &Dependency, version: &str, kind: &str) -> String {
    held_target(&dependency_key(file, dep), version, kind)
}

#[derive(Clone, Debug, Default)]
pub struct Controls {
    pub recheck: bool,
    pub branches: BTreeSet<String>,
    pub create: BTreeSet<String>,
    pub recreate: BTreeSet<String>,
    pub retry: BTreeSet<String>,
    /// Accepted release-age targets survive unticking and later process restarts.
    pub age_permits: BTreeSet<String>,
    pub branch_permits: BTreeMap<String, BTreeSet<String>>,
    /// Update keys the batch PR carries; set by `batch::apply`.
    pub batched: BTreeSet<String>,
}
fn checked_actions(body: &str) -> BTreeSet<Action> {
    let mut counts = BTreeMap::<Action, (usize, bool)>::new();
    let mut fence = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            let kind = &line[..3];
            if fence == Some(kind) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(kind);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let Some((state, rest)) = line.strip_prefix("- [").and_then(|s| s.split_once("] ")) else {
            continue;
        };
        let checked = state == "x" || state == "X";
        if !checked && state != " " {
            continue;
        }
        let Some((_, marker)) = rest.split_once("<!-- patchula-action:") else {
            continue;
        };
        let Some(json) = marker.strip_suffix(" -->") else {
            continue;
        };
        if json.len() > 256 {
            continue;
        }
        let Ok(action) = serde_json::from_str::<Action>(json) else {
            continue;
        };
        let target = match &action {
            Action::Recheck(t) | Action::Create(t) | Action::Recreate(t) | Action::Retry(t) => t,
        };
        if target.len() != 64
            || !target
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            continue;
        }
        let entry = counts.entry(action).or_default();
        entry.0 += 1;
        entry.1 |= checked;
    }
    counts
        .into_iter()
        .filter_map(|(a, (n, checked))| (n == 1 && checked).then_some(a))
        .collect()
}
impl Controls {
    /// Newly checked, valid marker actions; unrelated issue body edits do not trigger.
    pub fn checkbox_edited(before: &str, after: &str) -> bool {
        let previous = checked_actions(before);
        checked_actions(after)
            .iter()
            .any(|action| !previous.contains(action))
    }
    pub fn apply(&mut self, plans: &mut [Plan], dependencies: &[patchula_manager::Dependency]) {
        for plan in plans {
            let key = update_key(plan);
            let permits: BTreeSet<_> = plan
                .changes
                .iter()
                .flat_map(|c| {
                    dependencies
                        .iter()
                        .filter(|d| {
                            d.file == c.file
                                && d.package == c.package
                                && patchula_manager::word(&d.source) == c.source
                        })
                        .filter_map(|d| {
                            c.version
                                .as_deref()
                                .or(c.value.as_deref())
                                .map(|v| held_target(&extracted_key(d), v, &c.kind))
                        })
                })
                .filter(|target| self.age_permits.contains(target))
                .collect();
            if !permits.is_empty() {
                self.branch_permits
                    .insert(plan.action.branch.clone(), permits);
            }
            let age_click = plan.changes.iter().any(|c| {
                dependencies
                    .iter()
                    .filter(|d| {
                        d.file == c.file
                            && d.package == c.package
                            && patchula_manager::word(&d.source) == c.source
                    })
                    .any(|d| {
                        c.version
                            .as_deref()
                            .or(c.value.as_deref())
                            .is_some_and(|v| {
                                self.create
                                    .contains(&held_target(&extracted_key(d), v, &c.kind))
                            })
                    })
            });
            if age_click {
                self.create.insert(key.clone());
            }
            if (self.create.contains(&key)
                || self.recreate.contains(&key)
                || self.branches.contains(&plan.action.branch))
                && plan.action.reason.as_deref() == Some("outside the configured update windows")
            {
                plan.action.status = patchula_report::ActionStatus::Planned;
                plan.action.reason = None;
            }
            if self.recheck
                || self.branches.contains(&plan.action.branch)
                || self.create.contains(&key)
                || self.recreate.contains(&key)
            {
                plan.retry = true;
            }
        }
    }
    pub fn read(
        inventory: &Inventory,
        repo: &str,
        dashboard_enabled: bool,
    ) -> Result<Self, String> {
        let mut result = Self::default();
        if dashboard_enabled && let Some(issue) = issue(inventory)? {
            for action in checked_actions(issue["body"].as_str().unwrap_or("")) {
                match action {
                    Action::Recheck(t) if t == hash(repo.as_bytes()) => result.recheck = true,
                    Action::Recheck(_) => {}
                    Action::Create(t) => {
                        result.create.insert(t);
                    }
                    Action::Recreate(t) => {
                        result.recreate.insert(t);
                    }
                    Action::Retry(t) => {
                        result.retry.insert(t);
                    }
                }
            }
        }
        for pr in inventory.pull_requests(repo) {
            result
                .age_permits
                .extend(pr_permits(pr["body"].as_str().unwrap_or("")));
            if (result.recheck || crate::pull_requests::checked(&pr))
                && let Some(branch) = pr["head"]["ref"].as_str()
            {
                result.branches.insert(branch.into());
            }
        }
        Ok(result)
    }
}
const PERMIT_MARKER: &str = "<!-- patchula-age-permits:v1 ";
pub(crate) fn permit_marker(permits: &BTreeSet<String>) -> String {
    format!(
        "{PERMIT_MARKER}{} -->\n",
        serde_json::to_string(permits).expect("permits")
    )
}
fn pr_permits(body: &str) -> BTreeSet<String> {
    let mut lines = body.lines().filter_map(|l| l.strip_prefix(PERMIT_MARKER));
    let Some(json) = lines.next().and_then(|l| l.strip_suffix(" -->")) else {
        return BTreeSet::new();
    };
    if lines.next().is_some() || json.len() > 32_000 {
        return BTreeSet::new();
    }
    let Ok(permits) = serde_json::from_str::<BTreeSet<String>>(json) else {
        return BTreeSet::new();
    };
    if permits.iter().any(|t| {
        t.len() != 64
            || !t
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return BTreeSet::new();
    }
    permits
}
#[derive(Serialize, Deserialize)]
struct Permits {
    actor: String,
    targets: BTreeSet<String>,
}
fn permits_key(repo: &str) -> ExtractionKey {
    ExtractionKey {
        repo: repo.into(),
        default_branch_commit: String::new(),
        config_hash: "dashboard-age-permits-v1".into(),
    }
}
pub async fn load_permits(
    cache: &Cache,
    repo: &str,
    actor: &str,
) -> Result<BTreeSet<String>, String> {
    Ok(cache
        .extraction_get(&permits_key(repo))
        .await
        .map_err(|e| e.to_string())?
        .and_then(|b| serde_json::from_slice::<Permits>(&b).ok())
        .filter(|p| p.actor == actor)
        .map(|p| p.targets)
        .unwrap_or_default())
}
pub async fn save_permits(
    cache: &Cache,
    repo: &str,
    actor: &str,
    targets: BTreeSet<String>,
) -> Result<(), String> {
    cache
        .extraction_put(
            &permits_key(repo),
            &serde_json::to_vec(&Permits {
                actor: actor.into(),
                targets,
            })
            .map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())
}
/// Text for the page: neutralised first (see `text::inert`), then escaped so
/// no Markdown or HTML in it takes effect.
fn safe(text: &str) -> String {
    escape(&text::inert(text))
}
/// Text with no Markdown or HTML left in it. `&#64;` is still a mention, so
/// this alone does not make third-party text safe.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace(['\n', '\r'], " ")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
        .replace('`', "&#96;")
        .replace('*', "&#42;")
        .replace('_', "&#95;")
}
fn checkbox(label: &str, action: Action) -> String {
    format!(
        "- [ ] {label} <!-- patchula-action:{} -->\n",
        serde_json::to_string(&action).expect("action")
    )
}
pub fn issue(inventory: &Inventory) -> Result<Option<&Value>, String> {
    let mut issues = inventory.issues.iter().filter(|i| {
        crate::receipts::actor_login(&i["author"]).as_deref() == Some(&inventory.actor)
            && i["body"]
                .as_str()
                .is_some_and(|b| b.lines().filter(|l| *l == MARKER).count() == 1)
    });
    let result = issues.next();
    if issues.next().is_some() {
        return Err("multiple bot-owned dependency dashboards; close duplicates".into());
    }
    Ok(result)
}

/// No wall clock or lookup evaluation timestamps: unchanged evidence renders identically.
/// `server` is the URL to advertise to the CLI, if any.
pub fn render(
    repo: &Repository,
    plans: &[Plan],
    prs: &[Value],
    recheck: Recheck,
    server: Option<&str>,
) -> String {
    // Keep the numeric PR identity separate from the rendered Markdown.
    let mut sections = BTreeMap::<String, BTreeSet<(Option<u64>, String)>>::new();
    for pr in prs.iter().filter(|p| !p["merged_at"].is_null()) {
        if let Some(number) = pr["number"].as_u64() {
            sections.entry("Merged".into()).or_default().insert((
                Some(number),
                format!(
                    "- #{number}: {} — {}\n",
                    safe(pr["title"].as_str().unwrap_or("Update dependencies")),
                    safe(pr["patchula_merge"].as_str().unwrap_or("merged"))
                ),
            ));
        }
    }
    for pr in prs.iter().filter(|p| {
        p["state"] == "open"
            && !plans
                .iter()
                .any(|plan| p["head"]["ref"] == plan.action.branch)
    }) {
        if let Some(number) = pr["number"].as_u64() {
            sections
                .entry("Open PRs: other".into())
                .or_default()
                .insert((
                    Some(number),
                    format!(
                        "- #{number}: {}\n",
                        safe(pr["title"].as_str().unwrap_or("Update dependencies"))
                    ),
                ));
        }
    }
    for plan in plans {
        if plan.action.status == patchula_report::ActionStatus::Merged {
            continue;
        }
        let open = prs
            .iter()
            .find(|p| p["state"] == "open" && p["head"]["ref"] == plan.action.branch);
        if let Some(pr) = open {
            let kinds: BTreeSet<_> = plan.changes.iter().map(|c| c.kind.as_str()).collect();
            let kind = if plan.maintenance {
                "maintenance".into()
            } else {
                kinds.into_iter().collect::<Vec<_>>().join(", ")
            };
            if let Some(number) = pr["number"].as_u64() {
                sections
                    .entry(format!("Open PRs: {kind}"))
                    .or_default()
                    .insert((
                        Some(number),
                        format!(
                            "- #{number}: {}{}\n",
                            titled(plan),
                            pr["patchula_merge"]
                                .as_str()
                                .map(|reason| format!(" — {}", safe(reason)))
                                .unwrap_or_default()
                        ),
                    ));
            }
        } else if plan.action.reason.as_deref() == Some(crate::batch::BATCHED_REASON) {
            // The batch PR is listed with the open PRs; these are its items.
            sections
                .entry("Batched".into())
                .or_default()
                .insert((None, format!("- {}\n", titled(plan))));
        } else if plan.action.reason.as_deref() == Some(crate::pull_requests::CLOSED_BY_OWNER) {
            sections
                .entry("Updates you closed".into())
                .or_default()
                .insert((
                    None,
                    checkbox(
                        &format!("Bring back this update you closed: {}", titled(plan)),
                        Action::Recreate(update_key(plan)),
                    ),
                ));
        } else if let Some(reason) = &plan.action.reason {
            let row = if reason.starts_with("prs.")
                || reason == "outside the configured update windows"
            {
                checkbox(
                    &format!("Create it now anyway: {} ({})", titled(plan), safe(reason)),
                    Action::Create(update_key(plan)),
                )
            } else {
                let failure = text::failure(reason);
                let mut row = format!("- {}: {}\n", titled(plan), escape(&failure.summary));
                if let Some(detail) = &failure.detail {
                    row.push_str(&details(detail, "  "));
                }
                row
            };
            sections
                .entry("Pending updates".into())
                .or_default()
                .insert((None, row));
        }
    }
    let mut vulnerabilities = BTreeMap::<(String, String, String), Vulnerable>::new();
    let mut lookups = BTreeMap::<(String, String, String), Lookup>::new();
    let mut attributed = BTreeSet::<&String>::new();
    for file in &repo.package_files {
        for dep in &file.dependencies {
            if let Some(metadata) = &dep.metadata {
                let version = dep
                    .current
                    .version
                    .as_deref()
                    .or(dep.current.value.as_deref())
                    .unwrap_or("unknown");
                for advisory in &metadata.advisories {
                    let fix = fix_status(plans, prs, &file.path, dep, metadata, advisory);
                    let group = vulnerabilities
                        .entry((dep.manager.clone(), dep.package.clone(), version.into()))
                        .or_insert_with(|| Vulnerable {
                            name: dep.name.clone(),
                            version: version.into(),
                            ..Default::default()
                        });
                    group.files.insert(file.path.clone());
                    let entry = group
                        .advisories
                        .entry(advisory.id.clone())
                        .or_insert_with(|| (advisory, fix.clone()));
                    entry.1 = entry.1.clone().min(fix);
                }
                // Two pins of one tag differ only by digest.
                let pinned = dep
                    .current
                    .digest
                    .as_deref()
                    .map(|digest| format!("@{}", digest.chars().take(19).collect::<String>()))
                    .unwrap_or_default();
                let reference = format!(
                    "{} in {}",
                    code(&format!(
                        "{}:{}{}",
                        dep.package,
                        dep.current.value.as_deref().unwrap_or("unknown"),
                        pinned
                    )),
                    code(&file.path)
                );
                let previous = dep
                    .updates
                    .iter()
                    .filter_map(|u| u.image.as_ref())
                    .find(|check| check.state == "previous-unavailable");
                for check in metadata.image.iter().chain(previous) {
                    let row = match &check.silenced {
                        Some(reason) => (
                            IMAGES_SILENCED,
                            format!("- {reference}: {}\n", safe(reason)),
                        ),
                        None => (IMAGES, format!("- {reference}: {}\n", image_gap(check))),
                    };
                    sections
                        .entry(row.0.into())
                        .or_default()
                        .insert((None, row.1));
                }
                for held in &metadata.held_back {
                    sections
                        .entry("Pending updates".into())
                        .or_default()
                        .insert((
                            None,
                            checkbox(
                                &format!(
                                    "Create it now anyway: {} → {} ({}, {}; eligible {})",
                                    code(&dep.name),
                                    safe(&held.version),
                                    safe(&metadata.source),
                                    code(&file.path),
                                    escape(&text::when(
                                        held.target_eligible_at
                                            .as_deref()
                                            .unwrap_or(&held.eligible_at)
                                    ))
                                ),
                                Action::Create(held_key(
                                    &file.path,
                                    dep,
                                    &held.version,
                                    &held.update_type,
                                )),
                            ),
                        ));
                }
                let warnings: Vec<_> = repo
                    .warnings
                    .iter()
                    .filter(|w| {
                        w.starts_with(&format!("{}:", dep.name))
                            || w.starts_with(&format!("{} (", dep.name))
                            || w.starts_with(&format!("{}@", dep.name))
                    })
                    .collect();
                let failures: Vec<_> = repo
                    .lookup_failures
                    .iter()
                    .filter(|f| {
                        f.path == file.path && f.manager == dep.manager && f.name == dep.name
                    })
                    .collect();
                if !metadata.stale.is_empty() || !warnings.is_empty() || !failures.is_empty() {
                    // The lookup is shared by every file that declares the package.
                    let lookup = lookups
                        .entry((
                            dep.manager.clone(),
                            dep.package.clone(),
                            metadata.source.clone(),
                        ))
                        .or_insert_with(|| Lookup {
                            name: dep.name.clone(),
                            source: metadata.source.clone(),
                            ..Default::default()
                        });
                    lookup.files.insert(file.path.clone());
                    for (key, fetched) in &metadata.stale {
                        let newest = lookup.stale.entry(key.clone()).or_insert(*fetched);
                        *newest = (*newest).max(*fetched);
                    }
                    attributed.extend(warnings.iter().copied());
                    lookup
                        .warnings
                        .extend(warnings.into_iter().map(String::as_str));
                    lookup.failures.extend(failures.into_iter().map(|f| {
                        (
                            f.datasource.clone(),
                            f.kind.as_str().to_owned(),
                            f.message.clone(),
                        )
                    }));
                }
            }
            sections
                .entry(format!(
                    "Detected: {} / {}",
                    safe(&dep.manager),
                    safe(&file.path)
                ))
                .or_default()
                .insert((
                    None,
                    format!(
                        "- {}: {}\n",
                        code(&dep.name),
                        safe(dep.current.value.as_deref().unwrap_or("unknown"))
                    ),
                ));
        }
    }
    for ((manager, package, source), lookup) in lookups {
        let mut row = checkbox(
            &format!(
                "Try that lookup again: {} ({}, {})",
                code(&lookup.name),
                safe(&lookup.source),
                lookup
                    .files
                    .iter()
                    .map(|file| code(file))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Action::Retry(retry_key(&manager, &package, &source)),
        );
        for (key, fetched) in &lookup.stale {
            row.push_str(&format!(
                "  - Stale or rate-limited {}, cached {}\n",
                code(key),
                escape(&text::when_unix(*fetched))
            ));
        }
        for warning in &lookup.warnings {
            row.push_str(&format!("  - {}\n", safe(warning)));
        }
        for (datasource, kind, message) in &lookup.failures {
            let failure = text::failure(message);
            row.push_str(&format!(
                "  - Lookup failed ({}, {kind}): {}\n",
                safe(datasource),
                escape(&failure.summary)
            ));
            if let Some(detail) = &failure.detail {
                row.push_str(&details(detail, "    "));
            }
        }
        sections
            .entry("Lookup warnings and errors".into())
            .or_default()
            .insert((None, row));
    }
    for group in vulnerabilities.into_values() {
        let (rank, row) = group.render();
        sections
            .entry("Known vulnerabilities".into())
            .or_default()
            .insert((Some(rank), row));
    }
    for notice in &repo.notices {
        let (section, action) = if notice.is_template() {
            ("Template updates", "; run copier update to apply it")
        } else {
            ("Newer versions not proposed", "")
        };
        sections.entry(section.into()).or_default().insert((
            None,
            format!(
                "- {} in {}{action}{}\n",
                safe(&notice.text()),
                safe(&notice.path),
                notice
                    .compare_url
                    .as_ref()
                    .map(|url| format!(" ([compare]({}))", safe(url)))
                    .unwrap_or_default()
            ),
        ));
    }
    for warning in &repo.tooling {
        let open: Vec<_> = prs
            .iter()
            .filter(|p| {
                p["state"] == "open"
                    && warning
                        .branches
                        .iter()
                        .any(|b| p["head"]["ref"] == b.as_str())
            })
            .filter_map(|p| p["number"].as_u64())
            .map(|n| format!("#{n}"))
            .collect();
        let open = if open.is_empty() {
            String::new()
        } else {
            format!(" Open now: {}.", open.join(", "))
        };
        let row = match &warning.silenced {
            Some(reason) => (
                TOOLING_SILENCED,
                format!("- `{}`: \"{}\"{open}\n", safe(&warning.rule), safe(reason)),
            ),
            None => (
                TOOLING,
                format!(
                    "- {}{open}\n",
                    tooling_line(
                        warning,
                        repo.default_branch
                            .as_deref()
                            .unwrap_or("the default branch")
                    )
                ),
            ),
        };
        sections
            .entry(row.0.into())
            .or_default()
            .insert((None, row.1));
    }
    // A warning about one dependency is already under its lookup.
    for warning in repo.warnings.iter().filter(|w| !attributed.contains(w)) {
        sections
            .entry("Report warnings".into())
            .or_default()
            .insert((None, format!("- {}\n", safe(warning))));
    }
    let timing = match recheck {
        Recheck::Now => "now",
        Recheck::NextRun => "on the next run",
    };
    let advertised = server
        .map(|url| format!("{}\n", crate::advertise::marker(url)))
        .unwrap_or_default();
    let mut text = format!(
        "{MARKER}\n{advertised}\n[The Count](https://count-patchula.sweepy.dev) keeps watch over this repository's dependencies. Tick a box and he will see to it {timing}.\n\n"
    );
    text.push_str(&checkbox(
        "Re-check this repository",
        Action::Recheck(hash(repo.repository.as_bytes())),
    ));
    let mut sections: Vec<_> = sections.into_iter().collect();
    sections.sort_by_key(|(section, _)| {
        let rank = if section == "Known vulnerabilities" {
            0
        } else if section.starts_with(TOOLING) {
            1
        } else if section.starts_with(IMAGES) {
            2
        } else if section.starts_with("Open PRs: ") {
            3
        } else {
            3 + match section.as_str() {
                "Merged" => 1,
                "Pending updates" => 2,
                "Template updates" | "Newer versions not proposed" => 3,
                "Updates you closed" => 4,
                "Lookup warnings and errors" => 5,
                "Report warnings" => 6,
                _ => 7,
            }
        };
        (rank, section.clone())
    });
    let image_gaps = sections.iter().any(|(section, _)| section == IMAGES);
    let tooling = sections.iter().any(|(section, _)| section == TOOLING);
    let detected = sections
        .iter()
        .filter(|(section, _)| section.starts_with("Detected: "))
        .fold((0, 0), |(files, rows), (_, r)| (files + 1, rows + r.len()));
    let mut outer = false;
    for (section, rows) in sections {
        if section.starts_with("Detected: ") && !outer {
            // One line stands for every file's list.
            text.push_str(&format!(
                "\n<details>\n<summary>Every dependency found ({} in {} {})</summary>\n",
                detected.1,
                detected.0,
                if detected.0 == 1 { "file" } else { "files" }
            ));
            outer = true;
        }
        let (start, end) = if section == TOOLING {
            (
                format!(
                    "\n## {TOOLING}\n\nThese settings let updates merge on their own, and checks of this repository's CI, branch protection and delivery found a gap behind them.\n\n"
                ),
                "",
            )
        } else if section == TOOLING_SILENCED {
            (
                format!(
                    "{}\n<details><summary>Silenced ({})</summary>\n\n",
                    if tooling {
                        String::new()
                    } else {
                        format!("\n## {TOOLING}\n")
                    },
                    rows.len()
                ),
                "</details>\n",
            )
        } else if section == IMAGES {
            (
                format!(
                    "\n## {IMAGES}\n\nThese pinned images give the Count no usable package list, so he can't tell whether a new digest fixes vulnerabilities. He still proposes their updates.\n\n"
                ),
                "",
            )
        } else if section == IMAGES_SILENCED {
            (
                format!(
                    "{}\n<details><summary>Silenced ({})</summary>\n\n",
                    if image_gaps {
                        String::new()
                    } else {
                        format!("\n## {IMAGES}\n")
                    },
                    rows.len()
                ),
                "</details>\n",
            )
        } else if let Some(label) = section.strip_prefix("Detected: ") {
            (
                format!(
                    "\n<details>\n<summary>{label} ({} {})</summary>\n\n",
                    rows.len(),
                    if rows.len() == 1 {
                        "dependency"
                    } else {
                        "dependencies"
                    }
                ),
                "\n</details>\n",
            )
        } else {
            (format!("\n## {section}\n\n"), "")
        };
        if text.len() + start.len() + end.len() + 150 > LIMIT {
            break;
        }
        text.push_str(&start);
        for (_, row) in rows {
            if text.len() + row.len() + end.len() + 150 > LIMIT {
                text.push_str("\nAdditional entries omitted to fit GitHub's issue body limit.\n");
                break;
            }
            text.push_str(&row);
        }
        text.push_str(end);
    }
    if outer {
        text.push_str("\n</details>\n");
    }
    text
}

/// A collapsed block of tool output, indented under a list item.
fn details(output: &str, indent: &str) -> String {
    format!(
        "{indent}<details>\n{indent}<summary>The tool's output</summary>\n\n{}\n{indent}</details>\n",
        text::block(output, indent)
    )
}

/// A plan's title, with the names of its packages as code.
fn titled(plan: &Plan) -> String {
    let message = plan.action.message.as_str();
    let mut names: Vec<_> = plan.changes.iter().map(|c| c.package.as_str()).collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    names.dedup();
    match names
        .into_iter()
        .filter(|n| !n.is_empty())
        .find_map(|n| message.find(n).map(|at| (at, n)))
    {
        Some((at, name)) => format!(
            "{}{}{}",
            safe(&message[..at]),
            code(name),
            safe(&message[at + name.len()..])
        ),
        None => safe(message),
    }
}

/// How an advisory gets fixed. Ordered best first, so a package declared
/// in several files reports its most hopeful state.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Fix {
    Pr(u64),
    Update(String),
    NotProposed(String),
    None,
}
impl Fix {
    fn render(&self) -> String {
        match self {
            Self::Pr(number) => format!("fixed by #{number}"),
            Self::Update(version) => format!("fixed by the update to {}", code(version)),
            Self::NotProposed(version) => format!("fixed in {}, not proposed", code(version)),
            Self::None => "no fixed release yet".into(),
        }
    }
}
fn fix_status(
    plans: &[Plan],
    prs: &[Value],
    file: &str,
    dep: &Dependency,
    metadata: &patchula_report::DependencyMetadata,
    advisory: &patchula_report::Advisory,
) -> Fix {
    let Some(update) = dep.updates.iter().find(|u| u.fixes.contains(&advisory.id)) else {
        return advisory
            .fixed_in
            .clone()
            .map_or(Fix::None, Fix::NotProposed);
    };
    let number = plans
        .iter()
        .filter(|plan| {
            plan.changes.iter().any(|c| {
                c.file == file
                    && c.package == dep.package
                    && c.source == metadata.source
                    && ((update.new_version.is_some() && c.version == update.new_version)
                        || (update.new_value.is_some() && c.value == update.new_value))
            })
        })
        .find_map(|plan| {
            prs.iter()
                .find(|p| p["state"] == "open" && p["head"]["ref"] == plan.action.branch)
                .and_then(|p| p["number"].as_u64())
        });
    match (
        number,
        update.new_version.as_ref().or(update.new_value.as_ref()),
    ) {
        (Some(number), _) => Fix::Pr(number),
        (None, Some(version)) => Fix::Update(version.clone()),
        (None, None) => Fix::None,
    }
}

/// Every advisory against one package at one version, in every file that has it.
#[derive(Default)]
struct Vulnerable<'a> {
    name: String,
    version: String,
    files: BTreeSet<String>,
    advisories: BTreeMap<String, (&'a patchula_report::Advisory, Fix)>,
}
const SEVERITIES: [&str; 4] = ["critical", "high", "moderate", "low"];
fn severity_rank(severity: Option<&str>) -> usize {
    let severity = severity.map(str::to_ascii_lowercase);
    SEVERITIES
        .iter()
        .position(|s| {
            Some(*s) == severity.as_deref()
                || (*s == "moderate" && severity.as_deref() == Some("medium"))
        })
        .unwrap_or(SEVERITIES.len())
}
impl Vulnerable<'_> {
    /// Its severity rank, most severe first, and its bullet.
    fn render(&self) -> (u64, String) {
        let rank = self
            .advisories
            .values()
            .map(|(a, _)| severity_rank(a.severity.as_deref()))
            .min()
            .unwrap_or(SEVERITIES.len());
        let severity = SEVERITIES
            .get(rank)
            .map(|s| format!("**{s}**: "))
            .unwrap_or_default();
        let files: Vec<_> = self.files.iter().map(|f| code(f)).collect();
        let files = if files.len() > 5 {
            format!(
                "{} and {} more files",
                files[..5].join(", "),
                files.len() - 5
            )
        } else {
            files.join(", ")
        };
        let mut fixes = BTreeMap::<&Fix, usize>::new();
        for (_, fix) in self.advisories.values() {
            *fixes.entry(fix).or_default() += 1;
        }
        let fix = if fixes.len() == 1 {
            fixes.keys().next().map(|f| f.render()).unwrap_or_default()
        } else {
            fixes
                .iter()
                .map(|(fix, n)| format!("{n} {}", fix.render()))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let link = |advisory: &patchula_report::Advisory| {
            format!("[{}]({})", safe(&advisory.id), url(&advisory.url))
        };
        let aliases = |advisory: &patchula_report::Advisory| {
            let cves: Vec<_> = advisory
                .aliases
                .iter()
                .filter(|a| a.starts_with("CVE-"))
                .map(|a| safe(a))
                .collect();
            if cves.is_empty() {
                String::new()
            } else {
                format!(" ({})", cves.join(", "))
            }
        };
        let head = format!(
            "- {severity}{} {} in {files}",
            code(&self.name),
            safe(&self.version)
        );
        let mut advisories: Vec<_> = self.advisories.values().map(|(a, _)| *a).collect();
        advisories.sort_by_key(|a| (severity_rank(a.severity.as_deref()), a.id.clone()));
        let row = if let [advisory] = advisories[..] {
            format!(
                "{head}: {}{}{}. {}.\n",
                link(advisory),
                aliases(advisory),
                advisory
                    .summary
                    .as_ref()
                    .map(|s| format!(": {}", safe(s)))
                    .unwrap_or_default(),
                capitalise(&fix)
            )
        } else {
            let shown: Vec<_> = advisories.iter().take(12).map(|a| link(a)).collect();
            let more = advisories.len() - shown.len();
            let mut row = format!(
                "{head}: {} advisories ({}{}). {}.\n  <details>\n  <summary>What they say</summary>\n\n",
                advisories.len(),
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                },
                capitalise(&fix)
            );
            for advisory in advisories.iter().take(40) {
                row.push_str(&format!(
                    "  - {}{}{}{}\n",
                    link(advisory),
                    aliases(advisory),
                    advisory
                        .severity
                        .as_ref()
                        .map(|s| format!(", {}", safe(s)))
                        .unwrap_or_default(),
                    advisory
                        .summary
                        .as_ref()
                        .map(|s| format!(": {}", safe(s)))
                        .unwrap_or_default()
                ));
            }
            row.push_str("\n  </details>\n");
            row
        };
        (rank as u64, row)
    }
}
fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
/// A link destination that no character in it can cut short.
fn url(url: &str) -> String {
    url.chars()
        .map(|c| match c {
            ' ' | '(' | ')' | '<' | '>' | '[' | ']' | '`' | '"' | '\\' => {
                format!("%{:02X}", c as u32)
            }
            c => c.to_string(),
        })
        .collect::<String>()
        .replace("<!--", "%3C!--")
}

/// One failing package from one source, with every file that declares it.
#[derive(Default)]
struct Lookup<'a> {
    name: String,
    source: String,
    files: BTreeSet<String>,
    stale: BTreeMap<String, i64>,
    warnings: BTreeSet<&'a str>,
    /// Datasource, kind and message.
    failures: BTreeSet<(String, String, String)>,
}

const TOOLING: &str = "Tooling warnings";
const TOOLING_SILENCED: &str = "Tooling warnings: silenced";

/// A tooling warning as a dashboard line.
fn tooling_line(warning: &patchula_report::ToolingWarning, branch: &str) -> String {
    use crate::tooling::*;
    let branch = safe(branch);
    match (warning.rule.as_str(), warning.detail.as_deref()) {
        (MERGE_WITHOUT_CI, Some(NATIVE)) => "**Updates are set to merge, but there's no CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so nothing tests them before GitHub merges them.".into(),
        (MERGE_WITHOUT_CI, _) => "**Updates are set to merge, but there's no CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so these PRs stay open.".into(),
        (MERGE_WITHOUT_REQUIRED_CHECKS, Some(REVIEWS_ONLY)) => format!("**Nothing requires checks before GitHub merges.** `{branch}` requires a review but no status checks."),
        (MERGE_WITHOUT_REQUIRED_CHECKS, _) => format!("**Nothing requires checks before GitHub merges.** `{branch}` requires no status checks."),
        (SECURITY_MERGE_WITHOUT_DELIVERY, Some(TAGGED_RELEASE)) => format!("**Security fixes merge, but nothing runs when they do.** No workflow runs when `{branch}` changes, and the only one that could deliver them runs on tags or releases."),
        _ => format!("**Security fixes merge, but nothing ships them.** Fixes merge on their own, and no workflow runs when `{branch}` changes."),
    }
}

const IMAGES: &str = "Images the Count can't vouch for";
const IMAGES_SILENCED: &str = "Images the Count can't vouch for: silenced";

/// Why the Count can't vouch for an image, as a dashboard line.
fn image_gap(check: &patchula_report::ImageCheck) -> String {
    match check.state.as_str() {
        "no-sbom" => "no SBOM published".into(),
        "sbom-without-os" => format!(
            "its SBOM (made by {}) lists no OS packages",
            safe(check.detail.as_deref().unwrap_or("an unknown tool"))
        ),
        "os-not-covered" => match &check.os {
            Some(os) => format!("{}, which OSV.dev doesn't cover", safe(os)),
            None => "its OS isn't one OSV.dev covers".into(),
        },
        "previous-unavailable" => {
            "the previous image is gone from its registry, so the open update can't be compared"
                .into()
        }
        state => safe(state),
    }
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    number: u64,
    actor: String,
    hash: String,
}
fn receipt_key(repo: &str) -> ExtractionKey {
    ExtractionKey {
        repo: repo.into(),
        default_branch_commit: String::default(),
        config_hash: "dashboard-receipt-v1".into(),
    }
}
/// The dashboard issue the Count last wrote in `repo`.
pub async fn issue_number(cache: &Cache, repo: &str) -> Option<u64> {
    let bytes = cache.extraction_get(&receipt_key(repo)).await.ok()??;
    serde_json::from_slice::<Receipt>(&bytes)
        .ok()
        .map(|receipt| receipt.number)
}
pub async fn reconcile(
    client: &Client,
    cache: &Cache,
    repo: &Repository,
    config: &Config,
    inventory: &Inventory,
    body: String,
) -> Result<(), String> {
    let existing = issue(inventory)?;
    let path = format!("repos/{}/issues", repo.repository);
    if !config.prs.dashboard {
        if let Some(existing) = existing {
            let number = existing["number"]
                .as_u64()
                .ok_or("missing dashboard number")?;
            client
                .patch(&format!("{path}/{number}"), json!({"state":"closed"}))
                .await
                .map_err(|e| e.to_string())?;
        }
        // A closed dashboard is no longer the repository's dashboard.
        return cache
            .extraction_remove(&receipt_key(&repo.repository))
            .await
            .map_err(|e| e.to_string());
    }
    let digest = hash(body.as_bytes());
    let previous = cache
        .extraction_get(&receipt_key(&repo.repository))
        .await
        .map_err(|e| e.to_string())?
        .and_then(|b| serde_json::from_slice::<Receipt>(&b).ok());
    let number = if let Some(existing) = existing {
        let number = existing["number"]
            .as_u64()
            .ok_or("missing dashboard number")?;
        let unchanged = previous
            .as_ref()
            .is_some_and(|r| r.number == number && r.actor == inventory.actor && r.hash == digest)
            && existing["body"] == body;
        if !unchanged && existing["body"] != body {
            client
                .patch(&format!("{path}/{number}"), json!({"body":body}))
                .await
                .map_err(|e| e.to_string())?;
        }
        number
    } else {
        client
            .post(&path, json!({"title":TITLE,"body":body}))
            .await
            .map_err(|e| e.to_string())?["number"]
            .as_u64()
            .ok_or("missing created dashboard number")?
    };
    cache
        .extraction_put(
            &receipt_key(&repo.repository),
            &serde_json::to_vec(&Receipt {
                number,
                actor: inventory.actor.clone(),
                hash: digest,
            })
            .map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())
}
