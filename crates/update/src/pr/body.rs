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
use patchula_notes::{Naming, escape_html};
use patchula_words::{Recheck, Severity, ToolingDetail, ToolingRule, Update as UpdateKind};
use std::collections::{BTreeMap, BTreeSet};

/// Byte caps on the intent: a section appends only while the text built so
/// far stays within its cap, so the release notes keep their room. The
/// update table and the maintenance file and moved-ref lists.
const TABLE_CAP: usize = 18_000;
/// The plan's own `bodyNotes`, after the table.
const BODY_NOTES_CAP: usize = 22_000;
/// The line that says some `bodyNotes` were cut or left out; reserved, so it
/// always fits.
const BODY_NOTES_OMITTED: &str =
    "\nSome body notes were omitted to keep the PR body within its size budget.\n";
/// The security headline and its collapsed advisory table together.
const ADVISORY_CAP: usize = 6_000;
/// The whole image section: its headline, warnings and notes before the
/// update table and its collapsed advisory table after it.
const IMAGE_CAP: usize = 13_000;
/// The image section's table of fixed advisories, within [`IMAGE_CAP`], so
/// the lines after it keep room.
const IMAGE_TABLE_CAP: usize = 8_000;
/// The callout that opens the image section's warnings; reserved whole
/// whenever there are warnings.
const IMAGE_WARNING: &str =
    "> [!WARNING]\n> **The Count can't tell whether this rebuild fixes any vulnerabilities.**\n";
