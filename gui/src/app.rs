//! What the window knows and what a press or a key does to it. Nothing here
//! draws; the views read this state, and the tests drive it directly.
use crate::{
    cli::{self, Applied, Failure, Launch, Source, Written},
    demo,
    model::{self, Call, Data, Update},
    time,
};
use patchula_report::Report;
use patchula_words::Update as UpdateKind;
use rune::{Image, ImageCache, TextEdit, Tx, Waker};
use rune_kit::{
    SortDir, TableState,
    diff::{DiffState, parse},
    natural_cmp,
    shell::PaneState,
};
use std::{
    collections::BTreeSet,
    sync::mpsc::{Receiver, Sender, channel},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Updates,
    Held,
    Dependencies,
    Templates,
    Warnings,
    Stats,
}

impl Page {
    /// Every page, in the TUI's tab order.
    pub const ALL: [Page; 6] = [
        Page::Updates,
        Page::Held,
        Page::Dependencies,
        Page::Templates,
        Page::Warnings,
        Page::Stats,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Updates => "Updates",
            Page::Held => "Held back",
            Page::Dependencies => "Dependencies",
            Page::Templates => "Templates",
            Page::Warnings => "Warnings",
            Page::Stats => "Stats",
        }
    }

    /// Whether the search field filters this page.
    pub fn searchable(self) -> bool {
        matches!(self, Page::Updates | Page::Held | Page::Dependencies)
    }
}

/// Where the report stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Lookup {
    Running,
    Ready,
    Failed(Failure),
}

/// What `patchula apply --dry-run` said the picks would write.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub picked: BTreeSet<usize>,
    pub calls: Vec<Call>,
    pub applied: Applied,
    /// Updates the calls write that nobody picked: the CLI picks by name and
    /// type, and these share both with a pick.
    pub extras: Vec<Written>,
}

/// What a write did.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub applied: Applied,
    /// A written dependency's version came from a lockfile, now stale.
    pub locked: bool,
}

/// Reviewing and writing the picked updates: one value, so the window is
/// never both planning and writing.
#[derive(Clone, Debug, PartialEq)]
pub enum Review {
    Closed,
    Planning,
    Ready(Plan),
    Writing(Plan),
    Done(Outcome),
    Failed(String),
}

/// A result coming back from a worker thread, with the request it answers.
enum Event {
    Looked(u64, Result<Box<Report>, Failure>),
    Planned(u64, BTreeSet<usize>, Vec<Call>, Result<Applied, Failure>),
    Wrote(u64, Result<Applied, Failure>),
}

/// The rows each table shows, as positions in [`Data`], after the search,
/// the type filter and the sort.
#[derive(Default)]
pub struct Rows {
    pub updates: Vec<usize>,
    pub held: Vec<usize>,
    pub found: Vec<usize>,
}

pub struct Tables {
    pub updates: TableState,
    pub held: TableState,
    pub found: TableState,
}

pub struct App {
    /// The launch, or why the command line could not be read.
    pub launch: Result<Launch, String>,
    pub data: Data,
    /// The moment ages count from: the clock, or the demo's own date.
    pub now: i64,
    pub lookup: Lookup,
    pub page: Page,
    pub search: TextEdit,
    pub filter: Option<UpdateKind>,
    pub rows: Rows,
    pub tables: Tables,
    /// Picked updates, as positions in `data.updates`.
    pub picked: BTreeSet<usize>,
    /// Updates written in this session, which can no longer be picked.
    pub applied: BTreeSet<usize>,
    pub review: Review,
    pub diff: DiffState,
    pub sidebar: PaneState,
    pub inspector: PaneState,
    pub logo: Option<Image>,
    pub images: Option<ImageCache>,
    /// The keys overlay is showing.
    pub help: bool,
    waker: Option<Waker>,
    sender: Sender<Event>,
    events: Receiver<Event>,
    /// The newest lookup and review requests; an answer to an older one is dropped.
    looking: u64,
    reviewing: u64,
}

/// The target a written update moved to, as [`Update::target`] writes it.
fn target(written: &Written) -> String {
    Update {
        id: String::new(),
        repository: String::new(),
        file: written.file.clone(),
        manager: written.manager,
        name: written.name.clone(),
        package: String::new(),
        current: written.current.clone(),
        value: written.value.clone(),
        version: written.version.clone(),
        digest: written.digest.clone(),
        kind: written.kind,
        published_at: None,
        fixes: Vec::new(),
        group: None,
        transitive: false,
        locked: false,
    }
    .target()
}

fn same(update: &Update, written: &Written) -> bool {
    update.manager == written.manager
        && update.file == written.file
        && update.name == written.name
        && update.kind == written.kind
        && update.target() == target(written)
}

