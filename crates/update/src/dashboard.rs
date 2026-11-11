//! A deterministic issue, discovered by ownership and marker rather than title.
use crate::{
    Inventory, Plan,
    identity::{extracted_key, held_key, held_target, retry_key},
    markers,
    text::{self, code},
};
use patchula_cache::{Cache, State};
use patchula_config::{Config, Registry};
use patchula_github::Client;
use patchula_manager::hash;
use patchula_report::{Dependency, Repository};
use patchula_words::{Recheck, Severity, ToolingDetail, ToolingRule};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
    for (checked, rest) in markers::checkboxes(body) {
        let Some(json) = markers::line_end(rest, markers::ACTION, 256) else {
            continue;
        };
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
            let key = plan.update_key();
            let permits: BTreeSet<_> = plan
                .changes()
                .iter()
                .flat_map(|c| {
                    dependencies
                        .iter()
                        .filter(|d| {
                            c.touches(
                                &d.file,
                                d.source,
                                &d.package,
                                d.registry.as_ref().map(Registry::recorded),
                            )
                        })
                        .filter_map(|d| {
                            c.version
                                .as_deref()
                                .or(c.value.as_deref())
                                .map(|v| held_target(&extracted_key(d), v, c.kind.as_str()))
                        })
                })
                .filter(|target| self.age_permits.contains(target))
                .collect();
            if !permits.is_empty() {
                self.branch_permits
                    .insert(plan.action().branch.clone(), permits);
            }
            let age_click = plan.changes().iter().any(|c| {
                dependencies
                    .iter()
                    .filter(|d| {
                        c.touches(
                            &d.file,
                            d.source,
                            &d.package,
                            d.registry.as_ref().map(Registry::recorded),
                        )
                    })
                    .any(|d| {
                        c.version
                            .as_deref()
                            .or(c.value.as_deref())
                            .is_some_and(|v| {
                                self.create.contains(&held_target(
                                    &extracted_key(d),
                                    v,
                                    c.kind.as_str(),
                                ))
                            })
                    })
            });
            if age_click {
                self.create.insert(key.clone());
            }
            if (self.create.contains(&key)
                || self.recreate.contains(&key)
                || self.branches.contains(&plan.action().branch))
                && plan.held(&crate::plan::Hold::Window)
            {
                plan.lift();
            }
            if self.recheck
                || self.branches.contains(&plan.action().branch)
                || self.create.contains(&key)
                || self.recreate.contains(&key)
            {
                plan.request_retry();
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
            for action in checked_actions(&issue.body) {
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
                .extend(pr_permits(crate::pull_requests::body(&pr.pr)));
            if result.recheck || crate::pull_requests::checked(&pr.pr) {
                result.branches.insert(pr.pr.head.branch.clone());
            }
        }
        Ok(result)
    }
}
pub(crate) fn permit_marker(permits: &BTreeSet<String>) -> String {
    format!("{}\n", markers::hidden(markers::PERMITS, permits))
}
fn pr_permits(body: &str) -> BTreeSet<String> {
    let Some(json) = markers::section_line(body, markers::PERMITS, 32_000) else {
        return BTreeSet::new();
    };
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
fn repository_scope(repo: &str) -> patchula_cache::RepositoryScope {
    patchula_cache::RepositoryScope {
        repository: repo.into(),
    }
}
impl State for Permits {
    const NAMESPACE: &'static str = "dashboard-age-permits";
    const VERSION: u32 = 1;
    type Scope = patchula_cache::RepositoryScope;
}
pub async fn load_permits(
    cache: &Cache,
    repo: &str,
    actor: &str,
) -> Result<BTreeSet<String>, String> {
    Ok(cache
        .state::<Permits>(&repository_scope(repo))
        .await
        .map_err(|e| e.to_string())?
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
        .set_state(
            &repository_scope(repo),
            &Permits {
                actor: actor.into(),
                targets,
            },
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
        "- [ ] {label} {}{} -->\n",
        markers::ACTION,
        serde_json::to_string(&action).expect("action")
    )
}
/// The repository's dashboard issue, typed: `issue` is bot-authored and
/// carries the dashboard marker, and at most one may exist. An unreadable
/// issue page proves neither absence nor uniqueness, so it is an error.
pub fn issue(inventory: &Inventory) -> Result<Option<&crate::receipts::Issue>, String> {
    if !inventory.issues.complete {
        return Err("incomplete issue inventory".into());
    }
    let mut issues = inventory.issues.items.iter().flatten().filter(|i| {
        i.author
            .as_ref()
            .map(crate::receipts::actor_login)
            .as_deref()
            == Some(inventory.actor.as_str())
            && markers::dashboard(&i.body)
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
    prs: &[crate::pull_requests::Tracked],
    merged: &[crate::automerge::MergedPr],
    recheck: Recheck,
    server: Option<&str>,
) -> String {
    // Keep the numeric PR identity separate from the rendered Markdown.
    let mut sections = BTreeMap::<String, BTreeSet<(Option<u64>, String)>>::new();
    for row in merged.iter().filter(|row| row.merged_at.is_some()) {
        if let Some(number) = row.number {
            sections.entry("Merged".into()).or_default().insert((
                Some(number),
                format!(
                    "- #{number}: {} — {}\n",
                    safe(row.title.as_deref().unwrap_or("Update dependencies")),
                    safe(row.merge_note.as_deref().unwrap_or("merged"),)
                ),
            ));
        }
    }
    for tracked in prs.iter().filter(|t| {
        t.pr.state == "open"
            && !plans
                .iter()
                .any(|plan| t.pr.head.branch == plan.action().branch)
    }) {
        sections
            .entry("Open PRs: other".into())
            .or_default()
            .insert((
                Some(tracked.pr.number),
                format!("- #{}: {}\n", tracked.pr.number, safe(&tracked.pr.title)),
            ));
    }
    for plan in plans {
        if plan.action().status == patchula_report::BranchStatus::Merged {
            continue;
        }
        let open = prs
            .iter()
            .find(|t| t.pr.state == "open" && t.pr.head.branch == plan.action().branch);
        if let Some(tracked) = open {
            let kinds: BTreeSet<_> = plan.changes().iter().map(|c| c.kind.as_str()).collect();
            let kind = if plan.is_maintenance() {
                "maintenance".into()
            } else {
                kinds.into_iter().collect::<Vec<_>>().join(", ")
            };
            sections
                .entry(format!("Open PRs: {kind}"))
                .or_default()
                .insert((
                    Some(tracked.pr.number),
                    format!(
                        "- #{}: {}{}\n",
                        tracked.pr.number,
                        titled(plan),
                        tracked
                            .merge_note()
                            .map(|reason| format!(" — {}", safe(reason)))
                            .unwrap_or_default()
                    ),
                ));
        } else if plan.held(&crate::plan::Hold::Batched) {
            // The batch PR is listed with the open PRs; these are its items.
            sections
                .entry("Batched".into())
                .or_default()
                .insert((None, format!("- {}\n", titled(plan))));
        } else if plan.held(&crate::plan::Hold::ClosedByOwner) {
            sections
                .entry("Updates you closed".into())
                .or_default()
                .insert((
                    None,
                    checkbox(
                        &format!("Bring back this update you closed: {}", titled(plan)),
                        Action::Recreate(plan.update_key()),
                    ),
                ));
        } else if let Some(reason) = &plan.action().reason {
            let row = if plan.overridable() {
                checkbox(
                    &format!("Create it now anyway: {} ({})", titled(plan), safe(reason)),
                    Action::Create(plan.update_key()),
                )
            } else {
                let failure = plan.failure().expect("every reason keeps its failure");
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
                        .entry((dep.manager.to_string(), dep.package.clone(), version.into()))
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
                    .find(|check| check.state == patchula_report::ImageState::PreviousUnavailable);
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
                                    safe(metadata.source.as_str()),
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
                                    held.update_type.as_str(),
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
                            dep.manager.to_string(),
                            dep.package.clone(),
                            metadata.source.to_string(),
                        ))
                        .or_insert_with(|| Lookup {
                            name: dep.name.clone(),
                            source: metadata.source.to_string(),
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
                            f.datasource.to_string(),
                            f.kind.as_str().to_owned(),
                            f.message.clone(),
                        )
                    }));
                }
            }
            sections
                .entry(format!(
                    "Detected: {} / {}",
                    safe(dep.manager.as_str()),
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
            .filter(|t| t.pr.state == "open" && warning.branches.contains(&t.pr.head.branch))
            .map(|t| format!("#{}", t.pr.number))
            .collect();
        let open = if open.is_empty() {
            String::new()
        } else {
            format!(" Open now: {}.", open.join(", "))
        };
        let row = match &warning.silenced {
            Some(reason) => (
                TOOLING_SILENCED,
                format!("- `{}`: \"{}\"{open}\n", warning.rule, safe(reason)),
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
    let timing = crate::pull_requests::recheck_timing(recheck);
    let advertised = server
        .map(|url| format!("{}\n", crate::advertise::marker(url)))
        .unwrap_or_default();
    let mut text = format!(
        "{}\n{advertised}\n[The Count](https://count-patchula.sweepy.dev) keeps watch over this repository's dependencies. Tick a box and he will see to it {timing}.\n\n",
        markers::DASHBOARD
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
    let message = plan.action().message.as_str();
    let mut names: Vec<_> = plan.changes().iter().map(|c| c.package.as_str()).collect();
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
    /// Proposed, but its owner closed the PR.
    Closed(String),
    None,
}
impl Fix {
    fn render(&self) -> String {
        match self {
            Self::Pr(number) => format!("fixed by #{number}"),
            Self::Update(version) => format!("fixed by the update to {}", code(version)),
            Self::NotProposed(version) => format!("fixed in {}, not proposed", code(version)),
            Self::Closed(version) => format!("fixed in {}, whose PR was closed", code(version)),
            Self::None => "no fixed release yet".into(),
        }
    }
}
fn fix_status(
    plans: &[Plan],
    prs: &[crate::pull_requests::Tracked],
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
    let matching: Vec<&Plan> = plans
        .iter()
        .filter(|plan| {
            plan.changes().iter().any(|c| {
                c.touches(file, metadata.source, &dep.package, dep.registry.as_deref())
                    && ((update.new_version.is_some() && c.version == update.new_version)
                        || (update.new_value.is_some() && c.value == update.new_value))
            })
        })
        .collect();
    let open = |branch: &str| {
        prs.iter()
            .find(|t| t.pr.state == "open" && t.pr.head.branch == branch)
            .map(|t| t.pr.number)
    };
    // A batched update is fixed by the batch PR, if that PR carries it.
    let batch = |key: &str| {
        plans
            .iter()
            .find(|plan| {
                plan.batch_items()
                    .is_some_and(|items| items.iter().any(|item| item.origin == key))
            })
            .and_then(|plan| open(&plan.action().branch))
    };
    let number = matching.iter().find_map(|plan| {
        if plan.held(&crate::plan::Hold::Batched) {
            batch(&plan.update_key())
        } else {
            open(&plan.action().branch)
        }
    });
    // Closed only when no matching plan is still on its way to a PR.
    let closed = !matching.is_empty()
        && matching
            .iter()
            .all(|plan| plan.held(&crate::plan::Hold::ClosedByOwner));
    match (
        number,
        update.new_version.as_ref().or(update.new_value.as_ref()),
    ) {
        (Some(number), _) => Fix::Pr(number),
        (None, Some(version)) if closed => Fix::Closed(version.clone()),
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
fn severity_rank(severity: Option<Severity>) -> usize {
    severity.map_or(Severity::ALL.len(), |s| {
        Severity::ALL
            .iter()
            .position(|v| *v == s)
            .unwrap_or(Severity::ALL.len())
    })
}
impl Vulnerable<'_> {
    /// Its severity rank, most severe first, and its bullet.
    fn render(&self) -> (u64, String) {
        let rank = self
            .advisories
            .values()
            .map(|(a, _)| severity_rank(a.severity))
            .min()
            .unwrap_or(Severity::ALL.len());
        let severity = Severity::ALL
            .get(rank)
            .map(|s| format!("**{}**: ", s.as_str()))
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
        advisories.sort_by_key(|a| (severity_rank(a.severity), a.id.clone()));
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
                        .map(|s| format!(", {}", safe(s.as_str())))
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
    let branch = safe(branch);
    match (warning.rule, warning.detail) {
        (ToolingRule::MergeWithoutCi, Some(ToolingDetail::Native)) => "**Updates are set to merge, but there's no CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so nothing tests them before GitHub merges them.".into(),
        (
            ToolingRule::MergeWithoutCi,
            Some(ToolingDetail::ReviewsOnly | ToolingDetail::TaggedRelease) | None,
        ) => "**Updates are set to merge, but there's no CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so these PRs stay open.".into(),
        (ToolingRule::MergeWithoutRequiredChecks, Some(ToolingDetail::ReviewsOnly)) => format!("**Nothing requires checks before GitHub merges.** `{branch}` requires a review but no status checks."),
        (
            ToolingRule::MergeWithoutRequiredChecks,
            Some(ToolingDetail::Native | ToolingDetail::TaggedRelease) | None,
        ) => format!("**Nothing requires checks before GitHub merges.** `{branch}` requires no status checks."),
        (ToolingRule::SecurityMergeWithoutDelivery, Some(ToolingDetail::TaggedRelease)) => format!("**Security fixes merge, but nothing runs when they do.** No workflow runs when `{branch}` changes, and the only one that could deliver them runs on tags or releases."),
        (
            ToolingRule::SecurityMergeWithoutDelivery,
            Some(ToolingDetail::Native | ToolingDetail::ReviewsOnly) | None,
        ) => format!("**Security fixes merge, but nothing ships them.** Fixes merge on their own, and no workflow runs when `{branch}` changes."),
    }
}

const IMAGES: &str = "Images the Count can't vouch for";
const IMAGES_SILENCED: &str = "Images the Count can't vouch for: silenced";

/// Why the Count can't vouch for an image, as a dashboard line.
fn image_gap(check: &patchula_report::ImageCheck) -> String {
    use patchula_report::ImageState;
    match check.state {
        ImageState::NoSbom => "no SBOM published".into(),
        ImageState::SbomWithoutOs => format!(
            "its SBOM (made by {}) lists no OS packages",
            safe(check.detail.as_deref().unwrap_or("an unknown tool"))
        ),
        ImageState::OsNotCovered => match &check.os {
            Some(os) => format!("{}, which OSV.dev doesn't cover", safe(os)),
            None => "its OS isn't one OSV.dev covers".into(),
        },
        ImageState::PreviousUnavailable => {
            "the previous image is gone from its registry, so the open update can't be compared"
                .into()
        }
        ImageState::Unreadable
        | ImageState::PartialSources
        | ImageState::Checked
        | ImageState::NoOsPackages => safe(check.state.as_str()),
    }
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    number: u64,
    actor: String,
    hash: String,
}
impl State for Receipt {
    const NAMESPACE: &'static str = "dashboard-receipt";
    const VERSION: u32 = 1;
    type Scope = patchula_cache::RepositoryScope;
}
/// The dashboard issue the Count last wrote in `repo`.
pub async fn issue_number(cache: &Cache, repo: &str) -> Option<u64> {
    cache
        .state::<Receipt>(&repository_scope(repo))
        .await
        .ok()?
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
            client
                .patch::<Value>(
                    &format!("{path}/{}", existing.number),
                    json!({"state":"closed"}),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        // A closed dashboard is no longer the repository's dashboard.
        return cache
            .clear_state::<Receipt>(&repository_scope(&repo.repository))
            .await
            .map_err(|e| e.to_string());
    }
    let digest = hash(body.as_bytes());
    let previous = cache
        .state::<Receipt>(&repository_scope(&repo.repository))
        .await
        .map_err(|e| e.to_string())?;
    let number = if let Some(existing) = existing {
        let number = existing.number as u64;
        let current = existing.body.as_str();
        let unchanged = previous
            .as_ref()
            .is_some_and(|r| r.number == number && r.actor == inventory.actor && r.hash == digest)
            && crate::pr::body::body_hash(current) == digest;
        if !unchanged && crate::pr::body::body_hash(current) != digest {
            client
                .patch::<Value>(&format!("{path}/{number}"), json!({"body":body}))
                .await
                .map_err(|e| e.to_string())?;
        }
        number
    } else {
        client
            .post::<patchula_github::model::Issue>(&path, json!({"title":TITLE,"body":body}))
            .await
            .map_err(|e| e.to_string())?
            .number
    };
    cache
        .set_state(
            &repository_scope(&repo.repository),
            &Receipt {
                number,
                actor: inventory.actor.clone(),
                hash: digest,
            },
        )
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_report::{ImageCheck, ImageState, ToolingWarning};
    use patchula_words::{ToolingDetail, ToolingRule};

    fn warning(rule: ToolingRule, detail: Option<ToolingDetail>) -> ToolingWarning {
        ToolingWarning {
            rule,
            detail,
            branches: vec!["main".into()],
            silenced: None,
        }
    }

    #[test]
    fn tooling_lines_word_each_missing_delivery_case() {
        for (rule, detail, expected) in [
            (
                ToolingRule::MergeWithoutCi,
                Some(ToolingDetail::Native),
                "GitHub merges",
            ),
            (
                ToolingRule::MergeWithoutCi,
                Some(ToolingDetail::ReviewsOnly),
                "stay open",
            ),
            (ToolingRule::MergeWithoutCi, None, "stay open"),
            (
                ToolingRule::MergeWithoutCi,
                Some(ToolingDetail::TaggedRelease),
                "stay open",
            ),
            (
                ToolingRule::MergeWithoutRequiredChecks,
                Some(ToolingDetail::ReviewsOnly),
                "requires a review",
            ),
            (
                ToolingRule::MergeWithoutRequiredChecks,
                Some(ToolingDetail::Native),
                "requires no status checks",
            ),
            (
                ToolingRule::MergeWithoutRequiredChecks,
                None,
                "requires no status checks",
            ),
            (
                ToolingRule::SecurityMergeWithoutDelivery,
                Some(ToolingDetail::TaggedRelease),
                "when `main` changes",
            ),
            (
                ToolingRule::SecurityMergeWithoutDelivery,
                Some(ToolingDetail::Native),
                "nothing ships",
            ),
            (
                ToolingRule::SecurityMergeWithoutDelivery,
                None,
                "nothing ships",
            ),
        ] {
            let line = tooling_line(&warning(rule, detail), "main");
            assert!(line.contains(expected), "{rule:?} {detail:?}: {line}");
        }
    }

    #[test]
    fn image_gaps_name_why_the_count_cannot_vouch() {
        let check = |state: ImageState, detail: Option<&str>, os: Option<&str>| -> ImageCheck {
            ImageCheck {
                state,
                detail: detail.map(Into::into),
                os: os.map(Into::into),
                ..Default::default()
            }
        };
        assert_eq!(
            image_gap(&check(ImageState::NoSbom, None, None)),
            "no SBOM published"
        );
        assert!(image_gap(&check(ImageState::SbomWithoutOs, Some("syft"), None)).contains("syft"));
        assert!(
            image_gap(&check(ImageState::OsNotCovered, None, Some("windows"))).contains("windows")
        );
        assert!(image_gap(&check(ImageState::OsNotCovered, None, None)).contains("OSV.dev"));
        assert!(
            image_gap(&check(ImageState::PreviousUnavailable, None, None))
                .contains("previous image")
        );
        assert_eq!(
            image_gap(&check(ImageState::Checked, None, None)),
            "checked"
        );
    }

    /// An advisory a batched update fixes points at the batch PR, when the
    /// batch carries the update's key and has an open PR.
    #[test]
    fn a_batched_fix_reports_the_batch_prs_number() {
        use patchula_report::BranchStatus;
        let action = |branch: &str| patchula_report::BranchAction {
            repository: "o/r".into(),
            branch: branch.into(),
            base_commit: "b".repeat(40),
            status: BranchStatus::Planned,
            reason: None,
            message: "m".into(),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        };
        let change = || crate::Change {
            source: patchula_words::Source::Npm,
            package: "shared".into(),
            registry: None,
            file: "package.json".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some("1.2.0".into()),
            version: Some("1.2.0".into()),
            digest: None,
            kind: patchula_words::Update::Minor,
            merge: false,
            labels: Vec::new(),
            release_notes: false,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        };
        let mut held = crate::plan::testing::Draft::new(action("patchula/shared-1.x"))
            .change(change())
            .identity("shared")
            .build();
        held.hold(crate::plan::Hold::Batched);
        let carrier = crate::plan::testing::Draft::new(action("patchula/batch"))
            .batch(vec![crate::batch::Item {
                origin: held.update_key(),
                number: None,
                title: "batched".into(),
                targets: vec![change()],
            }])
            .build();
        let prs = vec![crate::pull_requests::Tracked::from(
            serde_json::from_value::<patchula_github::model::PullRequest>(serde_json::json!({
                "number": 7, "node_id": "N7", "state": "open",
                "html_url": "https://github.com/o/r/pull/7", "title": "batch", "body": "",
                "labels": [], "user": {"login": "bot"},
                "head": {"ref": "patchula/batch", "sha": "h", "repo": {"full_name": "o/r"}},
                "base": {"ref": "main", "repo": {"full_name": "o/r"}}
            }))
            .unwrap(),
        )];
        let dep: patchula_report::Dependency = serde_json::from_value(serde_json::json!({
            "manager": "npm", "name": "shared", "package": "shared",
            "current": {"value": "1.0.0", "version": "1.0.0", "digest": null},
            "updates": [{"newValue": "1.2.0", "newVersion": "1.2.0", "newDigest": null,
                "updateType": "minor", "fixes": ["GHSA-1"]}],
        }))
        .unwrap();
        let metadata: patchula_report::DependencyMetadata =
            serde_json::from_value(serde_json::json!({
                "source": "npm", "kinds": [], "locations": [],
                "advisories": [{"id": "GHSA-1", "url": "https://a.example"}],
            }))
            .unwrap();
        let advisory = &metadata.advisories[0];
        let fix = fix_status(
            &[held, carrier],
            &prs,
            "package.json",
            &dep,
            &metadata,
            advisory,
        );
        assert_eq!(fix.render(), "fixed by #7");
    }
}