/// The most a block break takes: two newlines after a cut line.
const BLOCK_BREAK: usize = 2;
/// What closes a collapsed block.
const CLOSE: &str = "\n</details>\n";
/// The bytes of advisory links a line on what a rebuild fixes lists
/// before it counts the rest.
const FIXED_LINKS: usize = 3_000;
/// The same for the advisories a rebuild brings in.
const BROUGHT_LINKS: usize = 1_500;
/// The room the intent and release notes leave the tail's fixed text: the
/// footer, the content and ownership markers, the re-check box and a
/// server advert.
const TAIL_RESERVE: usize = 2_500;
/// The line that ends the notes when the room or the fetches ran out;
/// reserved from the start, so it always fits.
const OUT_OF_ROOM: &str = "<sub>Out of room for more release notes — see the linked source repositories for the rest.</sub>\n";
/// The least room a release-notes fetch is worth asking for.
const MIN_NOTES_ROOM: usize = 1_000;
/// The most bytes one package's release notes take.
const NOTES_SECTION_CAP: usize = 12_000;
/// The release-notes fetches one body makes at most.
pub(crate) const RELEASES_PER_BODY: usize = 8;
/// The largest reviewed-targets marker the batch controls carry.
const TARGETS_CAP: usize = 12_000;
/// The longest source or compare link the update table carries.
const LINK: usize = 512;
/// The salt in the content hash. Changing how a body renders changes it, so
/// every open PR is rewritten once into the new layout and reused after.
const RENDER: &str = "notes-render-v7";
/// The line between his sections and the controls: who opened the PR, with
/// the avatar pinned to the running release like the rebasing banner.
const FOOTER: &str = concat!(
    "---\n\n<p>\n<a href=\"https://count-patchula.sweepy.dev\"><img align=\"left\" src=\"https://raw.githubusercontent.com/sweepies/count-patchula/v",
    env!("CARGO_PKG_VERSION"),
    "/assets/logo/bat.svg\" width=\"52\" alt=\"Count Patchula\"></a>\n<strong><a href=\"https://count-patchula.sweepy.dev\">Count Patchula</a></strong> opened this PR<br>\n<sub>The Count keeps watch over this repo. He works for free, but if you find him helpful, <a href=\"https://github.com/sponsors/sweepies\">consider supporting his development</a>.</sub><br clear=\"left\">\n</p>\n"
);
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
/// One row of the update table: the changes that move one package from one
/// version to the same target, whichever files they sit in.
struct Row<'a> {
    first: &'a Change,
    target: String,
    security: bool,
    files: Vec<&'a str>,
}
/// Where a row's package and version range link: the source repository and
/// the compare view, as the release-notes fetch found them.
#[derive(Clone, Default)]
struct Link {
    source: Option<String>,
    compare: Option<String>,
}
impl Row<'_> {
    /// The row as the table shows it, linked where `link` has a link.
    fn render(&self, link: Option<&Link>) -> String {
        let name = format!(
            "**{}**",
            cell(&self.first.package)
                .replace('[', "\\[")
                .replace(']', "\\]")
        );
        let package = match link.and_then(|l| l.source.as_deref()) {
            Some(url) => format!("[{name}]({url})"),
            None => name,
        };
        let range = format!(
            "{}&nbsp;→&nbsp;{}",
            code_cell(&self.first.current),
            code_cell(&self.target)
        );
        let range = match link.and_then(|l| l.compare.as_deref()) {
            Some(url) => format!("[{range}]({url})"),
            None => range,
        };
        let security = if self.security {
            "&nbsp;<kbd>security</kbd>"
        } else {
            ""
        };
        let files: Vec<_> = self.files.iter().map(|f| code_cell(f)).collect();
        format!(
            "| {package} | {range} | <kbd>{}</kbd>{security} | {} |\n",
            cell(self.first.kind.as_str()),
            files.join("<br>")
        )
    }
}
/// The rows of `changes`' table, in plan order: changes that share a
/// package, a version, a target and a kind share a row.
fn rows(changes: &[Change]) -> Vec<Row<'_>> {
    let mut rows: Vec<Row<'_>> = Vec::new();
    for c in changes {
        let target = crate::plan::with_digest(
            c.value
                .as_deref()
                .or(c.version.as_deref())
                .unwrap_or("unchanged"),
            c.digest.as_deref(),
        );
        let security = !c.advisories.is_empty();
        match rows.iter_mut().find(|r| {
            r.first.source == c.source
                && r.first.registry == c.registry
                && r.first.package == c.package
                && r.first.current == c.current
                && r.target == target
                && r.first.kind == c.kind
        }) {
            Some(row) => {
                row.security |= security;
                if !row.files.contains(&c.file.as_str()) {
                    row.files.push(&c.file);
                }
            }
            None => rows.push(Row {
                first: c,
                target,
                security,
                files: vec![&c.file],
            }),
        }
    }
    rows
}
/// A source or compare link as a Markdown link target inside a table, or
/// `None` when it is too long or not a web address.
fn link_target(url: &str) -> Option<String> {
    if url.len() > LINK
        || !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_control)
    {
        return None;
    }
    Some(
        url.replace('\\', "%5C")
            .replace('`', "%60")
            .replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
            .replace('|', "%7C")
            .replace('<', "%3C")
            .replace('>', "%3E"),
    )
}
/// The visible text the content marker hashes, in parts: everything before
/// the table's rows, the rows, and everything after them. The rows stay
/// apart so `finish` can link them once the fetches name their sources.
struct Intent<'a> {
    before: String,
    rows: Vec<Row<'a>>,
    after: String,
}
impl Intent<'_> {
    /// The text, each row linked where `links` has its link.
    fn text(&self, links: &BTreeMap<usize, Link>) -> String {
        let mut text = self.before.clone();
        for (at, row) in self.rows.iter().enumerate() {
            text.push_str(&row.render(links.get(&at)));
        }
        text.push_str(&self.after);
        text
    }
}
impl Bodies<'_> {
    /// The body's visible start — the cautions, the security headlines, the
    /// update table, the collapsed advisory tables and the plan's own notes
    /// — and the content marker hashing it, so an existing body can be
    /// reused when nothing it records changed.
    pub(crate) fn intent(&self, plan: &Plan) -> (String, String) {
        let (intent, _, marker) = self.compose(plan);
        (intent.text(&BTreeMap::new()), marker)
    }
    /// The intent in its parts, the batch section and the content marker.
    fn compose<'a>(&self, plan: &'a Plan) -> (Intent<'a>, String, String) {
        let mut text = tooling_section(plan.callouts());
        let (security, advisories) = security_section(plan.changes());
        let (image, image_advisories) = image_section(plan.changes());
        text.push_str(&security);
        text.push_str(&image);
        // A plan without changes, lockfile maintenance's, has no table rows,
        // so it gets no table either.
        if !plan.changes().is_empty() {
            text.push_str("| Package | Change | Type | File |\n| --- | --- | --- | --- |\n");
        }
        if plan.is_maintenance() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "Scheduled {} using the repository's mise tool versions.\n",
                safe(self.maintenance_group)
            ));
            // The lists take what `TABLE_CAP` leaves after the text so far.
            let mut lists = crate::body::Body::new(TABLE_CAP, text.len());
            for file in &plan.action().files {
                lists.push(&format!("\n- {}", cell(&file.path)), usize::MAX);
            }
            moved_section(&mut lists, plan.moved_refs());
            if plan.deleted().contains(patchula_lockfiles::ACTIONS_LOCK) {
                lists.push(
                    "\n\nEvery workflow `actions.lock` listed is gone, so the lockfile is removed.\n",
                    usize::MAX,
                );
            }
            text.push_str(lists.text());
        }
        let mut length = text.len();
        let mut shown = Vec::new();
        let mut after = String::new();
        for row in rows(plan.changes()) {
            let line = row.render(None);
            if length + line.len() > TABLE_CAP {
                after.push_str("\nAdditional updates omitted from this table to keep the PR body within its size budget.\n");
                break;
            }
            length += line.len();
            shown.push(row);
        }
        if plan.action().lockfile_pending {
            after.push_str("\nLockfile pending: regenerate and validate it before merging.\n");
        }
        for details in [advisories, image_advisories] {
            if !details.is_empty() {
                after.push('\n');
                after.push_str(&details);
            }
        }
        let body_notes: BTreeSet<_> = plan.changes().iter().flat_map(|c| &c.body_notes).collect();
        // Each note is fitted whole or cut where a block ends, with what it
        // leaves open closed; a note with no room left is skipped and a
        // shorter one after it still tried. A note cut or left out is never
        // silent: one reserved line says so.
        let mut notes = crate::body::Body::new(
            BODY_NOTES_CAP,
            length + after.len() + BODY_NOTES_OMITTED.len(),
        );
        let mut omitted = false;
        for note in body_notes {
            if note.trim().is_empty() {
                continue;
            }
            let room = notes.room(usize::MAX).saturating_sub(2);
            let (kept, whole) = patchula_notes::fit_within(&safe(note), room);
            omitted |= !whole;
            let kept = kept.trim_end_matches('\n');
            if !kept.trim().is_empty() {
                notes.push(&format!("\n{kept}\n"), usize::MAX);
            }
        }
        if omitted {
            notes.push_reserved(BODY_NOTES_OMITTED);
        }
        after.push_str(notes.text());
        let intent = Intent {
            before: text,
            rows: shown,
            after,
        };
        let batch = plan
            .batch_items()
            .map(crate::batch::render)
            .unwrap_or_default();
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
            RENDER,
            &intent.text(&BTreeMap::new()),
            &batch,
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
        (intent, batch, content_marker)
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
    /// The bytes `finish` reserves for this plan beside the visible text,
    /// for budget checks before the body is rendered.
    pub(crate) fn reserved(&self, plan: &Plan) -> usize {
        let controls = controls(plan);
        let permit = self
            .branch_permits
            .get(&plan.action().branch)
            .map(crate::dashboard::permit_marker);
        let batch = plan
            .batch_items()
            .map(crate::batch::render)
            .unwrap_or_default();
        fixed(
            self.tail(
                plan,
                &" ".repeat(markers::CONTENT_MARKER_LEN),
                &controls,
                permit.as_deref(),
            )
            .len(),
            batch.len(),
        )
    }
    /// The body under construction for `plan`: the intent is already in it,
    /// and each [`Draft::next`] names a release-notes fetch still worth its
    /// room. The caller fetches; the draft decides.
    pub(crate) fn draft<'a>(&'a self, plan: &'a Plan) -> Draft<'a> {
        let (intent, batch, marker) = self.compose(plan);
        let controls = controls(plan);
        let permit = self
            .branch_permits
            .get(&plan.action().branch)
            .map(crate::dashboard::permit_marker);
        let unlinked = intent.text(&BTreeMap::new());
        // The intent, the release notes and the links share the room the
        // budget leaves after his controls and the batch section.
        let notes = crate::body::Body::new(
            BODY_LIMIT - TAIL_RESERVE,
            controls.len() + batch.len() + unlinked.len() + BLOCK_BREAK + OUT_OF_ROOM.len(),
        );
        // One range of notes needs no name; several each name their package.
        let ranges: BTreeSet<_> = plan.changes().iter().filter_map(notes_key).collect();
        Draft {
            bodies: self,
            plan,
            ends_in_break: unlinked.is_empty() || unlinked.ends_with("\n\n"),
            intent,
            links: BTreeMap::new(),
            naming: if ranges.len() > 1 {
                Naming::Named
            } else {
                Naming::Only
            },
            changes: plan.changes().iter(),
            pending: None,
            releases: BTreeSet::new(),
            notes,
            marker,
            controls,
            batch,
            permit,
            done: false,
            failed: false,
        }
    }
    /// Everything after the visible text, hidden markers included, in the
    /// order `finish` writes it: the footer, the age permits, the server
    /// advert, the content marker, the re-check box, the batch controls and
    /// the ownership marker. The batch box is the tail's lowest priority: it
    /// joins only when the whole body still fits his budget.
    fn tail(
        &self,
        plan: &Plan,
        content_marker: &str,
        controls: &str,
        permit: Option<&str>,
    ) -> String {
        let mut tail = String::from(FOOTER);
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
            "\n- [ ] Re-check this PR {timing} {}\n",
            markers::RECHECK
        ));
        let ownership = markers::ownership(&markers::Marker::new(
            plan.identity().to_owned(),
            plan.update_key(),
        ));
        let with_controls = format!("{tail}{controls}\n{ownership}\n");
        if fixed(with_controls.len(), 0) <= BODY_LIMIT {
            with_controls
        } else {
            format!("{tail}\n{ownership}\n")
        }
    }
}
/// The bytes a body keeps beside its visible text: the start marker's
/// line, the tail, the batch section and the block break before them.
fn fixed(tail: usize, batch: usize) -> usize {
    markers::START.len() + 1 + tail + batch + BLOCK_BREAK
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
    /// The intent in its parts; the notes follow it.
    intent: Intent<'a>,
    /// Whether the intent ends in a blank line, so the first section needs
    /// no break of its own.
    ends_in_break: bool,
    /// The links each table row gained from its fetch, by row.
    links: BTreeMap<usize, Link>,
    /// Whether the notes name their packages.
    naming: Naming,
    changes: std::slice::Iter<'a, Change>,
    /// The change whose `next` key waits for its [`push`](Draft::push).
    pending: Option<&'a Change>,
    /// The keys already fetched: a duplicate asks for nothing twice.
    releases: BTreeSet<(PackageId, String, String)>,
    /// The release notes so far, one section per fetch, within the room
    /// the intent and the links leave.
    notes: crate::body::Body,
    /// The content marker `intent` computed.
    marker: String,
    /// The batch controls and their reservation, from `draft`.
    controls: String,
    /// The batch section of a batch PR: its removal list and items marker.
    batch: String,
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
            // A change with nothing to fetch, or one already fetched, asks
            // for no room, so it never ends the notes.
            let Some(key) = notes_key(change) else {
                continue;
            };
            if self.releases.contains(&key) {
                continue;
            }
            if self.releases.len() >= RELEASES_PER_BODY
                || self.notes.room(usize::MAX) < MIN_NOTES_ROOM
            {
                let missing = self.block_break();
                self.notes.push_reserved(missing);
                self.notes.push_reserved(OUT_OF_ROOM);
                self.done = true;
                return None;
            }
            self.releases.insert(key.clone());
            self.pending = Some(change);
            return Some(key);
        }
        self.done = true;
        None
    }
    /// What the text lacks of a blank line before the next block.
    fn block_break(&self) -> &'static str {
        let text = self.notes.text();
        if text.is_empty() {
            if self.ends_in_break { "" } else { "\n" }
        } else if text.ends_with("\n\n") {
            ""
        } else if text.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        }
    }
    /// The answer to the fetch [`next`](Draft::next) named: its links into
    /// the table rows it answers, then its notes into the body, within the
    /// room left. A transient failure says so in its section and keeps the
    /// body from being reused, so the next run fetches again.
    pub(crate) fn push(&mut self, fetched: &Fetched) {
        let change = self
            .pending
            .take()
            .expect("push answers the fetch next() named");
        let link = match fetched {
            Fetched::Read(notes) => Link {
                source: notes.source.as_ref().and_then(|s| link_target(&s.url)),
                compare: notes.compare.as_deref().and_then(link_target),
            },
            Fetched::Lasting(Some(source)) => Link {
                source: link_target(&source.url),
                compare: None,
            },
            Fetched::Lasting(None) | Fetched::Transient => Link::default(),
        };
        if link.source.is_some() || link.compare.is_some() {
            let key = notes_key(change);
            for (at, row) in self.intent.rows.iter().enumerate() {
                if self.links.contains_key(&at) || notes_key(row.first) != key {
                    continue;
                }
                // The links never take the room the package's own notes
                // need to render at all.
                let more = row.render(Some(&link)).len() - row.render(None).len();
                if self.notes.room(usize::MAX) >= more + BLOCK_BREAK + patchula_notes::MIN_RENDER
                    && self.notes.reserve(more)
                {
                    self.links.insert(at, link.clone());
                }
            }
        }
        let missing = self.block_break();
        self.notes.push(missing, usize::MAX);
        let budget = self.notes.room(NOTES_SECTION_CAP);
        let section = match fetched {
            Fetched::Read(notes) => notes.render(&change.package, self.naming, budget),
            Fetched::Lasting(None) => {
                patchula_notes::Notes::default().render(&change.package, self.naming, budget)
            }
            Fetched::Lasting(Some(source)) => {
                unreadable_notes(&change.package, self.naming, source, budget)
            }
            Fetched::Transient => {
                self.failed = true;
                failed_notes(&change.package, self.naming, budget)
            }
        };
        // Each section was rendered within its budget; one that would not
        // fit whole is left out rather than cut mid-tag.
        if section.len() <= self.notes.room(usize::MAX) {
            self.notes.push(&section, usize::MAX);
        }
    }
    /// Whether a fetch failed in a way a later run may not: the finished
    /// body is then not settled.
    pub(crate) fn unsettled(&self) -> bool {
        self.failed
    }
    /// The finished body: the start marker, the visible text, the batch
    /// section and the tail's footer and hidden markers. The tail is built
    /// first and the visible text fitted into exactly the room it and the
    /// batch section leave: a cut lands where a block ends and closes any
    /// fence or collapsed block it left open, so nothing after it — the
    /// batch section, the footer, the checkboxes — is ever swallowed, and no
    /// marker is ever cut.
    pub(crate) fn finish(self) -> String {
        let mut visible = self.intent.text(&self.links);
        visible.push_str(self.notes.text());
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
        let budget = BODY_LIMIT.saturating_sub(fixed(tail.len(), self.batch.len()));
        let (visible, _) = patchula_notes::fit_within(visible.trim_end_matches('\n'), budget);
        let visible = visible.trim_end_matches('\n');
        let mut body = String::with_capacity(BODY_LIMIT);
        body.push_str(markers::START);
        body.push('\n');
        body.push_str(visible);
        if !visible.is_empty() {
            // The footer opens with a rule, which right under a line of
            // text would turn it into a heading.
            body.push_str("\n\n");
        }
        body.push_str(&self.batch);
        body.push_str(&tail);
        body
    }
}
/// What a release-notes fetch came back with.
pub(crate) enum Fetched {
    /// The notes.
    Read(patchula_notes::Notes),
    /// A failure every run would repeat, with the source repository when
    /// it resolved before the notes failed.
    Lasting(Option<patchula_notes::Repository>),
    /// A failure a later run may not repeat.
    Transient,
}
/// `line` when it fits `budget`, else nothing: a one-line note is never cut
/// mid-tag.
fn within(line: String, budget: usize) -> String {
    if line.len() <= budget {
        line
    } else {
        String::new()
    }
}
/// The line for a package whose source resolved but whose notes could not
/// be read, linking the source when it fits `budget`.
fn unreadable_notes(
    package: &str,
    naming: Naming,
    source: &patchula_notes::Repository,
    budget: usize,
) -> String {
    let subject = patchula_notes::subject(package, naming);
    let url = escape_html(&source.url);
    let linked = format!(
        "<sub>The Count could not read the release notes{subject} — <a href=\"{url}\">view upstream</a>.</sub>\n"
    );
    if url.len() <= LINK && linked.len() <= budget {
        linked
    } else {
        within(
            format!("<sub>The Count could not read the release notes{subject}.</sub>\n"),
            budget,
        )
    }
}
/// The line for a package whose release-notes fetch failed this run.
fn failed_notes(package: &str, naming: Naming, budget: usize) -> String {
    within(
        format!(
            "<sub>The Count could not read the release notes{} this run; he will try again on the next.</sub>\n",
            patchula_notes::subject(package, naming)
        ),
        budget,
    )
}
/// Whether `plan` can join the batch PR: neither the batch PR itself nor
/// lockfile maintenance, and small enough for the reviewed-targets marker.
pub(crate) fn batchable(plan: &Plan) -> bool {
    plan.batch_items().is_none()
        && !plan.is_maintenance()
        && markers::hidden(markers::TARGETS, &plan.changes()).len() <= TARGETS_CAP
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
        "- [ ] Add to the batch PR *(closes this PR and moves the update there)* {}\n{targets}\n",
        markers::ADD,
    )
}
/// Why the Count closed a pull request: the marker his closure body ends
/// with.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Closure {
    /// The update is superseded or already satisfied.
    Obsolete,
    /// The update moved into the batch PR with this number.
    Batched(u64),
}
/// The body a closure leaves behind: what the PR said, less any rebasing
/// banner, cut to make room, then the closure's marker. For a batched PR, a
/// note pointing to the batch PR sits at the top of his section so closing
/// needs no separate comment. The marker keeps the closure from reading as
/// an ordinary close: a batched closure never suppresses, and an obsolete
/// one is still his.
pub(crate) fn closure(body: &str, why: Closure) -> String {
    let (marker, note) = match why {
        Closure::Obsolete => (markers::OBSOLETE, None),
        Closure::Batched(number) => (
            markers::BATCHED,
            Some(format!(
                "> [!NOTE]\n> Moved to the batch PR #{number}. Tick this update's box there to bring it back.\n\n"
            )),
        ),
    };
    let body = crate::pr::banner::without_banner(body);
    let note_len = note.as_ref().map_or(0, String::len);
    let mut text = markers::truncate_marked(&body, closure_budget(marker, note_len));
    if let Some(note) = note {
        text = insert_top(&text, &note);
    }
    text.push_str("\n\n");
    text.push_str(marker);
    text
}
/// Insert `note` at the top of the Count's section (right after his start
/// marker line when present, preserving any foreign text ahead of it), or
/// at the start of `body` when it carries no section markers.
fn insert_top(body: &str, note: &str) -> String {
    let Some((before, section, after)) = markers::regions(body) else {
        return format!("{note}{body}");
    };
    let (head, rest) = match section.strip_prefix(markers::START) {
        Some(rest) if rest.starts_with("\r\n") => section.split_at(markers::START.len() + 2),
        Some(rest) if rest.starts_with('\n') => section.split_at(markers::START.len() + 1),
        Some(_) => {
            let (head, rest) = section.split_at(markers::START.len());
            return format!("{before}{head}\n{note}{rest}{after}");
        }
        None => ("", section),
    };
    format!("{before}{head}{note}{rest}{after}")
}
/// Whether two bodies carry the same ownership marker — the same writer
/// and the same update — so a freshened PR is still the one this run read.
pub(crate) fn same_owner(a: &str, b: &str) -> bool {
    markers::ownership_marker(a) == markers::ownership_marker(b)
}
/// The title a plan's PR gets: its commit message on one line, in GitHub's
/// limit.
pub(crate) fn title(plan: &Plan) -> String {
    patchula_datasource::upstream::truncate(&plan.action().message.replace(['\n', '\r'], " "), 250)
        .to_owned()
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
/// limit less any prepended note, the `"\n\n"` separator and `marker`
/// appended after it. `truncate_marked` never cuts the Count's own section,
/// which stays within `BODY_LIMIT`, so only the text around it gives way.
fn closure_budget(marker: &str, extra: usize) -> usize {
    FOREIGN_LIMIT - "\n\n".len() - marker.len() - extra
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
/// The severities of a set of advisories as badges, worst first, each
/// once with its count: `<kbd>high</kbd>&nbsp;×2, <kbd>low</kbd>`.
fn severity_badges(severities: impl IntoIterator<Item = Option<Severity>>) -> String {
    let mut counts: BTreeMap<(bool, Option<Severity>), usize> = BTreeMap::new();
    for severity in severities {
        *counts.entry((severity.is_none(), severity)).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|((_, severity), n)| {
            let word = severity.map_or("unrated", Severity::as_str);
            if n > 1 {
                format!("<kbd>{word}</kbd>&nbsp;×{n}")
            } else {
                format!("<kbd>{word}</kbd>")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
/// Names as code spans in a sentence: "`a`", "`a` and `b`", "`a`, `b` and
/// `c`", and past four, a count of the rest.
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
/// A count of vulnerabilities as the headline words it.
fn vulnerabilities(n: usize) -> String {
    format!("{n} known vulnerabilit{}", if n == 1 { "y" } else { "ies" })
}
/// Advisories the changes leave behind: a headline for above the update
/// table and the collapsed table of advisories for below it, both within
/// [`ADVISORY_CAP`].
fn security_section(changes: &[crate::Change]) -> (String, String) {
    // Each advisory once per package, worst first.
    let mut fixed: BTreeMap<(bool, Option<Severity>, &str, &str), &patchula_report::Advisory> =
        BTreeMap::new();
    for change in changes.iter().filter(|c| c.image.is_none()) {
        for advisory in &change.advisories {
            fixed
                .entry((
                    advisory.severity.is_none(),
                    advisory.severity,
                    advisory.id.as_str(),
                    change.package.as_str(),
                ))
                .or_insert(advisory);
        }
    }
    if fixed.is_empty() {
        return (String::new(), String::new());
    }
    let mut packages: Vec<&str> = Vec::new();
    for change in changes.iter().filter(|c| c.image.is_none()) {
        if !change.advisories.is_empty() && !packages.contains(&change.package.as_str()) {
            packages.push(&change.package);
        }
    }
    let headline = format!(
        "> [!IMPORTANT]\n> **Fixes {}** ({}) in {}.\n\n",
        vulnerabilities(fixed.len()),
        severity_badges(fixed.values().map(|a| a.severity)),
        listing(&packages)
    );
    let mut head = crate::body::Body::new(ADVISORY_CAP / 2, 0);
    head.push(&headline, usize::MAX);
    let head = head.into_text();
    let by_package = packages.len() > 1;
    let opening = format!(
        "<details>\n<summary><strong>Security advisories</strong> ({} fixed)</summary>\n\n{}",
        fixed.len(),
        if by_package {
            "| Advisory | Package | Severity | Fixed&nbsp;in | Summary |\n| --- | --- | --- | --- | --- |\n"
        } else {
            "| Advisory | Severity | Fixed&nbsp;in | Summary |\n| --- | --- | --- | --- |\n"
        }
    );
    let omitted = "\nThe Count left some advisories out to keep this description short.\n";
    let mut details = crate::body::Body::new(
        ADVISORY_CAP - head.len(),
        opening.len() + omitted.len() + CLOSE.len(),
    );
    details.push_reserved(&opening);
    let mut left_out = false;
    for ((_, _, _, package), advisory) in &fixed {
        let cves: Vec<_> = advisory
            .aliases
            .iter()
            .filter(|a| a.starts_with("CVE-"))
            .map(|a| code_cell(a))
            .collect();
        let package = if by_package {
            format!(" {} |", code_cell(package))
        } else {
            String::new()
        };
        let row = format!(
            "| {}{} |{package} <kbd>{}</kbd> | {} | {} |\n",
            linked(&id_cell(&advisory.id), &advisory.url),
            if cves.is_empty() {
                String::new()
            } else {
                format!(" ({})", cves.join(", "))
            },
            cell(advisory.severity.map_or("unrated", Severity::as_str)),
            advisory.fixed_in.as_deref().map_or("—".into(), code_cell),
            prose_cell(advisory.summary.as_deref().unwrap_or("")),
        );
        if details.room(usize::MAX) < row.len() {
            left_out = true;
            break;
        }
        details.push(&row, usize::MAX);
    }
    if left_out {
        details.push_reserved(omitted);
    }
    details.push_reserved(CLOSE);
    (head, details.into_text())
}
/// What the Count found comparing each rebuilt image's old and new SBOMs:
/// for above the update table, the headline on what the rebuilds fix, what
/// they bring in, one warning per image he can't vouch for, and plain
/// notes; for below it, the collapsed table of fixed advisories.
fn image_section(changes: &[crate::Change]) -> (String, String) {
    let mut rows = Vec::new();
    let mut listed = BTreeSet::new();
    let mut verified: BTreeMap<&str, Option<Severity>> = BTreeMap::new();
    let mut fixing: Vec<&str> = Vec::new();
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
            let Some(advisory) = change.advisories.iter().find(|a| a.id == id) else {
                listed.insert(id.to_owned());
                continue;
            };
            verified.insert(&advisory.id, advisory.severity);
            if !fixing.contains(&change.package.as_str()) {
                fixing.push(&change.package);
            }
            for fixed in check.fixed.iter().filter(|f| f.id == id) {
                let row = format!(
                    "| {} | <kbd>{}</kbd> | {} | {} | {} | {} |\n",
                    linked(&id_cell(&fixed.id), &advisory.url),
                    cell(advisory.severity.map_or("unrated", Severity::as_str)),
                    prose_cell(&fixed.package),
                    code_cell(&fixed.old),
                    fixed.new.as_deref().map_or("removed".into(), code_cell),
                    prose_cell(&fixed.platforms.join(", ")),
                );
                if !rows.iter().any(|(_, r)| r == &row) {
                    // Worst first; within a severity, in the order OSV's
                    // ids sort, as found.
                    let key = (advisory.severity.is_none(), advisory.severity, rows.len());
                    rows.push((key, row));
                }
            }
        }
        for id in &check.introduced {
            match check.introduced_details.iter().find(|a| &a.id == id) {
                Some(advisory) => {
                    let shown = format!(
                        "{} ({})",
                        linked(&id_cell(id), &advisory.url),
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
                prose_cell(check.os.as_deref().unwrap_or("an OS"))
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
                prose_cell(check.detail.as_deref().unwrap_or("no answer"))
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
    // The collapsed table first: the headline above the update table takes
    // what it leaves of the section's cap.
    let mut details = String::new();
    if !rows.is_empty() {
        let opening = format!(
            "<details>\n<summary><strong>Image advisories</strong> ({} fixed)</summary>\n\n| Advisory | Severity | Package | Old | New | Platforms |\n| --- | --- | --- | --- | --- | --- |\n",
            verified.len()
        );
        rows.sort_by_key(|(key, _)| *key);
        let mut table = crate::body::Body::new(IMAGE_TABLE_CAP, 0);
        table.push(&opening, usize::MAX);
        for (_, row) in &rows {
            if table.room(usize::MAX) < row.len() {
                break;
            }
            table.push(row, usize::MAX);
        }
        details.push_str(table.text());
        if !listed.is_empty() {
            details.push_str(&format!(
                "\nAnd {} more listed by OSV: {}.\n",
                listed.len(),
                id_links(&listed, FIXED_LINKS)
            ));
        }
        details.push_str(CLOSE);
    }
    // The section always ends on a block break, so the table after it
    // starts its own block; with warnings, their callout and the breaks
    // around them are reserved too, so a full section never drops or cuts
    // them.
    let reserve = if warnings.is_empty() {
        BLOCK_BREAK
    } else {
        3 * BLOCK_BREAK + IMAGE_WARNING.len()
    };
    let mut text = crate::body::Body::new(IMAGE_CAP - details.len(), reserve);
    if !rows.is_empty() {
        let more = if listed.is_empty() {
            String::new()
        } else {
            format!(
                ", and OSV.dev lists {} more that the new image no longer has",
                listed.len()
            )
        };
        text.push(
            &format!(
                "> [!IMPORTANT]\n> **Fixes {}** ({}) in the OS packages of {}{more}.\n\n",
                vulnerabilities(verified.len()),
                severity_badges(verified.values().copied()),
                listing(&fixing)
            ),
            usize::MAX,
        );
    } else if !listed.is_empty() {
        text.push(
            &format!(
                "OSV.dev lists {} advisor{} that the new image no longer has, but the Count read no record that confirms one, so he doesn't count this rebuild as a security fix: {}.\n\n",
                listed.len(),
                if listed.len() == 1 { "y" } else { "ies" },
                id_links(&listed, FIXED_LINKS)
            ),
            usize::MAX,
        );
    }
    let also = if text.text().is_empty() { "" } else { " also" };
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
        text.push(
            &format!(
                "The new image{also} brings in {}.\n\n",
                introduced.join("; ")
            ),
            usize::MAX,
        );
    }
    if !warnings.is_empty() {
        end_block(&mut text);
        text.push_reserved(IMAGE_WARNING);
        for warning in warnings {
            text.push(&format!(">\n> {warning}\n"), usize::MAX);
        }
        end_block(&mut text);
    }
    for note in notes {
        text.push(&format!("{note}\n\n"), usize::MAX);
    }
    end_block(&mut text);
    (text.into_text(), details)
}
/// End `text`'s last block with a blank line, out of the reserve, unless
/// it is empty or already ends so.
fn end_block(text: &mut crate::body::Body) {
    let current = text.text();
    let missing = if current.is_empty() || current.ends_with("\n\n") {
        ""
    } else if current.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    text.push_reserved(missing);
}
/// Advisory IDs as osv.dev links, as many as fit in `budget` bytes, with a
/// count of the rest.
fn id_links(ids: &BTreeSet<String>, budget: usize) -> String {
    let ordered = patchula_lookup::images::most_severe(ids.iter().map(String::as_str));
    let mut links = Vec::new();
    let mut length = 0;
    for id in &ordered {
        let link = linked(&id_cell(id), &format!("https://osv.dev/vulnerability/{id}"));
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
fn moved_section(text: &mut crate::body::Body, moved: &[patchula_lockfiles::MovedRef]) {
    if moved.is_empty() {
        return;
    }
    text.push(
        "\n\nRefs that moved upstream, now locked at their new commits:\n",
        usize::MAX,
    );
    let mut workflows: BTreeMap<&str, Vec<&patchula_lockfiles::MovedRef>> = BTreeMap::new();
    for entry in moved {
        workflows.entry(&entry.workflow).or_default().push(entry);
    }
    for (workflow, entries) in workflows {
        text.push(&format!("\n- {}", code(workflow)), usize::MAX);
        for entry in entries {
            text.push(
                &format!(
                    "\n  - {} now resolves to {}; the lock had {}",
                    code(&entry.dependency),
                    code(short(&entry.to)),
                    code(short(&entry.from))
                ),
                usize::MAX,
            );
        }
    }
    text.push("\n", usize::MAX);
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
    crate::text::code(patchula_datasource::upstream::truncate(text, 240))
}
/// A table cell of generated text: package names, versions, files. Those are
/// data and stay as they are.
fn cell(text: &str) -> String {
    patchula_datasource::upstream::truncate(
        &safe(text).replace('|', "\\|").replace(['\n', '\r'], " "),
        240,
    )
    .to_owned()
}
/// Generated text as an inline code span inside a table cell: its pipes
/// escaped so they cannot end the cell.
fn code_cell(text: &str) -> String {
    code(text).replace('|', "\\|")
}
/// A table cell of third-party prose, such as an advisory summary or an
/// SBOM's package name: cut first, then made inert and given no tag of its
/// own, its backslashes escaped before its pipes so neither ends the cell,
/// and no cut splits an escape.
fn prose_cell(text: &str) -> String {
    let text = patchula_datasource::upstream::truncate(text, 240).replace(['\n', '\r'], " ");
    crate::text::inert(&text)
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
/// An advisory id from a third party as a cell or link label: cut first,
/// inert, with no tag, and its backslashes escaped before the pipes and
/// brackets, so no bracket can end the label early.
fn id_cell(id: &str) -> String {
    let id = patchula_datasource::upstream::truncate(id, 240).replace(['\n', '\r'], " ");
    crate::text::inert(&id)
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
/// `label` linked to `url`, or plain `label` when `url` is no safe link.
fn linked(label: &str, url: &str) -> String {
    match link_target(url) {
        Some(url) => format!("[{label}]({url})"),
        None => label.to_owned(),
    }
}

#[cfg(test)]
mod tests;