impl App {
    pub fn new(launch: Result<Launch, String>) -> Self {
        let (sender, events) = channel();
        let mut app = App {
            launch,
            data: Data::default(),
            now: time::now(),
            lookup: Lookup::Running,
            page: Page::Updates,
            search: TextEdit::new(""),
            filter: None,
            rows: Rows::default(),
            tables: Tables {
                updates: TableState::new().key("updates-table"),
                held: TableState::new().key("held-table"),
                found: TableState::new().key("found-table"),
            },
            picked: BTreeSet::new(),
            applied: BTreeSet::new(),
            review: Review::Closed,
            diff: DiffState::new(Vec::new()),
            sidebar: PaneState::new(232.0).range(180.0, 360.0),
            inspector: PaneState::new(320.0).range(260.0, 460.0),
            logo: None,
            images: None,
            help: false,
            waker: None,
            sender,
            events,
            looking: 0,
            reviewing: 0,
        };
        match &app.launch {
            // The demo needs no thread, so it is there on the first frame.
            Ok(launch) if launch.source == Source::Demo => app.load(&demo::report()),
            Ok(_) => {}
            Err(error) => {
                app.lookup = Lookup::Failed(Failure {
                    message: error.clone(),
                    uninvited: false,
                })
            }
        }
        app
    }

