//! What a pull request body says and how the Count writes it: the visible
//! sections, the hidden markers and the byte budget that keeps them inside
//! GitHub's limit. Pure — the lifecycle fetches what a [`Draft`] asks for,
//! hands the notes back, and writes what `finish` returns.
use crate::markers;
use crate::pull_requests::{BODY_LIMIT, body, checked, recheck_hashed, recheck_timing};
use crate::{Change, Plan};
use patchula_config::PackageId;
use patchula_github::model;
use patchula_manager::hash;
use patchula_words::{Recheck, Severity, ToolingDetail, ToolingRule, Update as UpdateKind};
use std::collections::{BTreeMap, BTreeSet};

/// Byte caps on the intent: a section appends only while the text built so
/// far stays within its cap, so the release notes keep their room. The
/// update table and the maintenance file and moved-ref lists.
const TABLE_CAP: usize = 18_000;
/// The plan's own `bodyNotes`, after the table.
const BODY_NOTES_CAP: usize = 22_000;
/// The security section's advisory rows.
const ADVISORY_CAP: usize = 6_000;
/// The image section's table of fixed advisories.
const IMAGE_TABLE_CAP: usize = 8_000;
/// The image section's OSV-listed and warning lines.
const IMAGE_LISTED_CAP: usize = 11_000;
/// The image section's notes.
const IMAGE_NOTES_CAP: usize = 12_000;
/// The image section's line on what the new image brings in.
const IMAGE_BRINGS_IN_CAP: usize = 13_000;
/// The bytes of advisory links a line on what a rebuild fixes lists
/// before it counts the rest.
const FIXED_LINKS: usize = 3_000;
/// The same for the advisories a rebuild brings in.
const BROUGHT_LINKS: usize = 1_500;
/// The room the intent and release notes leave the tail's fixed text.
const TAIL_RESERVE: usize = 1_500;
/// The least room a release-notes fetch is worth asking for.
const MIN_NOTES_ROOM: usize = 1_000;
/// The most bytes one package's release notes take.
const NOTES_SECTION_CAP: usize = 12_000;
/// The release-notes fetches one body makes at most.
pub(crate) const RELEASES_PER_BODY: usize = 8;
/// The room kept beside a permit marker and the controls when the visible
/// text is fitted around them.
const PERMIT_RESERVE: usize = 1_000;
/// The largest reviewed-targets marker the batch controls carry.
const TARGETS_CAP: usize = 12_000;
/// What one run's pull request bodies share: the re-check timing, the
/// server to advertise, the branch age permits and the maintenance group
/// name the maintenance body prints.
pub(crate) struct Bodies<'a> {
    /// When a ticked re-check box takes effect, as the body words it.
    pub recheck: Recheck,
    /// The server URL the body advertises to the CLI, if any.
    pub server: Option<&'a str>,
    /// The release-age permits each branch carries, by branch.
    pub branch_permits: &'a BTreeMap<String, BTreeSet<String>>,
    /// The mise group name a maintenance body names.
    pub maintenance_group: &'a str,
}
impl Bodies<'_> {
    /// The body's visible start — the cautions, advisories, the update
    /// table and the plan's own notes — and the content marker hashing it,
    /// so an existing body can be reused when nothing it records changed.
    pub(crate) fn intent(&self, plan: &Plan) -> (String, String) {
        let mut text = tooling_section(plan.callouts());
        text.push_str(&security_section(plan.changes()));
        text.push_str(&image_section(plan.changes()));
        text.push_str("| Package | Change | Type | File |\n| --- | --- | --- | --- |\n");
        if plan.is_maintenance() {
            text.push_str(&format!(
                "\nScheduled {} using the repository's mise tool versions.\n",
                safe(self.maintenance_group)
            ));
            for file in &plan.action().files {
                append(&mut text, &format!("\n- {}", cell(&file.path)), TABLE_CAP);
            }
            moved_section(&mut text, plan.moved_refs());
            if plan.deleted().contains(patchula_lockfiles::ACTIONS_LOCK) {
                append(
                    &mut text,
                    "\n\nEvery workflow `actions.lock` listed is gone, so the lockfile is removed.\n",
                    TABLE_CAP,
                );
            }
        }
        for c in plan.changes() {
            let target = crate::plan::with_digest(
                c.value
                    .as_deref()
                    .or(c.version.as_deref())
                    .unwrap_or("unchanged"),
                c.digest.as_deref(),
            );
            let row = format!(
                "| {} | {} → {} | {} | {} |\n",
                cell(&c.package),
                cell(&c.current),
                cell(&target),
                cell(c.kind.as_str()),
                cell(&c.file)
            );
            if text.len() + row.len() > TABLE_CAP {
                text.push_str("\nAdditional updates omitted from this table to keep the PR body within its size budget.\n");
                break;
            }
            text.push_str(&row);
        }
        if plan.action().lockfile_pending {
            text.push_str("\nLockfile pending: regenerate and validate it before merging.\n");
        }
        if let Some(items) = plan.batch_items() {
            text.push_str(&crate::batch::render(items));
        }
        let body_notes: BTreeSet<_> = plan.changes().iter().flat_map(|c| &c.body_notes).collect();
        for note in body_notes {
            append(&mut text, &format!("\n{}\n", safe(note)), BODY_NOTES_CAP);
        }
        let ranges: Vec<_> = plan
            .changes()
            .iter()
            .map(|c| {
                (
                    &c.source,
                    &c.package,
                    &c.current,
                    &c.current_version,
                    &c.version,
                    &c.value,
                    &c.kind,
                    c.release_notes,
                )
            })
            .collect();
        let inputs = (
            "notes-render-v6",
            &text,
            ranges,
            recheck_hashed(self.recheck),
            self.branch_permits.get(&plan.action().branch),
        );
        // Only an advertised server joins the hash, so bodies written without
        // one keep their hash.
        let content_hash = hash(
            &match &self.server {
                None => serde_json::to_vec(&inputs),
                Some(url) => serde_json::to_vec(&(inputs, url)),
            }
            .expect("serializable body intent"),
        );
        let content_marker = format!("{}{content_hash} -->", markers::CONTENT);
        (text, content_marker)
    }
    /// Whether `existing`'s body already says everything this plan would
    /// write: it carries this run's content marker in his section, nobody
    /// ticked its re-check box, the plan does not ask for a rerun and the
    /// ownership marker still names the plan's identity and update.
    pub(crate) fn reusable(
        &self,
        plan: &Plan,
        existing: Option<&model::PullRequest>,
        marker: &str,
    ) -> bool {
        existing.is_some_and(|pr| {
            markers::section(body(pr)).is_some_and(|s| s.contains(marker))
                && !checked(pr)
                && !plan.retry()
                && markers::ownership_marker(body(pr))
                    .is_some_and(|m| m.identity == plan.identity() && m.update == plan.update_key())
        })
    }
    /// The bytes `tail` reserves for this plan, for budget checks before
    /// the body is rendered.
    pub(crate) fn reserved(&self, plan: &Plan) -> usize {
        let controls = controls(plan);
        let permit = self
            .branch_permits
            .get(&plan.action().branch)
            .map(crate::dashboard::permit_marker);
        markers::START.len()
            + 1
            + self
                .tail(
                    plan,
                    &" ".repeat(markers::CONTENT_MARKER_LEN),
                    &controls,
                    permit.as_deref(),
                )
                .len()
    }
    /// The body under construction for `plan`: the intent is already in it,
    /// and each [`Draft::next`] names a release-notes fetch still worth its
    /// room. The caller fetches; the draft decides.
    pub(crate) fn draft<'a>(&'a self, plan: &'a Plan) -> Draft<'a> {
        let (intent, marker) = self.intent(plan);
        let controls = controls(plan);
        let permit = self
            .branch_permits
            .get(&plan.action().branch)
            .map(crate::dashboard::permit_marker);
        // The intent and the release notes share the room the budget leaves
        // after his controls.
        let mut notes = crate::body::Body::new(BODY_LIMIT - TAIL_RESERVE, controls.len());
        notes.push(&intent, usize::MAX);
        Draft {
            bodies: self,
            plan,
            changes: plan.changes().iter(),
            pending: None,
            releases: BTreeSet::new(),
            notes,
            marker,
            controls,
            permit,
            done: false,
            failed: false,
        }
    }
    /// Everything after the release notes, hidden markers included, in the
    /// order `finish` writes it: the age permits, the server advert, the
    /// content marker, the re-check box, the batch controls and the
    /// ownership marker. The batch box is the tail's lowest priority: it
    /// joins only when the whole body still fits his budget.
    fn tail(
        &self,
        plan: &Plan,
        content_marker: &str,
        controls: &str,
        permit: Option<&str>,
    ) -> String {
        let mut tail = String::new();
        if let Some(marker) = permit {
            tail.push('\n');
            tail.push_str(marker);
        }
        if let Some(url) = self.server {
            tail.push('\n');
            tail.push_str(&crate::advertise::marker(url));
        }
        tail.push('\n');
        tail.push_str(content_marker);
        let timing = recheck_timing(self.recheck);
        tail.push_str(&format!(
            "\n- [ ] Tick to re-check this PR {timing}. {}\n",
            markers::RECHECK
        ));
        let ownership = markers::ownership(&markers::Marker::new(
            plan.identity().to_owned(),
            plan.update_key(),
        ));
        let with_controls = format!("{tail}{controls}\n{ownership}\n");
        if markers::START.len() + 1 + with_controls.len() <= BODY_LIMIT {
            with_controls
        } else {
            format!("{tail}\n{ownership}\n")
        }
    }
}
/// The release-notes fetch `change` asks for: the package, the version it
/// is on and the version it moves to. `None` for a change with no upstream
/// notes to show: pins, changes opted out of notes, targets without a
/// version and packages from a private registry.
pub(crate) fn notes_key(change: &Change) -> Option<(PackageId, String, String)> {
    if change.kind == UpdateKind::Pin || !change.release_notes {
        return None;
    }
    let target = change.version.as_ref().or(change.value.as_ref())?;
    // A package from its own registry has no public release notes, and no
    // section saying so.
    let id = change.id();
    if !id.is_public() {
        return None;
    }
    Some((
        id,
        change
            .current_version
            .as_ref()
            .unwrap_or(&change.current)
            .clone(),
        target.clone(),
    ))
}
/// The body being written. The caller fetches; the draft decides what is
/// still worth fetching: [`next`](Draft::next) names a fetch only while the
/// body has room for its notes.
pub(crate) struct Draft<'a> {
    bodies: &'a Bodies<'a>,
    plan: &'a Plan,
    changes: std::slice::Iter<'a, Change>,
    /// The change whose `next` key waits for its [`push`](Draft::push).
    pending: Option<&'a Change>,
    /// The keys already fetched: a duplicate asks for nothing twice.
    releases: BTreeSet<(PackageId, String, String)>,
    /// The visible text so far: the intent, then one section per fetch.
    notes: crate::body::Body,
    /// The content marker `intent` computed.
    marker: String,
    /// The batch controls and their reservation, from `draft`.
    controls: String,
    /// The branch's release-age permit marker, if it has one.
    permit: Option<String>,
    /// No more fetches: the room ran out or the changes did.
    done: bool,
    /// A fetch failed: the finished body must not be reused as settled.
    failed: bool,
}
impl Draft<'_> {
    /// The next release-notes fetch worth its room, as `(package, current,
    /// target)`. `None` when the body is out of room — the "out of room"
    /// line is already in it — or the changes ran out.
    pub(crate) fn next(&mut self) -> Option<(PackageId, String, String)> {
        if self.done {
            return None;
        }
        for change in &mut self.changes {
            if self.releases.len() >= RELEASES_PER_BODY
                || self.notes.room(usize::MAX) < MIN_NOTES_ROOM
            {
                self.notes.push("\nOut of room for more release notes. The source repositories have the rest.\n", usize::MAX);
                self.done = true;
                return None;
            }
            let Some(key) = notes_key(change) else {
                continue;
            };
            if !self.releases.insert(key.clone()) {
                continue;
            }
            self.pending = Some(change);
            return Some(key);
        }
        self.done = true;
        None
    }
    /// The answer to the fetch [`next`](Draft::next) named, rendered into
    /// the body within the room left: the notes, or `None` when the fetch
    /// failed. A failed fetch says so in its section and keeps the body
    /// from being reused, so the next run fetches again.
    pub(crate) fn push(&mut self, notes: Option<&patchula_notes::Notes>) {
        let change = self
            .pending
            .take()
            .expect("push answers the fetch next() named");
        // Two bytes for the newlines around the section.
        let budget = self.notes.room(NOTES_SECTION_CAP + 2).saturating_sub(2);
        let section = match notes {
            Some(notes) => notes.render(&change.package, budget),
            None => {
                self.failed = true;
                failed_notes(&change.package)
            }
        };
        self.notes.push("\n", usize::MAX);
        self.notes.push(&section, usize::MAX);
        self.notes.push("\n", usize::MAX);
    }
    /// The finished body: the visible text fitted around the permit and
    /// controls, the start marker and the tail's hidden markers. Every
    /// hidden marker was reserved before the visible text was added, so
    /// the result always fits and no marker is ever cut.
    pub(crate) fn finish(self) -> String {
        let mut visible = self.notes.into_text();
        if let Some(marker) = &self.permit {
            // Keep policy evidence with the PR even if its local cache is lost.
            visible = patchula_notes::fit(
                &visible,
                BODY_LIMIT.saturating_sub(marker.len() + self.controls.len() + PERMIT_RESERVE),
            )
            .0;
        }
        // A body with a failed fetch carries a content marker no intent
        // computes, so `reusable` never takes it as settled.
        let marker = if self.failed {
            format!(
                "{}{} -->",
                markers::CONTENT,
                hash(format!("unsettled {}", self.marker).as_bytes())
            )
        } else {
            self.marker
        };
        let tail = self
            .bodies
            .tail(self.plan, &marker, &self.controls, self.permit.as_deref());
        let mut body = crate::body::Body::new(BODY_LIMIT, markers::START.len() + 1 + tail.len());
        body.push(&visible, usize::MAX);
        format!("{}\n{}{}", markers::START, body.text(), tail)
    }
}
/// The section for a package whose release-notes fetch failed this run.
fn failed_notes(package: &str) -> String {
    let package = markers::cut_bytes(&safe(package), 160).replace(['\n', '\r'], " ");
    format!(
        "### {package}\n\nThe Count could not read these release notes this run; he will try again on the next.\n"
    )
}
/// The batch controls his body offers: the "Add to batch" box and the
/// reviewed targets marker, when the group is small enough to record.
/// The reviewed targets travel with the PR, so batching keeps them even
/// after a newer release changes this branch's plan.
fn controls(plan: &Plan) -> String {
    if plan.batch_items().is_some() || plan.is_maintenance() {
        return String::new();
    }
    let targets = markers::hidden(markers::TARGETS, &plan.changes());
    if targets.len() > TARGETS_CAP {
        return String::new();
    }
    format!(
        "- [ ] Add to the batch PR. This one closes and the update moves there. {}\n{targets}\n",
        markers::ADD,
    )
}
/// Why the Count closed a pull request: the marker his closure body ends
/// with.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Closure {
    /// The update is superseded or already satisfied.
    Obsolete,
    /// The update moved into the batch PR.
    Batched,
}
/// The body a closure leaves behind: what the PR said, cut to make room,
/// then the closure's marker. The marker keeps the closure from reading as
/// an ordinary close: a batched closure never suppresses, and an obsolete
/// one is still his.
pub(crate) fn closure(body: &str, why: Closure) -> String {
    let marker = match why {
        Closure::Obsolete => markers::OBSOLETE,
        Closure::Batched => markers::BATCHED,
    };
    let mut text = markers::truncate_marked(body, closure_budget(marker));
    text.push_str("\n\n");
    text.push_str(marker);
    text
}
/// Whether two bodies carry the same ownership marker — the same writer
/// and the same update — so a freshened PR is still the one this run read.
pub(crate) fn same_owner(a: &str, b: &str) -> bool {
    markers::ownership_marker(a) == markers::ownership_marker(b)
}
/// The title a plan's PR gets: its commit message on one line, in GitHub's
/// limit.
pub(crate) fn title(plan: &Plan) -> String {
    markers::cut_bytes(&plan.action().message.replace(['\n', '\r'], " "), 250).to_owned()
}
/// The body with its re-check box unticked: the acknowledgement after a
/// tick was honoured.
pub(crate) fn unticked(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.trim_end().ends_with(markers::RECHECK) {
                line.replacen("- [x]", "- [ ]", 1)
                    .replacen("- [X]", "- [ ]", 1)
            } else {
                line.into()
            }
        })
        .collect()
}
/// The Count's rendering between whatever the PR's body has right now before
/// and after his section. Only the section of `text` is used, so a text that
/// was itself taken from an existing body, or from an older read, never
/// doubles or loses what others wrote, and a body he leaves alone comes out
/// byte for byte the same. A foreign edit inside his section is overwritten.
pub(crate) fn with_foreign(text: String, current: &str) -> String {
    let (Some((_, section, _)), Some((before, _, after))) =
        (markers::regions(&text), markers::regions(current))
    else {
        return text;
    };
    if before.len() + section.len() + after.len() > FOREIGN_LIMIT {
        tracing::warn!(
            foreign = before.len() + after.len(),
            "text others wrote in a PR body no longer fits beside the Count's own; dropped"
        );
        return format!("{section}\n");
    }
    format!("{before}{section}{after}")
}
/// GitHub's pull request body limit (65,536 characters), with room to spare.
/// The Count's own section stays within `BODY_LIMIT`, so more than 30,000
/// bytes of other people's text fit beside it.
const FOREIGN_LIMIT: usize = 65_000;
/// The most `truncate_marked` may keep of a body a closure rewrites: the
/// limit less the `"\n\n"` separator and `marker` appended after it.
/// `truncate_marked` never cuts the Count's own section, which stays within
/// `BODY_LIMIT`, so only the text around it gives way.
fn closure_budget(marker: &str) -> usize {
    FOREIGN_LIMIT - "\n\n".len() - marker.len()
}
/// The content hash of a body, as receipts record it: GitHub may return
/// CRLF for a body published with LF, so transport newlines do not count
/// as edits.
pub(crate) fn body_hash(text: &str) -> String {
    hash(text.replace("\r\n", "\n").as_bytes())
}
/// One caution per tooling warning on this PR, ahead of everything else.
fn tooling_section(callouts: &[crate::tooling::Callout]) -> String {
    let mut text = String::new();
    for callout in callouts {
        let branch = safe(&callout.branch);
        let body = match (callout.rule, callout.detail) {
            (ToolingRule::MergeWithoutCi, Some(ToolingDetail::Native)) => "**This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so nothing will have tested it when GitHub merges it.".to_owned(),
            (
                ToolingRule::MergeWithoutCi,
                Some(ToolingDetail::ReviewsOnly | ToolingDetail::TaggedRelease) | None,
            ) => "**This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits. He waits for at least one passing check before merging, so this PR will stay open.".to_owned(),
            (ToolingRule::MergeWithoutRequiredChecks, Some(ToolingDetail::ReviewsOnly)) => format!("**Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `{branch}` requires a review but no status checks, so once approved a red PR merges as readily as a green one. Add a required check in a ruleset."),
            (
                ToolingRule::MergeWithoutRequiredChecks,
                Some(ToolingDetail::Native | ToolingDetail::TaggedRelease) | None,
            ) => format!("**Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `{branch}` requires no status checks, so a red PR merges as readily as a green one. Add a required check in a ruleset."),
            (ToolingRule::SecurityMergeWithoutDelivery, Some(ToolingDetail::TaggedRelease)) => format!("**Merging this fix doesn't ship it.** It will merge on its own, and nothing runs when `{branch}` changes. The only workflow here that could deliver it runs on tags or releases, so it ships only if you cut one."),
            (
                ToolingRule::SecurityMergeWithoutDelivery,
                Some(ToolingDetail::Native | ToolingDetail::ReviewsOnly) | None,
            ) => format!("**Merging this fix ships nothing.** This PR fixes a known vulnerability and will merge on its own, but nothing in this repository builds, releases or deploys when `{branch}` changes. Whatever runs the old code stays bitten. If something outside this repository delivers it, say so in `warnings.quiet`."),
        };
        text.push_str("> [!CAUTION]\n> ");
        text.push_str(&body);
        text.push_str("\n\n");
    }
    text
}
/// Advisories the changes leave behind, after any tooling warnings.
fn security_section(changes: &[crate::Change]) -> String {
    let mut rows = BTreeSet::new();
    for change in changes.iter().filter(|c| c.image.is_none()) {
        for advisory in &change.advisories {
            let cves: Vec<_> = advisory
                .aliases
                .iter()
                .filter(|a| a.starts_with("CVE-"))
                .map(|a| safe(a))
                .collect();
            rows.insert(format!(
                "| [{}]({}){} | {} | {} | {} |\n",
                cell(&advisory.id),
                safe(&advisory.url),
                if cves.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", cves.join(", "))
                },
                cell(advisory.severity.map_or("unrated", Severity::as_str)),
                cell(&change.package),
                prose_cell(advisory.summary.as_deref().unwrap_or("")),
            ));
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    let mut text = format!(
        "> [!IMPORTANT]\n> This update fixes {} known vulnerabilit{}. The Count does not care for anyone else drawing blood.\n\n| Advisory | Severity | Package | Summary |\n| --- | --- | --- | --- |\n",
        rows.len(),
        if rows.len() == 1 { "y" } else { "ies" }
    );
    for row in rows {
        append(&mut text, &row, ADVISORY_CAP);
    }
    text.push('\n');
    text
}
/// What the Count found comparing each rebuilt image's old and new SBOMs:
/// the advisories fixed, then one line per image he can't vouch for, then
/// plain notes.
fn image_section(changes: &[crate::Change]) -> String {
    let mut rows = Vec::new();
    let mut listed = BTreeSet::new();
    let mut fixed_ids = BTreeSet::new();
    let mut brings_in = Vec::new();
    let mut brings_in_listed = BTreeSet::new();
    let mut warnings = BTreeSet::new();
    let mut notes = BTreeSet::new();
    for change in changes {
        let Some(check) = &change.image else {
            continue;
        };
        let image = code(&change.package);
        let order = patchula_lookup::images::most_severe(check.fixed.iter().map(|f| f.id.as_str()));
        for id in order {
            fixed_ids.insert(id.to_owned());
            let Some(advisory) = change.advisories.iter().find(|a| a.id == id) else {
                listed.insert(id.to_owned());
                continue;
            };
            for fixed in check.fixed.iter().filter(|f| f.id == id) {
                let row = format!(
                    "| [{}]({}) | {} | {} | {} | {} | {} |\n",
                    cell(&fixed.id),
                    safe(&advisory.url),
                    cell(advisory.severity.map_or("unrated", Severity::as_str)),
                    cell(&fixed.package),
                    cell(&fixed.old),
                    cell(fixed.new.as_deref().unwrap_or("removed")),
                    cell(&fixed.platforms.join(", ")),
                );
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
        }
        for id in &check.introduced {
            match check.introduced_details.iter().find(|a| &a.id == id) {
                Some(advisory) => {
                    let shown = format!(
                        "[{}]({}) ({})",
                        cell(id),
                        safe(&advisory.url),
                        cell(advisory.severity.map_or("unrated", Severity::as_str))
                    );
                    if !brings_in.contains(&shown) {
                        brings_in.push(shown);
                    }
                }
                None => {
                    brings_in_listed.insert(id.clone());
                }
            }
        }
        let previous = change
            .current
            .split_once('@')
            .map(|(_, digest)| digest.chars().take(14).collect::<String>())
            .unwrap_or_default();
        use patchula_report::ImageState;
        let warning = match check.state {
            ImageState::NoSbom => format!(
                "{image} publishes no SBOM for this image, and the Count doesn't open up image layers to look for himself. Treat it as an ordinary rebuild, or ask the publisher to attach an SBOM."
            ),
            ImageState::PreviousUnavailable => format!(
                "The previous {image} image (`{}…`) is no longer available from its registry, so he has nothing to compare the new one against. Some registries, quay.io among them, delete an image once its tag moves on.",
                cell(&previous)
            ),
            ImageState::SbomWithoutOs => format!(
                "The SBOM for {image} lists no OS packages, and it was made by {}, which doesn't look for them. The image may have none, or the SBOM may leave them out.",
                code(check.detail.as_deref().unwrap_or("an unknown tool"))
            ),
            ImageState::OsNotCovered => format!(
                "{image} is built on {}, and OSV.dev has no advisories for it.",
                cell(check.os.as_deref().unwrap_or("an OS"))
            ),
            ImageState::Unreadable
            | ImageState::PartialSources
            | ImageState::Checked
            | ImageState::NoOsPackages => String::new(),
        };
        if !warning.is_empty() {
            if check.silenced.is_none() {
                warnings.insert(warning);
            }
            continue;
        }
        let note = match check.state {
            ImageState::NoOsPackages => format!(
                "According to its SBOM, {image} has no OS packages, so there is nothing at the OS level for a rebuild to fix."
            ),
            ImageState::Unreadable => format!(
                "The Count couldn't read {image}'s SBOM on this run ({}). He'll try again on the next run.",
                cell(check.detail.as_deref().unwrap_or("no answer"))
            ),
            ImageState::PartialSources
            | ImageState::Checked
            | ImageState::SbomWithoutOs
            | ImageState::OsNotCovered
                if !check.fixed.is_empty() =>
            {
                String::new()
            }
            ImageState::PartialSources
            | ImageState::Checked
            | ImageState::SbomWithoutOs
            | ImageState::OsNotCovered
            | ImageState::PreviousUnavailable
            | ImageState::NoSbom => format!(
                "The Count compared {image}'s old and new package lists. OSV.dev knows of no vulnerability that the new one fixes."
            ),
        };
        if !note.is_empty() {
            notes.insert(note);
        }
        if check.state == ImageState::PartialSources {
            notes.insert(format!(
                "{image}'s SBOM doesn't say which source package its {} OS packages come from, so the Count checked them under their own names. Advisories filed under a different source name may be missing.",
                check.packages
            ));
        }
    }
    let mut text = String::new();
    if !rows.is_empty() {
        let verified = fixed_ids.len() - listed.len();
        text.push_str(&if listed.is_empty() {
            format!(
                "> [!IMPORTANT]\n> This rebuild fixes {verified} known vulnerabilit{} in the image's OS packages, according to OSV.dev. The Count does not care for anyone else drawing blood.\n\n",
                if verified == 1 { "y" } else { "ies" }
            )
        } else {
            format!(
                "> [!IMPORTANT]\n> This rebuild fixes known vulnerabilities in the image's OS packages. The Count read OSV.dev's record for the {verified} below, and OSV.dev lists {} more that the new image no longer has. The Count does not care for anyone else drawing blood.\n\n",
                listed.len()
            )
        });
        text.push_str("| Advisory | Severity | Package | Old | New | Platforms |\n| --- | --- | --- | --- | --- | --- |\n");
        for row in &rows {
            append(&mut text, row, IMAGE_TABLE_CAP);
        }
        if !listed.is_empty() {
            append(
                &mut text,
                &format!(
                    "\nAnd {} more listed by OSV: {}.\n",
                    listed.len(),
                    id_links(&listed, FIXED_LINKS)
                ),
                IMAGE_LISTED_CAP,
            );
        }
        text.push('\n');
    } else if !listed.is_empty() {
        append(
            &mut text,
            &format!(
                "OSV.dev lists {} advisor{} that the new image no longer has, but the Count read no record that confirms one, so he doesn't count this rebuild as a security fix: {}.\n\n",
                listed.len(),
                if listed.len() == 1 { "y" } else { "ies" },
                id_links(&listed, FIXED_LINKS)
            ),
            IMAGE_LISTED_CAP,
        );
    }
    let also = if text.is_empty() { "" } else { " also" };
    let mut introduced = brings_in;
    if !brings_in_listed.is_empty() {
        let links = id_links(&brings_in_listed, BROUGHT_LINKS);
        introduced.push(if introduced.is_empty() {
            format!(
                "{} advisor{} listed by OSV: {links}",
                brings_in_listed.len(),
                if brings_in_listed.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        } else {
            format!("and {} more listed by OSV: {links}", brings_in_listed.len())
        });
    }
    if !introduced.is_empty() {
        append(
            &mut text,
            &format!(
                "The new image{also} brings in {}.\n\n",
                introduced.join("; ")
            ),
            IMAGE_BRINGS_IN_CAP,
        );
    }
    if !warnings.is_empty() {
        text.push_str(
            "> [!WARNING]\n> **The Count can't tell whether this rebuild fixes any vulnerabilities.**\n",
        );
        for warning in warnings {
            append(&mut text, &format!(">\n> {warning}\n"), IMAGE_LISTED_CAP);
        }
        text.push('\n');
    }
    for note in notes {
        append(&mut text, &format!("{note}\n\n"), IMAGE_NOTES_CAP);
    }
    text
}
/// Advisory IDs as osv.dev links, as many as fit in `budget` bytes, with a
/// count of the rest.
fn id_links(ids: &BTreeSet<String>, budget: usize) -> String {
    let ordered = patchula_lookup::images::most_severe(ids.iter().map(String::as_str));
    let mut links = Vec::new();
    let mut length = 0;
    for id in &ordered {
        let link = format!("[{}](https://osv.dev/vulnerability/{})", cell(id), safe(id));
        if length + link.len() > budget {
            break;
        }
        length += link.len() + 2;
        links.push(link);
    }
    let rest = ordered.len() - links.len();
    let mut text = links.join(", ");
    if rest > 0 {
        text.push_str(&format!(" and {rest} not listed here"));
    }
    text
}
/// The refs a maintenance relock advanced in `actions.lock`, one line
/// each under its workflow; nothing when none moved.
fn moved_section(text: &mut String, moved: &[patchula_lockfiles::MovedRef]) {
    if moved.is_empty() {
        return;
    }
    append(
        text,
        "\n\nRefs that moved upstream, now locked at their new commits:\n",
        TABLE_CAP,
    );
    let mut workflows: BTreeMap<&str, Vec<&patchula_lockfiles::MovedRef>> = BTreeMap::new();
    for entry in moved {
        workflows.entry(&entry.workflow).or_default().push(entry);
    }
    for (workflow, entries) in workflows {
        append(text, &format!("\n- {}", code(workflow)), TABLE_CAP);
        for entry in entries {
            append(
                text,
                &format!(
                    "\n  - {} now resolves to {}; the lock had {}",
                    code(&entry.dependency),
                    code(short(&entry.to)),
                    code(short(&entry.from))
                ),
                TABLE_CAP,
            );
        }
    }
    append(text, "\n", TABLE_CAP);
}
/// A commit SHA as the moved-ref list shows it: its first seven characters.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}
fn safe(text: &str) -> String {
    text.replace("<!--", "&lt;!--").replace("-->", "--&gt;")
}
/// Text as an inline code span, cut to 240 bytes.
fn code(text: &str) -> String {
    crate::text::code(markers::cut_bytes(text, 240))
}
/// A table cell of generated text: package names, versions, files. Those are
/// data and stay as they are.
fn cell(text: &str) -> String {
    markers::cut_bytes(
        &safe(text).replace('|', "\\|").replace(['\n', '\r'], " "),
        240,
    )
    .to_owned()
}
/// A table cell of third-party prose, such as an advisory summary.
fn prose_cell(text: &str) -> String {
    cell(&crate::text::inert(text))
}
fn append(text: &mut String, addition: &str, limit: usize) {
    let remaining = limit.saturating_sub(text.len());
    if remaining > 0 {
        text.push_str(markers::cut_bytes(addition, remaining));
    }
}

#[cfg(test)]
mod tests;
