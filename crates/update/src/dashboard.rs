//! A deterministic issue, discovered by ownership and marker rather than title.
use crate::{Inventory, Plan, Recheck, plan::hash, update_key};
use rustovate_cache::{Cache, ExtractionKey};
use rustovate_config::Config;
use rustovate_github::Client;
use rustovate_report::{Dependency, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MARKER: &str = "<!-- rustovate-dashboard:v1 -->";
const TITLE: &str = "Dependency Dashboard";
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
        serde_json::to_string(&(file, &dep.manager, &dep.name, &dep.package, &dep.current))
            .expect("dependency identity")
            .as_bytes(),
    )
}
pub fn held_key(file: &str, dep: &Dependency, version: &str, kind: &str) -> String {
    hash(
        serde_json::to_string(&(dependency_key(file, dep), version, kind))
            .expect("held identity")
            .as_bytes(),
    )
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
        "- [ ] {label} <!-- rustovate-action:{} -->\n",
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
pub fn render(repo: &Repository, plans: &[Plan], prs: &[Value], recheck: Recheck) -> String {
    let mut sections = BTreeMap::<String, BTreeSet<String>>::new();
    for pr in prs.iter().filter(|p| {
        p["state"] == "open"
            && !plans
                .iter()
                .any(|plan| p["head"]["ref"] == plan.action.branch)
    }) {
        if let Some(number) = pr["number"].as_u64() {
            sections
                .entry("Open PRs — other".into())
                .or_default()
                .insert(format!(
                    "- #{number}: {}\n",
                    safe(pr["title"].as_str().unwrap_or("Update dependencies"))
                ));
        }
    }
    for plan in plans {
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
                    .entry(format!("Open PRs — {kind}"))
                    .or_default()
                    .insert(format!("- #{number}: {}\n", safe(&plan.action.message)));
            }
        } else if plan.action.reason.as_deref() == Some(crate::pull_requests::CLOSED_BY_OWNER) {
            sections
                .entry("Owner-closed updates".into())
                .or_default()
                .insert(checkbox(
                    &format!(
                        "Recreate this owner-closed update: {}",
                        safe(&plan.action.message)
                    ),
                    Action::Recreate(update_key(plan)),
                ));
        } else if let Some(reason) = &plan.action.reason {
            let action = if reason.starts_with("prs.")
                || reason == "outside the configured update windows"
            {
                checkbox(
                    &format!(
                        "Create this held-back/limited update now: {} ({})",
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
                .insert(action);
        }
    }
    for file in &repo.package_files {
        for dep in &file.dependencies {
            if let Some(metadata) = &dep.metadata {
                for held in &metadata.held_back {
                    sections.entry("Pending updates".into()).or_default().insert(checkbox(
                        &format!("Create this held-back/limited update now: {} → {} ({}, {}; eligible at {})", safe(&dep.name), safe(&held.version), safe(&metadata.source), safe(&file.path), safe(&held.eligible_at)),
                        Action::Create(held_key(&file.path, dep, &held.version, &held.update_type))));
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
                            "Retry failed lookup: {} ({}, {})",
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
                        .insert(row);
                }
            }
            sections
                .entry(format!(
                    "Detected: {} / {}",
                    safe(&dep.manager),
                    safe(&file.path)
                ))
                .or_default()
                .insert(format!(
                    "- {}: {}\n",
                    safe(&dep.name),
                    safe(dep.current.value.as_deref().unwrap_or("unknown"))
                ));
        }
    }
    for warning in &repo.warnings {
        sections
            .entry("Report warnings".into())
            .or_default()
            .insert(format!("- {}\n", safe(warning)));
    }
    let timing = match recheck {
        Recheck::Now => "now",
        Recheck::NextRun => "on the next run",
    };
    let mut text = format!("{MARKER}\n\nCreated by rustovate CLI. Click to re-check {timing}.\n\n");
    text.push_str(&checkbox(
        "Re-check this repository",
        Action::Recheck(hash(repo.repository.as_bytes())),
    ));
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
        for row in rows {
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