    #[cfg(test)]
    pub fn demo() -> Self {
        Self::new(Launch::parse(&["--demo".to_owned()], None))
    }

    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
    }

    /// Whether this window shows the demo report.
    pub fn is_demo(&self) -> bool {
        matches!(&self.launch, Ok(launch) if launch.source == Source::Demo)
    }

    /// Why the picks cannot be written, if they cannot.
    pub fn read_only(&self) -> Option<&'static str> {
        self.launch.as_ref().ok().and_then(Launch::read_only)
    }

    /// Run `work` on a thread and wake the window when its event is sent.
    fn spawn(&self, work: impl FnOnce() -> Event + Send + 'static) {
        let (sender, waker) = (self.sender.clone(), self.waker.clone());
        std::thread::spawn(move || {
            // The window closing drops the receiver; there is nobody to tell.
            let _ = sender.send(work());
            if let Some(wake) = waker {
                wake();
            }
        });
    }

    /// Look up again.
    pub fn refresh(&mut self) {
        let Ok(launch) = self.launch.clone() else {
            return;
        };
        self.looking += 1;
        if launch.source == Source::Demo {
            self.load(&demo::report());
            return;
        }
        self.lookup = Lookup::Running;
        let request = self.looking;
        self.spawn(move || Event::Looked(request, cli::lookup(&launch).map(Box::new)));
    }

    /// Take the Count's defaults for a tree with no `patchula.pkl`, and look up again.
    pub fn use_defaults(&mut self) {
        if let Ok(launch) = &mut self.launch {
            launch.use_defaults();
        }
        self.refresh();
    }

    /// Show a report, keeping the picks that are still in it.
    pub fn load(&mut self, report: &Report) {
        let kept: BTreeSet<_> = self
            .picked
            .iter()
            .filter_map(|i| self.data.updates.get(*i))
            .map(Update::identity)
            .collect();
        self.data = Data::new(report);
        self.now = if self.is_demo() {
            time::parse(demo::NOW).unwrap_or_else(time::now)
        } else {
            time::now()
        };
        self.picked = self
            .data
            .updates
            .iter()
            .enumerate()
            .filter(|(_, update)| kept.contains(&update.identity()))
            .map(|(i, _)| i)
            .collect();
        self.applied.clear();
        self.review = Review::Closed;
        self.lookup = Lookup::Ready;
        self.arrange();
        // Start on the first row, so the inspector has something to say.
        if let Some(first) = self.rows.updates.first() {
            self.tables
                .updates
                .select(self.data.updates[*first].id.clone());
        }
    }

    /// Recompute the rows each table shows.
    pub fn arrange(&mut self) {
        let query = self.search.text.clone();
        let data = &self.data;
        let mut updates: Vec<usize> = (0..data.updates.len())
            .filter(|i| {
                let u = &data.updates[*i];
                self.filter.is_none_or(|kind| kind == u.kind)
                    && model::found(&query, &[&u.name, &u.package, &u.file, u.manager.as_str()])
            })
            .collect();
        if let Some((column, dir)) = self.tables.updates.sort() {
            updates.sort_by(|a, b| {
                let (a, b) = (&data.updates[*a], &data.updates[*b]);
                dir.apply(match column {
                    "current" => natural_cmp(&a.current, &b.current),
                    "target" => natural_cmp(&a.target(), &b.target()),
                    "type" => a.kind.rank().cmp(&b.kind.rank()),
                    // Newest first reads as "youngest": compare the dates backwards.
                    "age" => b.published_at.cmp(&a.published_at),
                    "file" => natural_cmp(&a.file, &b.file),
                    _ => natural_cmp(&a.name, &b.name),
                })
            });
        }
        let mut held: Vec<usize> = (0..data.held.len())
            .filter(|i| {
                let h = &data.held[*i];
                model::found(&query, &[&h.name, &h.file, h.manager.as_str()])
            })
            .collect();
        if let Some((column, dir)) = self.tables.held.sort() {
            held.sort_by(|a, b| {
                let (a, b) = (&data.held[*a], &data.held[*b]);
                dir.apply(match column {
                    "version" => natural_cmp(&a.version, &b.version),
                    "type" => a.kind.rank().cmp(&b.kind.rank()),
                    "eligible" => a.eligible_at.cmp(&b.eligible_at),
                    "file" => natural_cmp(&a.file, &b.file),
                    _ => natural_cmp(&a.name, &b.name),
                })
            });
        }
        let mut found: Vec<usize> = (0..data.found.len())
            .filter(|i| {
                let f = &data.found[*i];
                model::found(&query, &[&f.name, &f.package, &f.file, f.manager.as_str()])
            })
            .collect();
        if let Some((column, dir)) = self.tables.found.sort() {
            found.sort_by(|a, b| {
                let (a, b) = (&data.found[*a], &data.found[*b]);
                dir.apply(match column {
                    "manager" => a.manager.as_str().cmp(b.manager.as_str()),
                    "current" => natural_cmp(&a.current, &b.current),
                    "updates" => a.updates.cmp(&b.updates),
                    "file" => natural_cmp(&a.file, &b.file),
                    _ => natural_cmp(&a.name, &b.name),
                })
            });
        }
        self.rows = Rows {
            updates,
            held,
            found,
        };
    }

    /// Sort a table by a column, as a header click asks.
    pub fn sort(&mut self, page: Page, column: &str, dir: SortDir) {
        let table = match page {
            Page::Held => &mut self.tables.held,
            Page::Dependencies => &mut self.tables.found,
            _ => &mut self.tables.updates,
        };
        table.set_sort(column, dir);
        self.arrange();
    }

    pub fn set_filter(&mut self, filter: Option<UpdateKind>) {
        self.filter = filter;
        self.arrange();
    }

    /// The next type filter, as the TUI's `t` cycles: all, then each type.
    pub fn cycle_filter(&mut self) {
        let next = match self.filter {
            None => UpdateKind::RANKED.first().copied(),
            Some(kind) => UpdateKind::RANKED
                .get(usize::from(kind.rank()) + 1)
                .copied(),
        };
        self.set_filter(next);
    }

    /// The update under the Updates table's cursor.
    pub fn cursor(&self) -> Option<usize> {
        self.data.position(self.tables.updates.cursor()?)
    }

    /// Move the cursor of the table on screen by `delta` rows and scroll it
    /// into view. False when this page has no table or the table no rows.
    pub fn step(&mut self, delta: isize, tx: &mut Tx) -> bool {
        let data = &self.data;
        let (ids, table): (Vec<&String>, &mut TableState) = match self.page {
            Page::Updates => (
                self.rows
                    .updates
                    .iter()
                    .map(|i| &data.updates[*i].id)
                    .collect(),
                &mut self.tables.updates,
            ),
            Page::Held => (
                self.rows.held.iter().map(|i| &data.held[*i].id).collect(),
                &mut self.tables.held,
            ),
            Page::Dependencies => (
                self.rows.found.iter().map(|i| &data.found[*i].id).collect(),
                &mut self.tables.found,
            ),
            Page::Templates | Page::Warnings | Page::Stats => return false,
        };
        let Some(last) = ids.len().checked_sub(1) else {
            return false;
        };
        let at = table
            .cursor()
            .and_then(|cursor| ids.iter().position(|id| *id == cursor));
        let next = match at {
            Some(at) => at.saturating_add_signed(delta).min(last),
            None if delta > 0 => 0,
            None => last,
        };
        table.reveal(ids[next].clone(), tx);
        true
    }

    /// Whether the update at `index` can still be picked.
    pub fn pickable(&self, index: usize) -> bool {
        !self.applied.contains(&index)
    }

    /// Pick the update at `index`, or put it back.
    pub fn toggle(&mut self, index: usize) {
        if !self.pickable(index) || index >= self.data.updates.len() {
            return;
        }
        if !self.picked.remove(&index) {
            self.picked.insert(index);
        }
    }

    /// Pick every visible update, or clear them if all are picked already.
    pub fn toggle_visible(&mut self) {
        let visible: Vec<usize> = self
            .rows
            .updates
            .iter()
            .copied()
            .filter(|i| self.pickable(*i))
            .collect();
        if visible.iter().all(|i| self.picked.contains(i)) {
            for i in visible {
                self.picked.remove(&i);
            }
        } else {
            self.picked.extend(visible);
        }
    }

    /// Whether every visible update is picked, or some, for the header box.
    pub fn visible_picked(&self) -> (bool, bool) {
        let mut visible = self
            .rows
            .updates
            .iter()
            .filter(|i| self.pickable(**i))
            .peekable();
        if visible.peek().is_none() {
            return (false, false);
        }
        let (mut all, mut any) = (true, false);
        for i in visible {
            let picked = self.picked.contains(i);
            all &= picked;
            any |= picked;
        }
        (all, any)
    }

    /// Why the picks cannot be reviewed right now.
    pub fn review_blocked(&self) -> Option<String> {
        if let Some(reason) = self.read_only() {
            return Some(reason.to_owned());
        }
        self.picked
            .is_empty()
            .then(|| "Pick updates with their boxes, Space or A first.".to_owned())
    }

    /// Ask the CLI what the picks would write.
    pub fn plan(&mut self) {
        if self.review_blocked().is_some() {
            return;
        }
        let Ok(launch) = self.launch.clone() else {
            return;
        };
        let picked = self.picked.clone();
        let calls = model::calls(&self.data.updates, &picked);
        self.reviewing += 1;
        let request = self.reviewing;
        if launch.source == Source::Demo {
            let applied = demo::plan(&self.data.updates, &picked);
            self.planned(picked, calls, applied);
            return;
        }
        self.review = Review::Planning;
        self.spawn(move || {
            let result = cli::apply(&launch, &calls, false);
            Event::Planned(request, picked, calls, result)
        });
    }

    fn planned(&mut self, picked: BTreeSet<usize>, calls: Vec<Call>, applied: Applied) {
        let extras = applied
            .updates
            .iter()
            .filter(|written| {
                !picked
                    .iter()
                    .filter_map(|i| self.data.updates.get(*i))
                    .any(|update| same(update, written))
            })
            .cloned()
            .collect();
        let files = applied
            .files
            .iter()
            .flat_map(|patch| parse(&patch.diff))
            .collect();
        self.diff = DiffState::new(files);
        self.review = Review::Ready(Plan {
            picked,
            calls,
            applied,
            extras,
        });
    }

    /// Write the plan on screen.
    pub fn write(&mut self) {
        let Review::Ready(plan) = &self.review else {
            return;
        };
        let Ok(launch) = self.launch.clone() else {
            return;
        };
        let plan = plan.clone();
        self.reviewing += 1;
        let request = self.reviewing;
        if launch.source == Source::Demo {
            // The demo has no tree: it reports the plan as written.
            self.review = Review::Writing(plan.clone());
            self.wrote(plan.applied);
            return;
        }
        let calls = plan.calls.clone();
        self.review = Review::Writing(plan);
        self.spawn(move || Event::Wrote(request, cli::apply(&launch, &calls, true)));
    }

    fn wrote(&mut self, applied: Applied) {
        let written: Vec<usize> = self
            .data
            .updates
            .iter()
            .enumerate()
            .filter(|(_, update)| applied.updates.iter().any(|w| same(update, w)))
            .map(|(i, _)| i)
            .collect();
        let locked = written.iter().any(|i| self.data.updates[*i].locked);
        for i in written {
            self.picked.remove(&i);
            self.applied.insert(i);
        }
        self.review = Review::Done(Outcome { applied, locked });
    }

    /// Leave the review. A write under way finishes; its answer still lands.
    pub fn close_review(&mut self) {
        if !matches!(self.review, Review::Writing(_)) {
            self.reviewing += 1;
            self.review = Review::Closed;
        }
    }

    /// Take what the worker threads sent. True when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Looked(request, result) if request == self.looking => {
                    match result {
                        Ok(report) => self.load(&report),
                        Err(failure) => self.lookup = Lookup::Failed(failure),
                    }
                    changed = true;
                }
                Event::Planned(request, picked, calls, result) if request == self.reviewing => {
                    match result {
                        Ok(applied) => self.planned(picked, calls, applied),
                        Err(failure) => self.review = Review::Failed(failure.message),
                    }
                    changed = true;
                }
                Event::Wrote(request, result) if request == self.reviewing => {
                    match result {
                        Ok(applied) => self.wrote(applied),
                        Err(failure) => self.review = Review::Failed(failure.message),
                    }
                    changed = true;
                }
                // An answer to a request since replaced.
                Event::Looked(..) | Event::Planned(..) | Event::Wrote(..) => {}
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_is_loaded_before_the_first_frame() {
        let app = App::demo();
        assert_eq!(app.lookup, Lookup::Ready);
        assert_eq!(app.rows.updates.len(), app.data.updates.len());
        assert_eq!(app.cursor(), Some(0));
        assert!(app.review_blocked().is_some());
    }

    #[test]
    fn search_and_the_type_filter_narrow_the_rows() {
        let mut app = App::demo();
        app.set_filter(Some(UpdateKind::Major));
        assert!(!app.rows.updates.is_empty());
        assert!(
            app.rows
                .updates
                .iter()
                .all(|i| app.data.updates[*i].kind == UpdateKind::Major)
        );
        app.search = TextEdit::new("react");
        app.arrange();
        let names: Vec<_> = app
            .rows
            .updates
            .iter()
            .map(|i| app.data.updates[*i].name.as_str())
            .collect();
        assert_eq!(names, ["react", "react-dom"]);
        app.cycle_filter();
        assert_eq!(app.filter, Some(UpdateKind::Minor));
        assert_eq!(
            app.rows.updates.len(),
            1,
            "@tanstack/react-query is the minor one"
        );
    }

    #[test]
    fn sorting_follows_the_column() {
        let mut app = App::demo();
        app.sort(Page::Updates, "name", SortDir::Ascending);
        let first = &app.data.updates[app.rows.updates[0]];
        assert_eq!(first.name, "@tanstack/react-query");
        app.sort(Page::Updates, "type", SortDir::Ascending);
        assert_eq!(
            app.data.updates[app.rows.updates[0]].kind,
            UpdateKind::Major
        );
    }

    #[test]
    fn select_all_takes_the_visible_rows_and_then_clears_them() {
        let mut app = App::demo();
        app.set_filter(Some(UpdateKind::Patch));
        let visible = app.rows.updates.len();
        assert_eq!(app.visible_picked(), (false, false));
        app.toggle(app.rows.updates[0]);
        assert_eq!(app.visible_picked(), (false, true));
        app.toggle_visible();
        assert_eq!(app.picked.len(), visible);
        assert_eq!(app.visible_picked(), (true, true));
        app.toggle_visible();
        assert!(app.picked.is_empty());
    }

    #[test]
    fn a_review_plans_then_writes_and_written_updates_cannot_be_picked_again() {
        let mut app = App::demo();
        let serde = app
            .data
            .updates
            .iter()
            .position(|u| u.name == "serde")
            .unwrap();
        let lodash = app
            .data
            .updates
            .iter()
            .position(|u| u.name == "lodash")
            .unwrap();
        app.toggle(serde);
        app.toggle(lodash);
        app.plan();
        let Review::Ready(plan) = &app.review else {
            panic!("{:?}", app.review)
        };
        assert_eq!(plan.applied.updates.len(), 2);
        assert!(plan.extras.is_empty());
        assert_eq!(app.diff.files().len(), 2);
        app.write();
        let Review::Done(outcome) = &app.review else {
            panic!("{:?}", app.review)
        };
        assert!(outcome.locked, "lodash's version came from the lockfile");
        assert!(app.picked.is_empty());
        assert!(!app.pickable(serde));
        app.toggle(serde);
        assert!(app.picked.is_empty());
    }

    #[test]
    fn picks_survive_a_refresh_and_read_only_sources_cannot_review() {
        let mut app = App::demo();
        app.toggle(3);
        let identity = app.data.updates[3].identity();
        app.refresh();
        assert_eq!(app.picked.len(), 1);
        assert_eq!(
            app.data.updates[*app.picked.first().unwrap()].identity(),
            identity
        );

        let mut saved = App::new(Launch::parse(&["--report".into(), "r.json".into()], None));
        saved.load(&demo::report());
        saved.toggle(0);
        assert!(saved.review_blocked().unwrap().contains("saved report"));
        saved.plan();
        assert_eq!(saved.review, Review::Closed);
    }

    #[test]
    fn a_stale_answer_is_dropped() {
        let mut app = App::demo();
        app.sender
            .send(Event::Looked(
                app.looking + 7,
                Err(Failure {
                    message: "late".into(),
                    uninvited: false,
                }),
            ))
            .unwrap();
        assert!(!app.poll());
        assert_eq!(app.lookup, Lookup::Ready);
    }
}
