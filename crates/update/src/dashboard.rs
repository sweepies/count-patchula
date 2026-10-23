//! A deterministic issue, discovered by ownership and marker rather than title.
use crate::{Inventory, Plan, Recheck, plan::hash, update_key};
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
fn safe(text: &str) -> String {
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
            sections
                .entry("Merged by the Count".into())
                .or_default()
                .insert((
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
                            safe(&plan.action.message),
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
                .entry("Batched by the Count".into())
                .or_default()
                .insert((None, format!("- {}\n", safe(&plan.action.message))));
        } else if plan.action.reason.as_deref() == Some(crate::pull_requests::CLOSED_BY_OWNER) {
            sections
                .entry("Updates you closed".into())
                .or_default()
                .insert((
                    None,
                    checkbox(
                        &format!(
                            "Bring back this update you closed: {}",
                            safe(&plan.action.message)
                        ),
                        Action::Recreate(update_key(plan)),
                    ),
                ));
        } else if let Some(reason) = &plan.action.reason {
            let action = if reason.starts_with("prs.")
                || reason == "outside the configured update windows"
            {
                checkbox(
                    &format!(
                        "Create it now anyway: {} ({})",
                        safe(&plan.action.message),
                        safe(reason)
                    ),
                    Action::Create(update_key(plan)),
                )
            } else {
                format!("- {}: {}\n", safe(&plan.action.message), safe(reason))
            };
            sections
                .entry("Pending updates".into())
                .or_default()
                .insert((None, action));
        }
    }
    for file in &repo.package_files {
        for dep in &file.dependencies {
            if let Some(metadata) = &dep.metadata {
                let catalog_age = metadata
                    .lookup
                    .as_ref()
                    .and_then(|lookup| lookup.catalog.as_ref())
                    .map(|catalog| format!("; catalog fetched at {}", safe(&catalog.fetched_at)))
                    .unwrap_or_default();
                for held in &metadata.held_back {
                    sections
                        .entry("Pending updates".into())
                        .or_default()
                        .insert((
                            None,
                            checkbox(
                                &format!(
                                    "Create it now anyway: {} → {} ({}, {}; eligible at {}{})",
                                    safe(&dep.name),
                                    safe(&held.version),
                                    safe(&metadata.source),
                                    safe(&file.path),
                                    safe(
                                        held.target_eligible_at
                                            .as_deref()
                                            .unwrap_or(&held.eligible_at)
                                    ),
                                    catalog_age
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
                if !metadata.stale.is_empty() || !warnings.is_empty() {
                    let mut row = checkbox(
                        &format!(
                            "Try that lookup again: {} ({}, {})",
                            safe(&dep.name),
                            safe(&metadata.source),
                            safe(&file.path)
                        ),
                        Action::Retry(dependency_key(&file.path, dep)),
                    );
                    for (key, fetched) in &metadata.stale {
                        row.push_str(&format!(
                            "  - Stale/rate-limited {} (cached at Unix {})\n",
                            safe(key),
                            fetched
                        ));
                    }
                    for warning in warnings {
                        row.push_str(&format!("  - {}\n", safe(warning)));
                    }
                    sections
                        .entry("Lookup warnings and errors".into())
                        .or_default()
                        .insert((None, row));
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
                        safe(&dep.name),
                        safe(dep.current.value.as_deref().unwrap_or("unknown"))
                    ),
                ));
        }
    }
    for warning in &repo.warnings {
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
        "{MARKER}\n{advertised}\nThe Count keeps watch over this repository's dependencies. Tick a box and he will see to it {timing}.\n\n"
    );
    text.push_str(&checkbox(
        "Ask the Count to re-check this repository",
        Action::Recheck(hash(repo.repository.as_bytes())),
    ));
    let mut sections: Vec<_> = sections.into_iter().collect();
    sections.sort_by_key(|(section, _)| {
        let rank = if section.starts_with("Open PRs: ") {
            0
        } else {
            match section.as_str() {
                "Merged by the Count" => 1,
                "Pending updates" => 2,
                "Updates you closed" => 3,
                "Lookup warnings and errors" => 4,
                "Report warnings" => 5,
                _ => 6,
            }
        };
        (rank, section.clone())
    });
    for (section, rows) in sections {
        let (start, end) = if let Some(label) = section.strip_prefix("Detected: ") {
            (
                format!(
                    "\n<details>\n<summary>{label} ({} dependencies)</summary>\n\n",
                    rows.len()
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
    text
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
        default_branch_commit: String::new(),
        config_hash: "dashboard-receipt-v1".into(),
    }
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
        return Ok(());
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
