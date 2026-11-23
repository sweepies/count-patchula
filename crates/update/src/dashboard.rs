//! A deterministic issue, discovered by ownership and marker rather than title.
use crate::error::Error;
use crate::{
    Inventory, Plan,
    identity::{extracted_key, held_key, held_target, retry_key},
    markers,
    pr::body::{id_cell, linked, prose_cell},
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
/// Footer matching the Count's pull request bodies, with the avatar pinned to
/// the running release.
const FOOTER: &str = concat!(
    "\n---\n\n<p>\n<a href=\"https://count-patchula.sweepy.dev\"><img align=\"left\" src=\"https://raw.githubusercontent.com/sweepies/count-patchula/v",
    env!("CARGO_PKG_VERSION"),
    "/assets/logo/bat.svg\" width=\"52\" alt=\"Count Patchula\"></a>\n<strong><a href=\"https://count-patchula.sweepy.dev\">Count Patchula</a></strong> keeps watch over this repository<br>\n<sub>The Count works for free, but if you find him helpful, <a href=\"https://github.com/sponsors/sweepies\">consider supporting his development</a>.</sub><br clear=\"left\">\n</p>\n"
);

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
    Batch(String),
}
#[derive(Clone, Debug, Default)]
pub struct Controls {
    pub recheck: bool,
    pub branches: BTreeSet<String>,
    pub create: BTreeSet<String>,
    pub recreate: BTreeSet<String>,
    pub retry: BTreeSet<String>,
    /// Update keys whose "Add to batch" box was ticked on the dashboard.
    pub batch: BTreeSet<String>,
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
            Action::Recheck(t)
            | Action::Create(t)
            | Action::Recreate(t)
            | Action::Retry(t)
            | Action::Batch(t) => t,
        };
        if !is_key(target) {
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
            // The held targets this plan's changes move, one per
            // dependency each change touches.
            let targets: BTreeSet<String> = plan
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
                .collect();
            let permits: BTreeSet<_> = targets
                .iter()
                .filter(|target| self.age_permits.contains(*target))
                .cloned()
                .collect();
            if !permits.is_empty() {
                self.branch_permits
                    .insert(plan.action().branch.clone(), permits);
            }
            let age_click = targets.iter().any(|target| self.create.contains(target));
            if age_click {
                self.create.insert(key.clone());
            }
            let asked = self.create.contains(&key)
                || self.recreate.contains(&key)
                || self.branches.contains(&plan.action().branch);
            if asked && plan.held(&crate::plan::Hold::Window) {
                plan.lift();
            }
            if self.recheck || asked {
                plan.request_retry();
            }
        }
    }
    pub fn read(inventory: &Inventory, repo: &str, dashboard_enabled: bool) -> Result<Self, Error> {
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
                    Action::Batch(t) => {
                        result.batch.insert(t);
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
/// Whether `text` is an update key: 64 lowercase hex digits.
fn is_key(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn pr_permits(body: &str) -> BTreeSet<String> {
    let Some(json) = markers::section_line(body, markers::PERMITS, 32_000) else {
        return BTreeSet::new();
    };
    let Ok(permits) = serde_json::from_str::<BTreeSet<String>>(json) else {
        return BTreeSet::new();
    };
    if !permits.iter().all(|t| is_key(t)) {
        return BTreeSet::new();
    }
    permits
}
#[derive(Serialize, Deserialize)]
struct Permits {
    actor: String,
    targets: BTreeSet<String>,
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
) -> Result<BTreeSet<String>, Error> {
    Ok(cache
        .state::<Permits>(&crate::repository_scope(repo))
        .await?
        .filter(|p| p.actor == actor)
        .map(|p| p.targets)
        .unwrap_or_default())
}
pub async fn save_permits(
    cache: &Cache,
    repo: &str,
    actor: &str,
    targets: BTreeSet<String>,
) -> Result<(), Error> {
    Ok(cache
        .set_state(
            &crate::repository_scope(repo),
            &Permits {
                actor: actor.into(),
                targets,
            },
        )
        .await?)
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
/// Generated text as an inline code span inside a table cell: its pipes
/// escaped so they cannot end the cell.
fn code_cell(text: &str) -> String {
    code(text).replace('|', "\\|")
}
/// The update key carried by `tracked`'s PR body when that body also carries a
/// non-empty reviewed-targets marker, so ticking the dashboard's batch box is
/// guaranteed to match `batch::items`.
fn batchable_pr_origin(tracked: &crate::pull_requests::Tracked) -> Option<String> {
    let body = crate::pull_requests::body(&tracked.pr);
    let origin = crate::pull_requests::marker_update(body)?;
    let targets =
        markers::section_json::<Vec<crate::Change>>(body, markers::TARGETS, markers::PAYLOAD)?;
    (!targets.is_empty()).then_some(origin)
}
/// A plan's title formatted for a table cell: pipe-safe.
fn titled_cell(plan: &Plan) -> String {
    titled(plan).replace('|', "\\|")
}
/// Update-type badges for a plan, matching the PR body's `<kbd>` style.
fn plan_badges(plan: &Plan) -> String {
    if plan.batch_items().is_some() {
        return "<kbd>batch</kbd>".into();
    }
    if plan.is_maintenance() {
        return "<kbd>maintenance</kbd>".into();
    }
    let mut badges: Vec<String> = plan
        .changes()
        .iter()
        .map(|c| c.kind.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|kind| format!("<kbd>{}</kbd>", escape(kind)))
        .collect();
    if plan.changes().iter().any(|c| !c.advisories.is_empty()) {
        badges.push("<kbd>security</kbd>".into());
    }
    badges.join("&nbsp;")
}
/// A plan's title followed by its badges when it has any.
fn plan_line(plan: &Plan) -> String {
    let title = titled(plan);
    let badges = plan_badges(plan);
    if badges.is_empty() {
        title
    } else {
        format!("{title} {badges}")
    }
}
/// Rated severities as badges, worst first, each once with its count:
/// `<kbd>high</kbd>&nbsp;×2, <kbd>low</kbd>`. Empty when none are rated.
fn severity_summary(severities: impl IntoIterator<Item = Option<Severity>>) -> String {
    let mut counts: BTreeMap<Severity, usize> = BTreeMap::new();
    for severity in severities.into_iter().flatten() {
        *counts.entry(severity).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(severity, n)| {
            let word = severity.as_str();
            if n > 1 {
                format!("<kbd>{word}</kbd>&nbsp;×{n}")
            } else {
                format!("<kbd>{word}</kbd>")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
/// Package names as code spans in a sentence: "`a`", "`a` and `b`", "`a`,
/// `b` and `c`", and past four, a count of the rest.
fn listing(names: &[&str]) -> String {
    const SHOWN: usize = 4;
    let mut parts: Vec<String> = names.iter().take(SHOWN).map(|n| code(n)).collect();
    if names.len() > SHOWN {
        parts.push(format!("{} more", names.len() - SHOWN));
    }
    match parts.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
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
pub fn issue(inventory: &Inventory) -> Result<Option<&crate::receipts::Issue>, Error> {
    if !inventory.issues.complete {
        return Err(Error::Refused("incomplete issue inventory".into()));
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
        return Err(Error::Refused(
            "multiple bot-owned dependency dashboards; close duplicates".into(),
        ));
    }
    Ok(result)
}

/// Byte budget before the footer, so the footer always fits within `LIMIT`.
const CONTENT_LIMIT: usize = LIMIT - FOOTER.len();

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
    let mut open_prs = BTreeSet::<(u8, u64, String)>::new();
    let mut any_batchable_pr = false;
    let open_batch_pr = plans
        .iter()
        .find(|p| p.batch_items().is_some())
        .and_then(|plan| {
            prs.iter()
                .find(|t| t.pr.state == "open" && t.pr.head.branch == plan.action().branch)
                .map(|t| t.pr.number)
        });
    for row in merged.iter().filter(|row| row.merged_at.is_some()) {
        if let Some(number) = row.number {
            sections.entry("Merged".into()).or_default().insert((
                Some(number),
                format!(
                    "| #{number} | {} | {} |\n",
                    prose_cell(row.title.as_deref().unwrap_or("Update dependencies")),
                    prose_cell(row.merge_note.as_deref().unwrap_or("merged")),
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
        open_prs.insert((
            2,
            tracked.pr.number,
            format!("- #{} {}\n", tracked.pr.number, safe(&tracked.pr.title)),
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
            let note = tracked
                .merge_note()
                .map(|reason| format!(" — <sub>{}</sub>", safe(reason)))
                .unwrap_or_default();
            let line = format!("#{} {}{note}", tracked.pr.number, plan_line(plan));
            let (group, row) = if plan.batch_items().is_some() {
                (0, format!("- {line}\n"))
            } else if crate::pr::body::batchable(plan)
                && let Some(origin) = batchable_pr_origin(tracked)
            {
                any_batchable_pr = true;
                (1, checkbox(&line, Action::Batch(origin)))
            } else {
                (1, format!("- {line}\n"))
            };
            open_prs.insert((group, tracked.pr.number, row));
        } else if plan.held(&crate::plan::Hold::Batched) {
            // The batch PR is listed with the open PRs; these are its items.
            let badges = plan_badges(plan);
            let badges = if badges.is_empty() {
                "—".into()
            } else {
                badges
            };
            sections
                .entry("Batched".into())
                .or_default()
                .insert((None, format!("| {} | {badges} |\n", titled_cell(plan))));
        } else if plan.held(&crate::plan::Hold::ClosedByOwner) {
            sections
                .entry("Updates you closed".into())
                .or_default()
                .insert((
                    None,
                    checkbox(&plan_line(plan), Action::Recreate(plan.update_key())),
                ));
        } else if plan.action().reason.is_some() {
            let waiting = Waiting::of(plan);
            let row = if waiting.tickable() {
                checkbox(&plan_line(plan), Action::Create(plan.update_key()))
            } else {
                let failure = plan.failure().expect("every reason keeps its failure");
                let mut row = format!("- {}: {}\n", titled(plan), escape(&failure.summary));
                if let Some(detail) = &failure.detail {
                    row.push_str(&details(detail, "  "));
                }
                row
            };
            sections
                .entry(PENDING.into())
                .or_default()
                .insert((Some(waiting as u64), row));
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
                let image_ref = format!(
                    "{}:{}{}",
                    dep.package,
                    dep.current.value.as_deref().unwrap_or("unknown"),
                    pinned
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
                            format!(
                                "| {} | {} | {} |\n",
                                code_cell(&image_ref),
                                code_cell(&file.path),
                                prose_cell(reason),
                            ),
                        ),
                        None => (
                            IMAGES,
                            format!(
                                "| {} | {} | {} |\n",
                                code_cell(&image_ref),
                                code_cell(&file.path),
                                image_gap(check).replace('|', "\\|"),
                            ),
                        ),
                    };
                    sections
                        .entry(row.0.into())
                        .or_default()
                        .insert((None, row.1));
                }
                let current_val = dep.current.value.as_deref().unwrap_or("unknown");
                for held in &metadata.held_back {
                    let eligible = escape(&text::when(
                        held.target_eligible_at
                            .as_deref()
                            .unwrap_or(&held.eligible_at),
                    ));
                    // A hold by the repository's committed cooldown gets no
                    // tick: not even the Count can create it early.
                    let row = match &held.cooldown {
                        Some(cooldown) => (
                            Waiting::Cooldown,
                            format!(
                                "- **{}** {}&nbsp;→&nbsp;{} <kbd>{}</kbd> · {} · <sub>set in {}, eligible {}</sub>\n",
                                code(&dep.name),
                                code(current_val),
                                code(&held.version),
                                safe(held.update_type.as_str()),
                                code(&file.path),
                                code(&cooldown.file),
                                eligible,
                            ),
                        ),
                        None => (
                            Waiting::ReleaseAge,
                            checkbox(
                                &format!(
                                    "**{}** {}&nbsp;→&nbsp;{} <kbd>{}</kbd> · {} · <sub>eligible {}</sub>",
                                    code(&dep.name),
                                    code(current_val),
                                    code(&held.version),
                                    safe(held.update_type.as_str()),
                                    code(&file.path),
                                    eligible,
                                ),
                                Action::Create(held_key(
                                    &file.path,
                                    dep,
                                    &held.version,
                                    held.update_type.as_str(),
                                )),
                            ),
                        ),
                    };
                    sections
                        .entry(PENDING.into())
                        .or_default()
                        .insert((Some(row.0 as u64), row.1));
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
                .entry(format!("Detected: {}\0{}", dep.manager.as_str(), file.path))
                .or_default()
                .insert((
                    None,
                    format!(
                        "| {} | {} |\n",
                        code_cell(&dep.name),
                        code_cell(dep.current.value.as_deref().unwrap_or("unknown"))
                    ),
                ));
        }
    }
    for ((manager, package, source), lookup) in lookups {
        let mut row = checkbox(
            &format!(
                "**{}** (<kbd>{}</kbd> · {})",
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
    for notice in &repo.notices {
        let range = format!(
            "{}&nbsp;→&nbsp;{}",
            code_cell(&notice.current),
            code_cell(&notice.available)
        );
        if notice.is_template() {
            let change = match &notice.compare_url {
                Some(compare) => linked(&range, compare),
                None => range,
            };
            sections
                .entry("Template updates".into())
                .or_default()
                .insert((
                    None,
                    format!(
                        "| **{}** | {change} | {} |\n",
                        code_cell(&notice.name),
                        code_cell(&notice.path),
                    ),
                ));
        } else {
            let reason = notice.reason.as_deref().map_or("—".into(), prose_cell);
            sections
                .entry("Newer versions not proposed".into())
                .or_default()
                .insert((
                    None,
                    format!(
                        "| **{}** | {range} | {} | {reason} |\n",
                        code_cell(&notice.name),
                        code_cell(&notice.path),
                    ),
                ));
        }
    }
    for warning in &repo.tooling {
        let open: Vec<_> = prs
            .iter()
            .filter(|t| t.pr.state == "open" && warning.branches.contains(&t.pr.head.branch))
            .map(|t| format!("#{}", t.pr.number))
            .collect();
        let row = match &warning.silenced {
            Some(reason) => (
                TOOLING_SILENCED,
                format!(
                    "| {} | {} | {} |\n",
                    code_cell(warning.rule.as_str()),
                    prose_cell(reason),
                    if open.is_empty() {
                        "—".into()
                    } else {
                        open.join(", ")
                    },
                ),
            ),
            None => {
                let open = if open.is_empty() {
                    String::new()
                } else {
                    format!(" Open now: {}.", open.join(", "))
                };
                (
                    TOOLING,
                    format!(
                        "> [!CAUTION]\n> {}{open}\n\n",
                        tooling_line(
                            warning,
                            repo.default_branch
                                .as_deref()
                                .unwrap_or("the default branch")
                        )
                    ),
                )
            }
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
    if !vulnerabilities.is_empty() {
        render_vulnerabilities(&mut text, &vulnerabilities);
    }
    let mut sections: Vec<_> = sections.into_iter().collect();
    sections.sort_by_key(|(section, _)| {
        let rank = if section.starts_with(TOOLING) {
            1
        } else if section.starts_with(IMAGES) {
            2
        } else {
            3 + match section.as_str() {
                "Batched" => 1,
                "Merged" => 2,
                PENDING => 3,
                "Template updates" | "Newer versions not proposed" => 4,
                "Updates you closed" => 5,
                "Lookup warnings and errors" => 6,
                "Report warnings" => 7,
                _ => 8,
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
    let mut open_written = false;
    let write_open = |text: &mut String, open_prs: &BTreeSet<(u8, u64, String)>| {
        if open_prs.is_empty() {
            return;
        }
        let header = if any_batchable_pr {
            "\n## Open PRs\n\n<sub>Tick an open PR to add it to the batch PR.</sub>\n\n"
        } else {
            "\n## Open PRs\n\n"
        };
        if text.len() + header.len() + 150 > CONTENT_LIMIT {
            return;
        }
        text.push_str(header);
        for (_, _, row) in open_prs {
            if text.len() + row.len() + 150 > CONTENT_LIMIT {
                text.push_str("\nAdditional entries omitted to fit GitHub's issue body limit.\n");
                break;
            }
            text.push_str(row);
        }
    };
    for (section, rows) in sections {
        let rank = if section.starts_with(TOOLING) {
            1
        } else if section.starts_with(IMAGES) {
            2
        } else {
            4
        };
        if rank > 2 && !open_written {
            write_open(&mut text, &open_prs);
            open_written = true;
        }
        if section == PENDING {
            pending(&mut text, rows);
            continue;
        }
        if section.starts_with("Detected: ") && !outer {
            // One line stands for every file's list.
            text.push_str(&format!(
                "\n<details>\n<summary><strong>Every dependency found</strong> ({} in {} {})</summary>\n",
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
                    "{}\n<details>\n<summary><strong>Silenced</strong> ({})</summary>\n\n| Warning | Reason | Open PRs |\n| --- | --- | --- |\n",
                    if tooling {
                        String::new()
                    } else {
                        format!("\n## {TOOLING}\n")
                    },
                    rows.len()
                ),
                "\n</details>\n",
            )
        } else if section == IMAGES {
            (
                format!(
                    "\n## {IMAGES}\n\n> [!WARNING]\n> **These pinned images give the Count no usable package list**, so he can't tell whether a new digest fixes vulnerabilities. He still proposes their updates.\n\n| Image | File | Reason |\n| --- | --- | --- |\n"
                ),
                "",
            )
        } else if section == IMAGES_SILENCED {
            (
                format!(
                    "{}\n<details>\n<summary><strong>Silenced</strong> ({})</summary>\n\n| Image | File | Reason |\n| --- | --- | --- |\n",
                    if image_gaps {
                        String::new()
                    } else {
                        format!("\n## {IMAGES}\n")
                    },
                    rows.len()
                ),
                "\n</details>\n",
            )
        } else if section == "Batched" {
            let carried = open_batch_pr
                .map(|n| format!("<sub>Carried by #{n}.</sub>\n\n"))
                .unwrap_or_default();
            (
                format!("\n## Batched\n\n{carried}| Update | Type |\n| --- | --- |\n"),
                "",
            )
        } else if section == "Merged" {
            (
                "\n## Merged\n\n| PR | Update | Status |\n| --- | --- | --- |\n".into(),
                "",
            )
        } else if section == "Template updates" {
            (
                "\n## Template updates\n\n<sub>Run <code>copier update</code> to apply template updates.</sub>\n\n| Template | Change | File |\n| --- | --- | --- |\n".into(),
                "",
            )
        } else if section == "Newer versions not proposed" {
            (
                "\n## Newer versions not proposed\n\n| Package | Change | File | Reason |\n| --- | --- | --- | --- |\n".into(),
                "",
            )
        } else if section == "Updates you closed" {
            (
                "\n## Updates you closed\n\n<sub>Tick an update to bring it back as a new PR.</sub>\n\n".into(),
                "",
            )
        } else if section == "Lookup warnings and errors" {
            (
                "\n## Lookup warnings and errors\n\n<sub>Tick a package to retry its lookup.</sub>\n\n".into(),
                "",
            )
        } else if let Some(rest) = section.strip_prefix("Detected: ") {
            let (manager, path) = rest.split_once('\0').unwrap_or(("", rest));
            (
                format!(
                    "\n<details>\n<summary><code>{}</code> (<kbd>{}</kbd>, {} {})</summary>\n\n| Dependency | Current |\n| --- | --- |\n",
                    safe(path),
                    safe(manager),
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
        if text.len() + start.len() + end.len() + 150 > CONTENT_LIMIT {
            break;
        }
        text.push_str(&start);
        for (_, row) in rows {
            if text.len() + row.len() + end.len() + 150 > CONTENT_LIMIT {
                text.push_str("\nAdditional entries omitted to fit GitHub's issue body limit.\n");
                break;
            }
            text.push_str(&row);
        }
        text.push_str(end);
    }
    if !open_written {
        write_open(&mut text, &open_prs);
    }
    if outer {
        text.push_str("\n</details>\n");
    }
    text.push_str(FOOTER);
    text
}

const PENDING: &str = "Pending updates";

/// Why a pending update waits. The order is the order of the groups.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Waiting {
    ReleaseAge,
    Cooldown,
    Window,
    Concurrent,
    PerHour,
    Other,
}
impl Waiting {
    const ALL: [Self; 6] = [
        Self::ReleaseAge,
        Self::Cooldown,
        Self::Window,
        Self::Concurrent,
        Self::PerHour,
        Self::Other,
    ];
    fn of(plan: &Plan) -> Self {
        use crate::plan::Hold;
        if plan.held(&Hold::Window) {
            Self::Window
        } else if plan.held(&Hold::Concurrent) {
            Self::Concurrent
        } else if plan.held(&Hold::PerHour) {
            Self::PerHour
        } else {
            Self::Other
        }
    }
    /// Whether a tick creates its updates now.
    fn tickable(self) -> bool {
        !matches!(self, Self::Cooldown | Self::Other)
    }
    fn summary(self) -> &'static str {
        match self {
            Self::ReleaseAge => "Waiting for their release age",
            Self::Cooldown => "Held by the repository's cooldown",
            Self::Window => "Outside the update windows",
            Self::Concurrent => "prs.concurrent limit reached",
            Self::PerHour => "prs.perHour limit reached",
            Self::Other => "Held for other reasons",
        }
    }
    fn explanation(self) -> &'static str {
        match self {
            Self::ReleaseAge => {
                "Too fresh for the configured release age, each until the time shown."
            }
            Self::Cooldown => {
                "The repository's committed cooldown binds the Count too, so these wait for the time shown with no box to tick."
            }
            Self::Window => "They wait for the next update window.",
            Self::Concurrent => "As many of his PRs are open as prs.concurrent allows.",
            Self::PerHour => "He has opened as many PRs this hour as prs.perHour allows.",
            Self::Other => "No box here: each says what holds it.",
        }
    }
}

/// The pending updates, one collapsed group per reason they wait, with the
/// shared reason said once, bounded like every other section.
fn pending(text: &mut String, rows: BTreeSet<(Option<u64>, String)>) {
    const END: &str = "\n</details>\n";
    let heading = format!("\n## {PENDING}\n");
    if text.len() + heading.len() + 150 > CONTENT_LIMIT {
        return;
    }
    text.push_str(&heading);
    for waiting in Waiting::ALL {
        let group: Vec<_> = rows
            .iter()
            .filter(|(rank, _)| *rank == Some(waiting as u64))
            .map(|(_, row)| row)
            .collect();
        if group.is_empty() {
            continue;
        }
        let tick = if waiting.tickable() {
            " Tick one and the Count creates it now anyway."
        } else {
            ""
        };
        let start = format!(
            "\n<details>\n<summary><strong>{}</strong> ({})</summary>\n\n<sub>{}{tick}</sub>\n\n",
            waiting.summary(),
            group.len(),
            waiting.explanation()
        );
        if text.len() + start.len() + END.len() + 150 > CONTENT_LIMIT {
            return;
        }
        text.push_str(&start);
        for row in group {
            if text.len() + row.len() + END.len() + 150 > CONTENT_LIMIT {
                text.push_str("\nAdditional entries omitted to fit GitHub's issue body limit.\n");
                text.push_str(END);
                return;
            }
            text.push_str(row);
        }
        text.push_str(END);
    }
}

/// A collapsed block of tool output, indented under a list item.
fn details(output: &str, indent: &str) -> String {
    format!(
        "{indent}<details>\n{indent}<summary><strong>The tool's output</strong></summary>\n\n{}\n{indent}</details>\n",
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
            Self::Update(version) => format!("fixed by the update to {}", code_cell(version)),
            Self::NotProposed(version) => format!("fixed in {}, not proposed", code_cell(version)),
            Self::Closed(version) => {
                format!("fixed in {}, whose PR was closed", code_cell(version))
            }
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
/// Render the `## Known vulnerabilities` section: a PR-style `[!IMPORTANT]`
/// callout, a summary table with one row per vulnerable package/version (most
/// severe first), and a collapsed PR-style advisory details table.
fn render_vulnerabilities(
    text: &mut String,
    vulnerabilities: &BTreeMap<(String, String, String), Vulnerable<'_>>,
) {
    let mut groups: Vec<&Vulnerable<'_>> = vulnerabilities.values().collect();
    groups.sort_by_key(|g| (g.rank(), g.name.as_str(), g.version.as_str()));

    // Deduplicate advisories once per package (matching PR bodies), worst first.
    let mut distinct: BTreeMap<(usize, &str, &str), &patchula_report::Advisory> = BTreeMap::new();
    let mut packages: Vec<&str> = Vec::new();
    for group in &groups {
        if !packages.contains(&group.name.as_str()) {
            packages.push(&group.name);
        }
        for (advisory, _) in group.advisories.values() {
            distinct
                .entry((
                    severity_rank(advisory.severity),
                    group.name.as_str(),
                    advisory.id.as_str(),
                ))
                .or_insert(*advisory);
        }
    }
    let total_advisories = distinct.len();
    let badges = severity_summary(distinct.values().map(|a| a.severity));
    let badge_note = if badges.is_empty() {
        String::new()
    } else {
        format!(" ({badges})")
    };
    let header = format!(
        "\n## Known vulnerabilities\n\n> [!IMPORTANT]\n> **{} known vulnerabilit{}**{badge_note} in {}.\n\n| Package | Version | Severity | Advisories | File | Status |\n| --- | --- | --- | --- | --- | --- |\n",
        total_advisories,
        if total_advisories == 1 { "y" } else { "ies" },
        listing(&packages),
    );
    if text.len() + header.len() + 150 > CONTENT_LIMIT {
        return;
    }
    text.push_str(&header);
    for group in &groups {
        let row = group.table_row();
        if text.len() + row.len() + 150 > CONTENT_LIMIT {
            text.push_str("\nAdditional entries omitted to fit GitHub's issue body limit.\n");
            return;
        }
        text.push_str(&row);
    }

    // Detailed advisory table, collapsed just like PRs' security section.
    const MAX_ADVISORIES: usize = 60;
    let details_open = format!(
        "\n<details>\n<summary><strong>Advisory details</strong> ({total_advisories})</summary>\n\n| Advisory | Package | Severity | Fixed&nbsp;in | Summary |\n| --- | --- | --- | --- | --- |\n",
    );
    const DETAILS_CLOSE: &str = "\n</details>\n";
    const BUDGET_OMITTED: &str =
        "\nAdditional advisories omitted to fit GitHub's issue body limit.\n";
    if text.len() + details_open.len() + BUDGET_OMITTED.len() + DETAILS_CLOSE.len() + 150
        > CONTENT_LIMIT
    {
        return;
    }
    text.push_str(&details_open);
    let mut shown = 0usize;
    let mut budget_hit = false;
    for ((_, package, _), advisory) in distinct.into_iter().take(MAX_ADVISORIES) {
        let cves: Vec<_> = advisory
            .aliases
            .iter()
            .filter(|a| a.starts_with("CVE-"))
            .map(|a| code_cell(&text::inert(a)))
            .collect();
        let aliases = if cves.is_empty() {
            String::new()
        } else {
            format!(" ({})", cves.join(", "))
        };
        let severity = advisory
            .severity
            .map_or("—".into(), |s| format!("<kbd>{}</kbd>", s.as_str()));
        let fixed_in = advisory.fixed_in.as_deref().map_or("—".into(), code_cell);
        let summary = advisory.summary.as_deref().map_or("—".into(), prose_cell);
        let row = format!(
            "| {}{aliases} | {} | {severity} | {fixed_in} | {summary} |\n",
            linked(&id_cell(&advisory.id), &advisory.url),
            code_cell(package),
        );
        if text.len() + row.len() + BUDGET_OMITTED.len() + DETAILS_CLOSE.len() + 150 > CONTENT_LIMIT
        {
            budget_hit = true;
            break;
        }
        text.push_str(&row);
        shown += 1;
    }
    if budget_hit {
        text.push_str(BUDGET_OMITTED);
    } else if shown < total_advisories {
        let omitted = total_advisories - shown;
        text.push_str(&format!(
            "\n{omitted} additional advisor{} omitted.\n",
            if omitted == 1 { "y" } else { "ies" }
        ));
    }
    text.push_str(DETAILS_CLOSE);
}
impl Vulnerable<'_> {
    fn rank(&self) -> usize {
        self.advisories
            .values()
            .map(|(a, _)| severity_rank(a.severity))
            .min()
            .unwrap_or(Severity::ALL.len())
    }
    fn table_row(&self) -> String {
        let severity = Severity::ALL
            .get(self.rank())
            .map_or("—".into(), |s| format!("<kbd>{}</kbd>", s.as_str()));
        let files: Vec<_> = self.files.iter().map(|f| code_cell(f)).collect();
        let files = if files.len() > 5 {
            format!(
                "{}<br>and {} more",
                files[..5].join("<br>"),
                files.len() - 5
            )
        } else {
            files.join("<br>")
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
        let link =
            |advisory: &patchula_report::Advisory| linked(&id_cell(&advisory.id), &advisory.url);
        let aliases = |advisory: &patchula_report::Advisory| {
            let cves: Vec<_> = advisory
                .aliases
                .iter()
                .filter(|a| a.starts_with("CVE-"))
                .map(|a| code_cell(&text::inert(a)))
                .collect();
            if cves.is_empty() {
                String::new()
            } else {
                format!(" ({})", cves.join(", "))
            }
        };
        let mut advisories: Vec<_> = self.advisories.values().map(|(a, _)| *a).collect();
        advisories.sort_by_key(|a| (severity_rank(a.severity), a.id.clone()));
        let adv_cell = if let [advisory] = advisories[..] {
            format!("{}{}", link(advisory), aliases(advisory))
        } else {
            let shown: Vec<_> = advisories.iter().take(12).map(|a| link(a)).collect();
            let more = advisories.len() - shown.len();
            format!(
                "{} advisories ({}{})",
                advisories.len(),
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            )
        };
        format!(
            "| **{}** | {} | {severity} | {adv_cell} | {files} | {} |\n",
            code_cell(&self.name),
            code_cell(&self.version),
            capitalise(&fix),
        )
    }
}
fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
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
        .state::<Receipt>(&crate::repository_scope(repo))
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
) -> Result<(), Error> {
    let existing = issue(inventory)?;
    let path = format!("repos/{}/issues", repo.repository);
    if !config.prs.dashboard {
        if let Some(existing) = existing {
            client
                .patch::<Value>(
                    &format!("{path}/{}", existing.number),
                    json!({"state":"closed"}),
                )
                .await?;
        }
        // A closed dashboard is no longer the repository's dashboard.
        return Ok(cache
            .clear_state::<Receipt>(&crate::repository_scope(&repo.repository))
            .await?);
    }
    let digest = hash(body.as_bytes());
    let number = if let Some(existing) = existing {
        let number = existing.number as u64;
        let current = existing.body.as_str();
        if crate::pr::body::body_hash(current) != digest {
            // The run's controls were read from `current`, which the
            // early inventory may have sampled a whole extraction ago.
            // Re-read the one issue: a body that moved since is a
            // person's edit this run never saw, and writing over it
            // would lose it — the rerun reads the newer controls.
            let fresh: patchula_github::model::Issue =
                client.get_fresh(&format!("{path}/{number}")).await?;
            if fresh.body.as_deref() != Some(current) {
                return Err(Error::Raced(
                    "dependency dashboard changed under the run; rerun",
                ));
            }
            client
                .patch::<Value>(&format!("{path}/{number}"), json!({"body":body}))
                .await?;
        }
        number
    } else {
        client
            .post::<patchula_github::model::Issue>(&path, json!({"title":TITLE,"body":body}))
            .await?
            .number
    };
    Ok(cache
        .set_state(
            &crate::repository_scope(&repo.repository),
            &Receipt {
                number,
                actor: inventory.actor.clone(),
                hash: digest,
            },
        )
        .await?)
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
        use crate::plan::testing::action;
        let change = || crate::Change {
            value: Some("1.2.0".into()),
            version: Some("1.2.0".into()),
            release_notes: false,
            ..crate::plan::testing::change("shared")
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
                green: None,
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
