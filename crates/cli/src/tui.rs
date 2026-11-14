//! `patchula tui`: browse a lookup report, and in a working tree, apply
//! selected updates to the files on disk. Rendering is a pure function of [`Model`], so
//! tests draw frames on a test backend and drive it with key events.
use crate::{
    palette,
    plan::{Local, plan},
    updates::{Pick, Update, updates},
};
use patchula_report::{LookupFailure, Report};
use patchula_words::{Manager, Update as UpdateKind};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, Paragraph, Row as TableRow, Table, TableState, Tabs, Wrap,
    },
};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub repository: String,
    pub name: String,
    pub version: String,
    pub kind: UpdateKind,
    pub published_at: String,
    pub eligible_at: String,
}

/// A template update the Count reports but never edits; in a working tree,
/// copier can apply it on request.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub repository: String,
    pub notice: patchula_report::Notice,
    pub applied: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub repository: String,
    pub file: String,
    pub manager: Manager,
    pub name: String,
    pub current: String,
    pub resolved: Option<String>,
    pub updates: usize,
}

/// One entry on the Warnings tab.
pub enum Warning {
    Failure(LookupFailure),
    Note(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Updates,
    Held,
    Dependencies,
    Templates,
    Warnings,
    Stats,
}
const TABS: [(Tab, &str); 6] = [
    (Tab::Updates, "Updates"),
    (Tab::Held, "Held back"),
    (Tab::Dependencies, "Dependencies"),
    (Tab::Templates, "Templates"),
    (Tab::Warnings, "Warnings"),
    (Tab::Stats, "Stats"),
];

/// Which update classes the Updates tab shows; `t` cycles through them.
/// `None` is every class; `Some` cycles through [`UpdateKind::RANKED`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter(pub Option<UpdateKind>);
impl Filter {
    /// Every update class.
    pub const ALL: Self = Self(None);
    fn next(self) -> Self {
        Self(match self.0 {
            None => UpdateKind::RANKED.first().copied(),
            Some(kind) => UpdateKind::RANKED
                .get(usize::from(kind.rank()) + 1)
                .copied(),
        })
    }
    fn label(self) -> &'static str {
        self.0.map_or("all", UpdateKind::as_str)
    }
    fn matches(self, kind: UpdateKind) -> bool {
        self.0.is_none_or(|filter| filter == kind)
    }
}

/// Where the lookup came from, which decides whether the TUI can write.
pub enum Origin {
    /// The working tree in the current directory.
    Local(Box<Local>),
    /// This checkout, looked up on a Count Patchula server.
    Server,
    /// Repositories read through the GitHub API.
    GitHub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    NextTab,
    PreviousTab,
    JumpTab,
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    Search,
    Filter,
    Select,
    SelectAll,
    Apply,
    Copier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
}
const fn plain(code: KeyCode) -> Key {
    Key { code, ctrl: false }
}
impl Key {
    pub fn name(self) -> String {
        let name = match self.code {
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char(c) if self.ctrl => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Shift-Tab".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            other => format!("{other:?}"),
        };
        if self.ctrl {
            format!("Ctrl-{name}")
        } else {
            name
        }
    }
    fn matches(self, event: KeyEvent) -> bool {
        self.code == event.code && self.ctrl == event.modifiers.contains(KeyModifiers::CONTROL)
    }
}

/// One row of the key table. Dispatch, the key bar and the help all read
/// [`KEYS`], so they cannot disagree.
pub struct Binding {
    pub action: Action,
    pub keys: &'static [Key],
    /// The tab the keys work on; `None` for every tab.
    pub tab: Option<Tab>,
    pub help: &'static str,
    /// The key bar's label, shown after the first key; `None` keeps the
    /// binding to the help.
    pub bar: Option<&'static str>,
}
impl Binding {
    pub fn keys(&self) -> String {
        self.keys
            .iter()
            .map(|k| k.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
    fn on(&self, tab: Tab) -> bool {
        self.tab.is_none_or(|t| t == tab)
    }
}

pub const KEYS: &[Binding] = &[
    Binding {
        action: Action::Quit,
        keys: &[
            plain(KeyCode::Char('q')),
            plain(KeyCode::Esc),
            Key {
                code: KeyCode::Char('c'),
                ctrl: true,
            },
        ],
        tab: None,
        help: "quit",
        bar: Some("quit"),
    },
    Binding {
        action: Action::Help,
        keys: &[plain(KeyCode::Char('?'))],
        tab: None,
        help: "show these keys",
        bar: Some("help"),
    },
    Binding {
        action: Action::NextTab,
        keys: &[plain(KeyCode::Tab), plain(KeyCode::Right)],
        tab: None,
        help: "next tab",
        bar: None,
    },
    Binding {
        action: Action::PreviousTab,
        keys: &[plain(KeyCode::BackTab), plain(KeyCode::Left)],
        tab: None,
        help: "previous tab",
        bar: None,
    },
    Binding {
        action: Action::JumpTab,
        keys: &[
            plain(KeyCode::Char('1')),
            plain(KeyCode::Char('2')),
            plain(KeyCode::Char('3')),
            plain(KeyCode::Char('4')),
            plain(KeyCode::Char('5')),
            plain(KeyCode::Char('6')),
        ],
        tab: None,
        help: "jump to a tab",
        bar: None,
    },
    Binding {
        action: Action::Down,
        keys: &[plain(KeyCode::Char('j')), plain(KeyCode::Down)],
        tab: None,
        help: "move down",
        bar: None,
    },
    Binding {
        action: Action::Up,
        keys: &[plain(KeyCode::Char('k')), plain(KeyCode::Up)],
        tab: None,
        help: "move up",
        bar: None,
    },
    Binding {
        action: Action::PageDown,
        keys: &[plain(KeyCode::PageDown)],
        tab: None,
        help: "page down",
        bar: None,
    },
    Binding {
        action: Action::PageUp,
        keys: &[plain(KeyCode::PageUp)],
        tab: None,
        help: "page up",
        bar: None,
    },
    Binding {
        action: Action::Top,
        keys: &[plain(KeyCode::Char('g')), plain(KeyCode::Home)],
        tab: None,
        help: "top",
        bar: None,
    },
    Binding {
        action: Action::Bottom,
        keys: &[plain(KeyCode::Char('G')), plain(KeyCode::End)],
        tab: None,
        help: "bottom",
        bar: None,
    },
    Binding {
        action: Action::Search,
        keys: &[plain(KeyCode::Char('/'))],
        tab: None,
        help: "search; Enter or Esc to finish",
        bar: Some("search"),
    },
    Binding {
        action: Action::Filter,
        keys: &[plain(KeyCode::Char('t'))],
        tab: Some(Tab::Updates),
        help: "cycle the update type filter",
        bar: Some("type"),
    },
    Binding {
        action: Action::Select,
        keys: &[plain(KeyCode::Char(' '))],
        tab: Some(Tab::Updates),
        help: "select or clear the update under the cursor",
        bar: Some("select"),
    },
    Binding {
        action: Action::SelectAll,
        keys: &[plain(KeyCode::Char('a'))],
        tab: Some(Tab::Updates),
        help: "select every visible update, or clear them",
        bar: Some("all"),
    },
    Binding {
        action: Action::Apply,
        keys: &[plain(KeyCode::Char('w'))],
        tab: Some(Tab::Updates),
        help: "apply: write the selection to files, after y",
        bar: Some("apply"),
    },
    Binding {
        action: Action::Copier,
        keys: &[plain(KeyCode::Char('u'))],
        tab: Some(Tab::Templates),
        help: "run copier update for a template, after y",
        bar: Some("copier update"),
    },
];

/// The binding a key press triggers on a tab. Shift with a lowercase letter
/// counts as the uppercase letter, whichever way the terminal reports it.
pub fn binding(event: KeyEvent, tab: Tab) -> Option<&'static Binding> {
    let event = shifted(event);
    KEYS.iter()
        .find(|b| b.on(tab) && b.keys.iter().any(|k| k.matches(event)))
}
fn shifted(mut event: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(c) = event.code
        && event.modifiers.contains(KeyModifiers::SHIFT)
    {
        event.code = KeyCode::Char(c.to_ascii_uppercase());
    }
    event
}

/// What the keyboard drives: the tabs, or one step over them. One value, so
/// two prompts can never be open at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Searching,
    Help,
    /// Waiting for y before writing the selection.
    ConfirmApply,
    /// Waiting for y before running copier for this template.
    ConfirmCopier(usize),
    /// Confirmed: the terminal loop hands this template to copier.
    Copier(usize),
}

pub struct Model {
    pub tab: Tab,
    pub updates: Vec<Update>,
    pub held: Vec<Held>,
    pub templates: Vec<Template>,
    pub found: Vec<Found>,
    /// Each repository's warnings in report order, so a repository's
    /// entries are next to each other.
    pub warnings: Vec<(String, Warning)>,
    pub stats: Vec<(String, String)>,
    pub repositories: usize,
    pub filter: Filter,
    pub search: String,
    pub mode: Mode,
    pub cursor: usize,
    pub scroll: usize,
    /// Each tab's cursor and scroll while another tab is shown.
    places: [(usize, usize); TABS.len()],
    pub message: Option<String>,
    pub origin: Origin,
    /// The working tree has uncommitted changes, so copier would refuse.
    pub uncommitted: bool,
    /// Scroll positions on the Warnings tab, as of the last draw.
    pub warning_rows: usize,
    pub quit: bool,
}

impl Model {
    pub fn new(report: &Report, origin: Origin) -> Self {
        let updates = updates(report);
        let mut held = Vec::new();
        let mut templates = Vec::new();
        let mut found = Vec::new();
        let mut warnings = Vec::new();
        let mut managers = BTreeMap::<String, usize>::new();
        for repo in &report.repositories {
            for warning in &repo.warnings {
                warnings.push((repo.repository.clone(), Warning::Note(warning.clone())));
            }
            for notice in &repo.notices {
                if notice.is_template() {
                    templates.push(Template {
                        repository: repo.repository.clone(),
                        notice: notice.clone(),
                        applied: false,
                    });
                } else {
                    warnings.push((
                        repo.repository.clone(),
                        Warning::Note(format!("{} ({})", notice.text(), notice.path)),
                    ));
                }
            }
            for failure in &repo.lookup_failures {
                warnings.push((repo.repository.clone(), Warning::Failure(failure.clone())));
            }
            if let Some(reason) = &repo.reason {
                warnings.push((
                    repo.repository.clone(),
                    Warning::Note(format!("{:?}: {reason}", repo.status)),
                ));
            }
            for file in &repo.package_files {
                for dep in &file.dependencies {
                    *managers.entry(dep.manager.to_string()).or_default() += 1;
                    let current = crate::updates::current(dep);
                    found.push(Found {
                        repository: repo.repository.clone(),
                        file: file.path.clone(),
                        manager: dep.manager,
                        name: dep.name.clone(),
                        current,
                        resolved: dep.current.version.clone(),
                        updates: dep.updates.len(),
                    });
                    for hold in dep
                        .metadata
                        .as_ref()
                        .map(|m| m.held_back.as_slice())
                        .unwrap_or_default()
                    {
                        held.push(Held {
                            repository: repo.repository.clone(),
                            name: dep.name.clone(),
                            version: hold.version.clone(),
                            kind: hold.update_type,
                            published_at: hold.published_at.clone(),
                            eligible_at: hold.eligible_at.clone(),
                        });
                    }
                }
            }
        }
        let mut stats = vec![
            ("Repositories".into(), report.repositories.len().to_string()),
            ("Dependencies".into(), found.len().to_string()),
            ("Updates".into(), updates.len().to_string()),
            ("Held back".into(), held.len().to_string()),
            ("Template updates".into(), templates.len().to_string()),
        ];
        if let Some(s) = &report.stats {
            stats.push(("Requests".into(), s.requests.to_string()));
            stats.push(("Not modified (304)".into(), s.not_modified.to_string()));
            stats.push(("Cache hits".into(), s.cache_hits.to_string()));
            stats.push(("Unique lookups".into(), s.lookups.to_string()));
            for (phase, micros) in &s.phase_micros {
                stats.push((
                    format!("{phase} time"),
                    format!("{:.0} ms", *micros as f64 / 1000.0),
                ));
            }
        }
        for (manager, count) in managers {
            stats.push((format!("{manager} dependencies"), count.to_string()));
        }
        Model {
            tab: Tab::Updates,
            updates,
            held,
            templates,
            found,
            warnings,
            stats,
            repositories: report.repositories.len(),
            filter: Filter::ALL,
            search: String::new(),
            cursor: 0,
            scroll: 0,
            places: [(0, 0); TABS.len()],
            mode: Mode::Browse,
            message: None,
            uncommitted: match &origin {
                Origin::Local(local) => uncommitted(&local.dir),
                _ => false,
            },
            origin,
            warning_rows: 1,
            quit: false,
        }
    }

    /// Why the TUI cannot write to these files at all.
    fn read_only(&self) -> Option<&'static str> {
        match self.origin {
            Origin::Local(_) => None,
            Origin::Server => Some(
                "this lookup ran on the server; pass --here to look up this working tree on this machine",
            ),
            Origin::GitHub => Some(
                "these repositories come from GitHub; only a working tree, with no repository named, can be written",
            ),
        }
    }
    /// Why `w` cannot apply right now.
    pub fn apply_blocked(&self) -> Option<String> {
        if let Some(reason) = self.read_only() {
            return Some(reason.into());
        }
        (self.selected() == 0).then(|| "select updates with space or a first".into())
    }
    /// Why `u` cannot run copier for the template under the cursor.
    pub fn copier_blocked(&self) -> Option<String> {
        if let Some(reason) = self.read_only() {
            return Some(reason.into());
        }
        let template = self.templates.get(self.cursor)?;
        if template.applied {
            Some("copier already updated this template; review and commit its changes".into())
        } else if self.uncommitted {
            Some("the working tree has uncommitted changes, and copier needs a clean tree".into())
        } else {
            None
        }
    }
    /// The question waiting for `y`, if any.
    pub fn prompt(&self) -> Option<String> {
        let index = match self.mode {
            Mode::ConfirmApply => {
                let n = self.selected();
                return Some(format!(
                    "Write {n} selected update{} into the working tree? y writes, any other key cancels.",
                    plural(n)
                ));
            }
            Mode::ConfirmCopier(index) => index,
            _ => return None,
        };
        let template = self.templates.get(index)?;
        Some(format!(
            "Run copier update --vcs-ref {} for {}? The template's code runs on this machine as you{}. y runs it, any other key cancels.",
            template.notice.available,
            template.notice.name,
            template
                .notice
                .compare_url
                .as_ref()
                .map(|url| format!("; its changes: {url}"))
                .unwrap_or_default()
        ))
    }
    /// The key bar for the current tab, built from [`KEYS`].
    pub fn bar(&self) -> String {
        KEYS.iter()
            .filter(|b| b.on(self.tab))
            .filter_map(|b| {
                let label = format!("{} {}", b.keys[0].name(), b.bar?);
                let blocked = match b.action {
                    Action::Apply => self.apply_blocked(),
                    Action::Copier => self.copier_blocked(),
                    _ => None,
                };
                Some(match (b.action, blocked) {
                    (_, Some(reason)) => format!("{label} (off: {reason})"),
                    (Action::Filter, _) => format!("{label}: {}", self.filter.label()),
                    (Action::Search, _) if !self.search.is_empty() => {
                        format!("{label} \"{}\"", self.search)
                    }
                    (Action::Apply, _) => format!("{label} {} selected", self.selected()),
                    _ => label,
                })
            })
            .chain((self.tab == Tab::Warnings).then(|| "j/k scroll  PgUp/PgDn page".to_owned()))
            .collect::<Vec<_>>()
            .join("  ")
    }

    fn query_matches(&self, text: &[&str]) -> bool {
        let query = self.search.to_lowercase();
        query.is_empty() || text.iter().any(|t| t.to_lowercase().contains(&query))
    }
    /// Indexes into `updates` that the filter and search let through.
    pub fn visible(&self) -> Vec<usize> {
        self.updates
            .iter()
            .enumerate()
            .filter(|(_, u)| {
                self.filter.matches(u.kind)
                    && self.query_matches(&[&u.name, &u.package, &u.file, u.manager.as_str()])
            })
            .map(|(i, _)| i)
            .collect()
    }
    fn rows(&self) -> usize {
        match self.tab {
            Tab::Updates => self.visible().len(),
            Tab::Held => self.held.len(),
            Tab::Templates => self.templates.len(),
            Tab::Dependencies => self
                .found
                .iter()
                .filter(|f| self.query_matches(&[&f.name, &f.file, f.manager.as_str()]))
                .count(),
            Tab::Warnings => self.warning_rows,
            Tab::Stats => self.stats.len(),
        }
    }
    fn selected(&self) -> usize {
        self.updates
            .iter()
            .filter(|u| u.pick == Pick::Selected)
            .count()
    }

    pub fn key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.mode == Mode::Searching {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.mode = Mode::Browse,
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c) => self.search.push(c),
                _ => {}
            }
            self.cursor = 0;
            return;
        }
        let yes = matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
        if let Mode::ConfirmCopier(index) = self.mode {
            self.mode = Mode::Browse;
            self.message = Some(if yes {
                self.mode = Mode::Copier(index);
                "Copier is running in your terminal.".into()
            } else {
                "Copier was not run.".into()
            });
            return;
        }
        if self.mode == Mode::ConfirmApply {
            self.mode = Mode::Browse;
            self.message = Some(if yes {
                match self.apply() {
                    Ok((edits, files, skipped)) => {
                        let mut message = format!(
                            "The Count wrote {edits} update{} into {files} file{}.",
                            plural(edits),
                            plural(files)
                        );
                        if !skipped.is_empty() {
                            message.push_str(&format!(" Left alone: {}.", skipped.join("; ")));
                        }
                        message.push_str(" Refresh lockfiles with your package manager.");
                        message
                    }
                    Err(error) => format!("The Count stopped: {error}"),
                }
            } else {
                "Apply cancelled; nothing was written.".into()
            });
            return;
        }
        if let Mode::Copier(_) = self.mode {
            // Confirmed: the terminal loop runs it next, whatever is pressed.
            return;
        }
        self.message = None;
        if self.mode == Mode::Help {
            self.mode = Mode::Browse;
            return;
        }
        let Some(binding) = binding(key, self.tab) else {
            return;
        };
        let rows = self.rows();
        let tab = self.tab;
        match binding.action {
            Action::Quit => self.quit = true,
            Action::Help => self.mode = Mode::Help,
            Action::NextTab => self.switch(1),
            Action::PreviousTab => self.switch(TABS.len() - 1),
            Action::JumpTab => {
                if let KeyCode::Char(c) = key.code
                    && let Some(index) = c
                        .to_digit(10)
                        .map(|digit| digit as usize)
                        .filter(|digit| (1..=TABS.len()).contains(digit))
                {
                    self.show(index - 1);
                }
            }
            Action::Down => self.cursor = (self.cursor + 1).min(rows.saturating_sub(1)),
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::PageDown => self.cursor = (self.cursor + 10).min(rows.saturating_sub(1)),
            Action::PageUp => self.cursor = self.cursor.saturating_sub(10),
            Action::Top => self.cursor = 0,
            Action::Bottom => self.cursor = rows.saturating_sub(1),
            Action::Search => {
                self.mode = Mode::Searching;
                self.search.clear();
            }
            Action::Filter => {
                self.filter = self.filter.next();
                self.cursor = 0;
            }
            Action::Select => {
                if let Some(&index) = self.visible().get(self.cursor) {
                    let update = &mut self.updates[index];
                    update.pick = match update.pick {
                        Pick::Open => Pick::Selected,
                        Pick::Selected => Pick::Open,
                        Pick::Applied => Pick::Applied,
                    };
                }
            }
            Action::SelectAll => {
                let visible = self.visible();
                let select = visible.iter().any(|&i| self.updates[i].pick == Pick::Open);
                for i in visible {
                    let pick = &mut self.updates[i].pick;
                    if *pick != Pick::Applied {
                        *pick = if select { Pick::Selected } else { Pick::Open };
                    }
                }
            }
            Action::Apply => match self.apply_blocked() {
                Some(reason) => self.message = Some(format!("The Count cannot apply: {reason}.")),
                None => self.mode = Mode::ConfirmApply,
            },
            Action::Copier => {
                if self.templates.get(self.cursor).is_none() {
                    return;
                }
                self.refresh();
                match self.copier_blocked() {
                    Some(reason) => {
                        self.message = Some(format!("The Count cannot run copier: {reason}."))
                    }
                    None => self.mode = Mode::ConfirmCopier(self.cursor),
                }
            }
        }
        if self.tab == Tab::Templates && tab != Tab::Templates {
            self.refresh();
        }
    }
    /// Read the working tree's Git state again; it may have changed outside
    /// the TUI since the last look.
    fn refresh(&mut self) {
        if let Origin::Local(local) = &self.origin {
            self.uncommitted = uncommitted(&local.dir);
        }
    }
    fn switch(&mut self, step: usize) {
        self.show((self.index() + step) % TABS.len());
    }
    fn index(&self) -> usize {
        TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0)
    }
    /// Show a tab where it was left: its cursor and scroll are kept while
    /// another tab is shown.
    fn show(&mut self, index: usize) {
        self.places[self.index()] = (self.cursor, self.scroll);
        self.tab = TABS[index].0;
        (self.cursor, self.scroll) = self.places[index];
    }

    /// Write the selected updates into the local directory's files. As in
    /// `patchula apply`, the applicable subset is written and each skip is
    /// reported; an error says what was written.
    fn apply(&mut self) -> Result<(usize, usize, Vec<String>), String> {
        let Origin::Local(local) = &self.origin else {
            return Err("not a local directory; nothing was written".into());
        };
        let selected: Vec<usize> = (0..self.updates.len())
            .filter(|&i| self.updates[i].pick == Pick::Selected)
            .collect();
        let chosen: Vec<&Update> = selected.iter().map(|&i| &self.updates[i]).collect();
        let plan = plan(local, &chosen).map_err(|error| format!("{error}; nothing was written"))?;
        if let Err(error) = plan.write(&local.dir) {
            // Whatever landed is in the tree now, so the tree's view follows it.
            self.uncommitted = uncommitted(&local.dir);
            return Err(error.to_string());
        }
        self.uncommitted = uncommitted(&local.dir);
        let (applied, skipped) = crate::plan::outcomes(&plan, &chosen);
        let edits = applied.len();
        let files = plan.files.len();
        let skipped: Vec<String> = skipped
            .iter()
            .map(|(update, reason)| format!("{}: {reason}", update.name))
            .collect();
        for &position in &plan.applied {
            self.updates[selected[position]].pick = Pick::Applied;
        }
        Ok((edits, files, skipped))
    }
    /// Run copier for the confirmed template, with the terminal handed over
    /// to it, and say what it changed.
    pub fn run_copier(&mut self, program: &str) {
        let Mode::Copier(index) = self.mode else {
            return;
        };
        self.mode = Mode::Browse;
        let Origin::Local(local) = &self.origin else {
            return;
        };
        let notice = self.templates[index].notice.clone();
        self.message = Some(
            match crate::copier::update(program, &local.dir, &notice.path, &notice.available, false)
            {
                Ok(outcome) => {
                    self.templates[index].applied = true;
                    crate::copier::summary(&notice.name, &notice.available, &outcome)
                }
                Err(error) => format!("Copier did not finish: {error}"),
            },
        );
        self.uncommitted = uncommitted(&local.dir);
    }
}

/// Whether the checkout holding `dir` has changes Git would commit; false
/// when Git cannot tell, and copier then says why itself.
fn uncommitted(dir: &Path) -> bool {
    crate::copier::changed(dir).is_ok_and(|paths| !paths.is_empty())
}
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
fn kind_style(kind: UpdateKind) -> Style {
    Style::new().fg(palette::kind_colour(kind).tui())
}
fn age(published: Option<&str>) -> String {
    let Some(published) = published else {
        return String::new();
    };
    match chrono::DateTime::parse_from_rfc3339(published) {
        Ok(time) => {
            let hours = (chrono::Utc::now() - time.with_timezone(&chrono::Utc)).num_hours();
            if hours < 48 {
                format!("{hours}h")
            } else {
                format!("{}d", hours / 24)
            }
        }
        Err(_) => String::new(),
    }
}

pub fn draw(frame: &mut Frame, model: &mut Model) {
    let area = frame.area();
    let width = usize::from(area.width).saturating_sub(1);
    let mut footer_lines: Vec<Line> = Vec::new();
    if let Some(message) = &model.message {
        footer_lines.extend(
            wrap(message, width)
                .into_iter()
                .map(|l| Line::styled(format!(" {l}"), Style::new().fg(palette::WARNING.tui()))),
        );
    }
    let (status, style) = if model.mode == Mode::Searching {
        (
            format!("search: {}_  Enter or Esc to finish", model.search),
            Style::new(),
        )
    } else if let Some(prompt) = model.prompt() {
        (prompt, Style::new().fg(palette::ACCENT.tui()).bold())
    } else {
        (model.bar(), Style::new().fg(palette::MUTED.tui()))
    };
    footer_lines.extend(
        wrap(&status, width)
            .into_iter()
            .map(|l| Line::styled(format!(" {l}"), style)),
    );
    // The footer wraps rather than cuts, giving way only to keep three rows
    // of the body.
    let footer_height = u16::try_from(footer_lines.len())
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(5))
        .max(1);
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(footer_height),
    ])
    .areas(area);
    let selected = model.selected();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Count Patchula ",
                Style::new()
                    .fg(Color::Black)
                    .bg(palette::ACCENT.tui())
                    .bold(),
            ),
            Span::raw(format!(
                "  {} repositor{} · {} dependencies · {} updates",
                model.repositories,
                if model.repositories == 1 { "y" } else { "ies" },
                model.found.len(),
                model.updates.len()
            )),
            Span::styled(
                if selected > 0 {
                    format!(" · {selected} selected")
                } else {
                    String::new()
                },
                Style::new().fg(palette::WARNING.tui()),
            ),
            Span::styled(
                match model.origin {
                    Origin::Local(_) => "  local",
                    Origin::Server => "  server",
                    Origin::GitHub => "",
                },
                Style::new().fg(palette::ACCENT_DIM.tui()),
            ),
        ])),
        header,
    );
    let index = TABS.iter().position(|(t, _)| *t == model.tab).unwrap_or(0);
    frame.render_widget(
        Tabs::new(
            TABS.iter()
                .enumerate()
                .map(|(i, (_, name))| format!("{} {name}", i + 1)),
        )
        .select(index)
        .highlight_style(Style::new().fg(palette::ACCENT.tui()).bold())
        .style(Style::new().fg(palette::MUTED.tui())),
        tabs,
    );
    match model.tab {
        Tab::Updates => draw_updates(frame, model, body),
        Tab::Held => draw_held(frame, model, body),
        Tab::Templates => draw_templates(frame, model, body),
        Tab::Dependencies => draw_found(frame, model, body),
        Tab::Warnings => draw_warnings(frame, model, body),
        Tab::Stats => draw_list(
            frame,
            model,
            body,
            "Stats",
            model
                .stats
                .iter()
                .map(|(k, v)| format!("{k:<28} {v}"))
                .collect(),
        ),
    }
    frame.render_widget(Paragraph::new(footer_lines), footer);
    if model.mode == Mode::Help {
        draw_help(frame);
    }
}

fn table_state(model: &mut Model, rows: usize, height: u16) -> TableState {
    let height = usize::from(height.saturating_sub(3)).max(1);
    model.cursor = model.cursor.min(rows.saturating_sub(1));
    if model.cursor < model.scroll {
        model.scroll = model.cursor;
    } else if model.cursor >= model.scroll + height {
        model.scroll = model.cursor + 1 - height;
    }
    TableState::default()
        .with_offset(model.scroll)
        .with_selected((rows > 0).then_some(model.cursor))
}

fn draw_updates(frame: &mut Frame, model: &mut Model, area: Rect) {
    let [list, detail] = Layout::vertical([Constraint::Min(3), Constraint::Length(6)]).areas(area);
    let visible = model.visible();
    let many = model.repositories > 1;
    let cells: Vec<[String; 6]> = visible
        .iter()
        .map(|&i| {
            let u = &model.updates[i];
            [
                if u.fixes.is_empty() {
                    u.name.clone()
                } else {
                    format!("{} (security)", u.name)
                },
                u.current.clone(),
                u.target(),
                u.kind.to_string(),
                age(u.published_at.as_deref()),
                if many {
                    format!("{}:{}", u.repository, u.file)
                } else {
                    u.file.clone()
                },
            ]
        })
        .collect();
    let header = ["", "Dependency", "Current", "Target", "Type", "Age", "File"];
    let natural = |c: usize| {
        cells
            .iter()
            .map(|row| Span::raw(row[c - 1].as_str()).width())
            .chain([header[c].len()])
            .max()
            .unwrap_or(0)
    };
    // Versions keep their full width up to a cap; the dependency name and
    // the file give way first, the name down to 24 columns before the file
    // goes below 12.
    let widths = fit(
        usize::from(list.width),
        &[
            (4, 4),
            (natural(1), 12),
            (natural(2).min(32), 8),
            (natural(3).min(48), 12),
            (natural(4), 5),
            (natural(5), 3),
            (natural(6), 4),
        ],
        &[(1, 24), (6, 12), (1, 12), (6, 4), (2, 8), (3, 12)],
    );
    let rows: Vec<TableRow> = visible
        .iter()
        .zip(cells)
        .map(|(&i, [name, current, target, kind, age, file])| {
            let u = &model.updates[i];
            let mark = if u.pick == Pick::Applied {
                "done"
            } else if u.pick == Pick::Selected {
                "[x]"
            } else {
                "[ ]"
            };
            TableRow::new(vec![
                Cell::from(mark).style(Style::new().fg(if u.pick == Pick::Applied {
                    palette::SUCCESS.tui()
                } else {
                    palette::WARNING.tui()
                })),
                Cell::from(middle(&name, widths[1])).style(if u.fixes.is_empty() {
                    Style::new()
                } else {
                    Style::new().fg(palette::ERROR.tui())
                }),
                Cell::from(current).style(Style::new().fg(palette::MUTED.tui())),
                Cell::from(target),
                Cell::from(kind).style(kind_style(u.kind)),
                Cell::from(age).style(Style::new().fg(palette::MUTED.tui())),
                Cell::from(middle(&file, widths[6])).style(Style::new().fg(palette::MUTED.tui())),
            ])
        })
        .collect();
    let mut state = table_state(model, visible.len(), list.height);
    let title = format!(
        " Updates ({} of {}, type: {}) ",
        visible.len(),
        model.updates.len(),
        model.filter.label()
    );
    frame.render_stateful_widget(
        Table::new(rows, widths.iter().map(|&w| Constraint::Length(w as u16)))
            .column_spacing(GAP as u16)
            .header(
                TableRow::new(header).style(
                    Style::new()
                        .fg(palette::ACCENT_DIM.tui())
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .row_highlight_style(Style::new().bg(palette::SELECTION.tui()))
            .block(
                Block::new()
                    .borders(Borders::TOP)
                    .title(title)
                    .title_style(Style::new().fg(palette::ACCENT.tui())),
            ),
        list,
        &mut state,
    );
    let text = match visible.get(model.cursor).map(|&i| &model.updates[i]) {
        Some(u) => vec![
            Line::from(vec![
                Span::styled(u.package.clone(), Style::new().bold()),
                Span::styled(
                    format!("  {} · {}", u.manager, u.file),
                    Style::new().fg(palette::MUTED.tui()),
                ),
            ]),
            Line::from(format!("{} → {}  ({})", u.current, u.target(), u.kind)),
            Line::from(format!(
                "version {}{}",
                u.version.as_deref().unwrap_or("unchanged"),
                u.digest
                    .as_deref()
                    .map(|d| format!(" · digest {d}"))
                    .unwrap_or_default()
            )),
            if u.fixes.is_empty() {
                Line::from(match &u.published_at {
                    Some(p) => format!("published {p}"),
                    None => "no publication date".into(),
                })
                .style(Style::new().fg(palette::MUTED.tui()))
            } else {
                Line::from(format!("fixes {}", u.fixes.join(", ")))
                    .style(Style::new().fg(palette::ERROR.tui()))
            },
        ],
        None => vec![Line::from(if model.updates.is_empty() {
            "The Count found nothing to update. Rest easy."
        } else {
            "No updates match this filter."
        })],
    };
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: true }).block(
            Block::new()
                .borders(Borders::TOP)
                .border_style(Style::new().fg(palette::MUTED.tui())),
        ),
        detail,
    );
}

/// Column widths from content. Each column is `(natural, minimum)`: it starts
/// at its natural width, and while the row is wider than `width` the
/// `(column, floor)` steps in `order` take it back towards the floor, never
/// below its minimum. Room left over goes to the last column. Columns are
/// [GAP] apart.
const GAP: usize = 2;

fn fit(width: usize, columns: &[(usize, usize)], order: &[(usize, usize)]) -> Vec<usize> {
    let mut widths: Vec<usize> = columns.iter().map(|&(natural, _)| natural).collect();
    let total =
        |widths: &[usize]| widths.iter().sum::<usize>() + widths.len().saturating_sub(1) * GAP;
    for &(column, floor) in order {
        let excess = total(&widths).saturating_sub(width);
        let floor = floor.max(columns[column].1);
        widths[column] -= excess.min(widths[column].saturating_sub(floor));
    }
    let used = total(&widths);
    if let Some(last) = widths.last_mut() {
        *last += width.saturating_sub(used);
    }
    widths
}

/// `text` cut to `width` columns in the middle, so both its scope or host
/// and its last segment stay readable.
fn middle(text: &str, width: usize) -> String {
    let columns = |s: &str| Span::raw(s).width();
    if columns(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let tail_width = (width - 1) / 2;
    let head_width = width - 1 - tail_width;
    let take = |chars: &mut dyn Iterator<Item = char>, room: usize| {
        let mut taken = Vec::new();
        let mut used = 0;
        for c in chars {
            let w = columns(c.encode_utf8(&mut [0; 4]));
            if used + w > room {
                break;
            }
            used += w;
            taken.push(c);
        }
        taken
    };
    let head: String = take(&mut text.chars(), head_width).into_iter().collect();
    let tail: String = take(&mut text.chars().rev(), tail_width)
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn draw_held(frame: &mut Frame, model: &mut Model, area: Rect) {
    let rows: Vec<TableRow> = model
        .held
        .iter()
        .map(|h| {
            TableRow::new(vec![
                Cell::from(h.name.clone()),
                Cell::from(h.version.clone()),
                Cell::from(h.kind.to_string()).style(kind_style(h.kind)),
                Cell::from(h.published_at.clone()).style(Style::new().fg(palette::MUTED.tui())),
                Cell::from(h.eligible_at.clone()).style(Style::new().fg(palette::WARNING.tui())),
            ])
        })
        .collect();
    let mut state = table_state(model, model.held.len(), area.height);
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(30),
                Constraint::Percentage(18),
                Constraint::Length(7),
                Constraint::Fill(1),
                Constraint::Fill(1),
            ],
        )
        .header(
            TableRow::new(["Dependency", "Version", "Type", "Published", "Eligible"])
                .style(Style::new().fg(palette::ACCENT_DIM.tui()).bold()),
        )
        .row_highlight_style(Style::new().bg(palette::SELECTION.tui()))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(
                    " Held back by the release-age wait ({}) ",
                    model.held.len()
                ))
                .title_style(Style::new().fg(palette::ACCENT.tui())),
        ),
        area,
        &mut state,
    );
}

fn draw_templates(frame: &mut Frame, model: &mut Model, area: Rect) {
    let [area, detail] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(area);
    let rows: Vec<TableRow> = if model.templates.is_empty() {
        vec![TableRow::new(["No template updates. The night is quiet."])]
    } else {
        model
            .templates
            .iter()
            .map(|t| {
                TableRow::new(vec![
                    Cell::from(if t.applied { "done" } else { "" })
                        .style(Style::new().fg(palette::SUCCESS.tui())),
                    Cell::from(t.notice.name.clone()),
                    Cell::from(t.notice.current.clone())
                        .style(Style::new().fg(palette::MUTED.tui())),
                    Cell::from(t.notice.available.clone())
                        .style(Style::new().fg(palette::WARNING.tui())),
                    Cell::from(t.notice.path.clone()).style(Style::new().fg(palette::MUTED.tui())),
                ])
            })
            .collect()
    };
    let mut state = table_state(model, model.templates.len(), area.height);
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Percentage(36),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Fill(1),
            ],
        )
        .header(
            TableRow::new(["", "Template", "Current", "Available", "Answers file"])
                .style(Style::new().fg(palette::ACCENT_DIM.tui()).bold()),
        )
        .row_highlight_style(Style::new().bg(palette::SELECTION.tui()))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(
                    " Template updates: reported, never edited ({}) ",
                    model.templates.len()
                ))
                .title_style(Style::new().fg(palette::ACCENT.tui())),
        ),
        area,
        &mut state,
    );
    let changes = match model.templates.get(model.cursor) {
        Some(t) => match &t.notice.compare_url {
            Some(url) => format!("changes: {url}"),
            None => "no compare page for this host".into(),
        },
        None => String::new(),
    };
    frame.render_widget(
        Paragraph::new(changes)
            .style(Style::new().fg(palette::MUTED.tui()))
            .block(
                Block::new()
                    .borders(Borders::TOP)
                    .border_style(Style::new().fg(palette::MUTED.tui())),
            ),
        detail,
    );
}

fn draw_found(frame: &mut Frame, model: &mut Model, area: Rect) {
    let found: Vec<&Found> = model
        .found
        .iter()
        .filter(|f| model.query_matches(&[&f.name, &f.file, f.manager.as_str()]))
        .collect();
    let header = [
        "Manager",
        "Dependency",
        "Current",
        "Resolved",
        "Updates",
        "File",
    ];
    let natural = |c: usize, cell: &dyn Fn(&Found) -> usize| {
        found
            .iter()
            .map(|f| cell(f))
            .chain([header[c].len()])
            .max()
            .unwrap_or(0)
    };
    let width = |s: &str| Span::raw(s).width();
    let widths = fit(
        usize::from(area.width),
        &[
            (natural(0, &|f| width(f.manager.as_str())), 6),
            (natural(1, &|f| width(&f.name)), 12),
            (natural(2, &|f| width(&f.current)).min(40), 8),
            (
                natural(3, &|f| width(f.resolved.as_deref().unwrap_or(""))).min(24),
                8,
            ),
            (7, 7),
            (natural(5, &|f| width(&f.file)), 4),
        ],
        &[(1, 24), (5, 12), (1, 12), (5, 4), (3, 8), (2, 8)],
    );
    let rows: Vec<TableRow> = found
        .iter()
        .map(|f| {
            TableRow::new(vec![
                Cell::from(f.manager.to_string()).style(Style::new().fg(palette::ACCENT_DIM.tui())),
                Cell::from(middle(&f.name, widths[1])),
                Cell::from(f.current.clone()),
                Cell::from(f.resolved.clone().unwrap_or_default())
                    .style(Style::new().fg(palette::MUTED.tui())),
                Cell::from(if f.updates > 0 {
                    f.updates.to_string()
                } else {
                    String::new()
                })
                .style(Style::new().fg(palette::WARNING.tui())),
                Cell::from(middle(&f.file, widths[5])).style(Style::new().fg(palette::MUTED.tui())),
            ])
        })
        .collect();
    let count = found.len();
    let mut state = table_state(model, count, area.height);
    frame.render_stateful_widget(
        Table::new(rows, widths.iter().map(|&w| Constraint::Length(w as u16)))
            .column_spacing(GAP as u16)
            .header(TableRow::new(header).style(Style::new().fg(palette::ACCENT_DIM.tui()).bold()))
            .row_highlight_style(Style::new().bg(palette::SELECTION.tui()))
            .block(
                Block::new()
                    .borders(Borders::TOP)
                    .title(format!(" Dependencies ({count}) "))
                    .title_style(Style::new().fg(palette::ACCENT.tui())),
            ),
        area,
        &mut state,
    );
}

fn draw_list(frame: &mut Frame, model: &mut Model, area: Rect, title: &str, lines: Vec<String>) {
    let count = lines.len();
    let rows: Vec<TableRow> = if lines.is_empty() {
        vec![TableRow::new(["Nothing here. The night is quiet."])]
    } else {
        lines.into_iter().map(|l| TableRow::new([l])).collect()
    };
    let mut state = table_state(model, count, area.height);
    frame.render_stateful_widget(
        Table::new(rows, [Constraint::Fill(1)])
            .row_highlight_style(Style::new().bg(palette::SELECTION.tui()))
            .block(
                Block::new()
                    .borders(Borders::TOP)
                    .title(format!(" {title} ({count}) "))
                    .title_style(Style::new().fg(palette::ACCENT.tui())),
            ),
        area,
        &mut state,
    );
}

/// The Warnings tab is a pager laid out like the report's sections: grouped
/// by repository when there are several, failed lookups as aligned rows with
/// their message under each, then the other warnings. Everything wraps to the
/// pane's width, and the cursor is the first line shown, so nothing is cut.
fn draw_warnings(frame: &mut Frame, model: &mut Model, area: Rect) {
    let block = Block::new()
        .borders(Borders::TOP)
        .title_style(Style::new().fg(palette::ACCENT.tui()));
    let inner = block.inner(area);
    let width = usize::from(inner.width).saturating_sub(1).max(1);
    let mut lines = warning_lines(model, width);
    if lines.is_empty() {
        lines.push(Line::from("Nothing here. The night is quiet."));
    }
    let height = usize::from(inner.height).max(1);
    let last = lines.len().saturating_sub(height);
    model.warning_rows = last + 1;
    model.cursor = model.cursor.min(last);
    let count = model.warnings.len();
    let title = if lines.len() > height {
        format!(
            " Warnings ({count}) · lines {}–{} of {} ",
            model.cursor + 1,
            (model.cursor + height).min(lines.len()),
            lines.len()
        )
    } else {
        format!(" Warnings ({count}) ")
    };
    let shown: Vec<Line> = lines.into_iter().skip(model.cursor).take(height).collect();
    frame.render_widget(Paragraph::new(shown).block(block.title(title)), area);
}

fn warning_lines(model: &Model, width: usize) -> Vec<Line<'static>> {
    let indented = |indent: usize, text: &str, style: Style| -> Vec<Line<'static>> {
        wrap(text, width.saturating_sub(indent))
            .into_iter()
            .map(|line| Line::styled(format!("{:indent$}{line}", ""), style))
            .collect()
    };
    let heading = |title: &str, count: usize, colour: palette::Colour, indent: usize| {
        Line::from(vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(title.to_owned(), Style::new().fg(colour.tui()).bold()),
            Span::styled(format!(" ({count})"), Style::new().fg(palette::MUTED.tui())),
        ])
    };
    let many = model.repositories > 1;
    let mut lines = Vec::new();
    let mut entries = model.warnings.iter().peekable();
    while let Some((repository, _)) = entries.peek() {
        let repository = repository.clone();
        let mut failures = Vec::new();
        let mut notes = Vec::new();
        while let Some((_, warning)) = entries.next_if(|(r, _)| *r == repository) {
            match warning {
                Warning::Failure(failure) => failures.push(failure),
                Warning::Note(note) => notes.push(note),
            }
        }
        let indent = if many {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.extend(indented(0, &repository, Style::new().bold()));
            2
        } else {
            0
        };
        let start = lines.len();
        if !failures.is_empty() {
            lines.push(heading(
                "Lookup failures",
                failures.len(),
                palette::ERROR,
                indent,
            ));
            let columns = |s: &str| Span::raw(s).width();
            let padded = |s: &str, to: usize| format!("{s}{}  ", " ".repeat(to - columns(s)));
            let name = failures.iter().map(|f| columns(&f.name)).max().unwrap_or(0);
            let path = failures.iter().map(|f| columns(&f.path)).max().unwrap_or(0);
            for failure in failures {
                let mut source = failure.datasource.to_string();
                if failure.package != failure.name {
                    source = format!("{source} {}", failure.package);
                }
                let source = format!("{source}, {}", failure.kind.as_str());
                if indent + 2 + name + 2 + path + 2 + columns(&source) <= width {
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(indent + 2)),
                        Span::styled(padded(&failure.name, name), Style::new().bold()),
                        Span::styled(
                            padded(&failure.path, path),
                            Style::new().fg(palette::MUTED.tui()),
                        ),
                        Span::raw(source),
                    ]));
                } else {
                    lines.extend(indented(
                        indent + 2,
                        &format!("{}  {}  {source}", failure.name, failure.path),
                        Style::new().bold(),
                    ));
                }
                lines.extend(indented(indent + 4, &failure.message, Style::new()));
            }
        }
        if !notes.is_empty() {
            if lines.len() > start {
                lines.push(Line::from(""));
            }
            lines.push(heading("Warnings", notes.len(), palette::WARNING, indent));
            for note in notes {
                lines.extend(indented(indent + 2, note, Style::new()));
            }
        }
    }
    lines
}

/// Greedy word wrap to `width` columns. A word longer than a line is broken,
/// so every character of `text` lands on some line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let options = textwrap::Options::new(width.max(1))
        .word_separator(textwrap::WordSeparator::AsciiSpace)
        .word_splitter(textwrap::WordSplitter::NoHyphenation)
        .break_words(true);
    text.lines()
        .flat_map(|paragraph| textwrap::wrap(paragraph, &options))
        .map(std::borrow::Cow::into_owned)
        .collect()
}

fn draw_help(frame: &mut Frame) {
    let area = frame.area();
    let column = KEYS
        .iter()
        .map(|b| b.keys().chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let text: Vec<Line> = KEYS
        .iter()
        .map(|b| {
            Line::from(vec![
                Span::styled(
                    format!("{:<column$}", b.keys()),
                    Style::new().fg(palette::WARNING.tui()),
                ),
                Span::raw(match b.tab {
                    Some(tab) => format!("{} ({})", b.help, tab_name(tab)),
                    None => b.help.to_owned(),
                }),
            ])
        })
        .chain([
            Line::from(""),
            Line::from("Any key closes this.").style(Style::new().fg(palette::MUTED.tui())),
        ])
        .collect();
    let width = area.width.min(80);
    let height = area
        .height
        .min(u16::try_from(text.len() + 2).unwrap_or(u16::MAX));
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" The Count's keys ")
                .title_style(Style::new().fg(palette::ACCENT.tui()))
                .border_style(Style::new().fg(palette::ACCENT_DIM.tui())),
        ),
        popup,
    );
}
fn tab_name(tab: Tab) -> &'static str {
    TABS.iter()
        .find(|(t, _)| *t == tab)
        .map_or("", |(_, name)| name)
}

/// A plain-text snapshot of the updates, for pipes and CI logs.
pub fn snapshot(model: &Model) -> String {
    let mut out = format!(
        "Count Patchula: {} repositor{}, {} dependencies, {} updates, {} held back\n",
        model.repositories,
        if model.repositories == 1 { "y" } else { "ies" },
        model.found.len(),
        model.updates.len(),
        model.held.len()
    );
    for u in &model.updates {
        out.push_str(&format!(
            "{:<7} {} {} -> {}  ({}{})\n",
            u.kind,
            u.name,
            u.current,
            u.target(),
            if model.repositories > 1 {
                format!("{}:", u.repository)
            } else {
                String::new()
            },
            u.file
        ));
    }
    for t in &model.templates {
        out.push_str(&format!(
            "notice  {} ({}{})\n",
            t.notice.text(),
            if model.repositories > 1 {
                format!("{}:", t.repository)
            } else {
                String::new()
            },
            t.notice.path
        ));
    }
    for (repo, warning) in &model.warnings {
        match warning {
            Warning::Failure(failure) => {
                out.push_str(&format!("warning {repo}: lookup failed: {failure}\n"))
            }
            Warning::Note(note) => out.push_str(&format!("warning {repo}: {note}\n")),
        }
    }
    out
}

/// Run the interactive loop until the user quits.
pub fn interact(mut model: Model) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| {
        while !model.quit {
            terminal.draw(|frame| draw(frame, &mut model))?;
            if let Event::Key(key) = event::read()? {
                model.key(key);
            }
            if matches!(model.mode, Mode::Copier(_)) {
                // Copier asks its own questions, so it gets the plain terminal.
                ratatui::restore();
                model.run_copier("copier");
                println!(
                    "\n{}\nPress Enter to return to the Count.",
                    model.message.as_deref().unwrap_or("")
                );
                std::io::stdin().read_line(&mut String::new())?;
                terminal = ratatui::init();
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::local;
    use ratatui::{Terminal, backend::TestBackend, crossterm::event::KeyEvent};

    fn report(updates: &[(&str, &str, &str, &str)]) -> Report {
        let dependencies: Vec<_> = updates
            .iter()
            .map(|(name, current, target, kind)| {
                serde_json::json!({
                    "manager": "npm", "name": name, "package": name,
                    "current": {"value": current, "version": current},
                    "updates": [{"newValue": target, "newVersion": target, "updateType": kind}],
                    "metadata": {"source": "npm", "kinds": ["production"],
                        "locations": [{"start": 0, "end": 0, "line": 1, "column": 1, "pointer": "/"}]}
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 8,
            "repositories": [{"repository": "local/app", "status": "complete",
                "packageFiles": [{"path": "package.json", "dependencies": dependencies}]}]
        }))
        .unwrap()
    }
    fn press(model: &mut Model, code: KeyCode) {
        model.key(KeyEvent::from(code));
    }
    fn screen(model: &mut Model) -> String {
        sized(model, 110, 24)
    }
    fn sized(model: &mut Model, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, model)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn updates_render_filter_and_search() {
        let mut model = Model::new(
            &report(&[
                ("zod", "3.22.0", "4.1.0", "major"),
                ("hono", "4.6.0", "4.7.2", "minor"),
            ]),
            Origin::GitHub,
        );
        let text = screen(&mut model);
        assert!(text.contains("Count Patchula"), "{text}");
        assert!(text.contains("zod") && text.contains("4.1.0") && text.contains("hono"));
        press(&mut model, KeyCode::Char('t'));
        assert_eq!(model.filter, Filter(Some(UpdateKind::Major)));
        assert_eq!(model.visible().len(), 1);
        press(&mut model, KeyCode::Char('t'));
        press(&mut model, KeyCode::Char('/'));
        for c in "hon".chars() {
            press(&mut model, KeyCode::Char(c));
        }
        press(&mut model, KeyCode::Enter);
        assert_eq!(model.visible().len(), 1);
        let text = screen(&mut model);
        assert!(text.contains("hono") && !text.contains("zod "), "{text}");
        press(&mut model, KeyCode::Char('w'));
        assert!(model.message.as_deref().unwrap().contains("working tree"));
        assert_ne!(model.mode, Mode::ConfirmApply);
        press(&mut model, KeyCode::Char('3'));
        assert_eq!(model.tab, Tab::Dependencies);
        press(&mut model, KeyCode::Char('?'));
        assert!(screen(&mut model).contains("The Count's keys"));
        press(&mut model, KeyCode::Char('x'));
        press(&mut model, KeyCode::Char('q'));
        assert!(model.quit);
        assert!(snapshot(&model).contains("major   zod 3.22.0 -> 4.1.0  (package.json)"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn selected_updates_are_written_to_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let manifest =
            "{\n  \"dependencies\": {\n    \"zod\": \"3.22.0\",\n    \"hono\": \"4.6.0\"\n  }\n}\n";
        std::fs::write(dir.path().join("package.json"), manifest).unwrap();
        std::fs::write(
            dir.path().join("patchula.pkl"),
            "amends \"PatchulaConfig.pkl\"\n",
        )
        .unwrap();
        let config = patchula_config::load(dir.path()).unwrap();
        let mut report = report(&[
            ("zod", "3.22.0", "4.1.0", "major"),
            ("hono", "4.6.0", "4.7.2", "minor"),
        ]);
        // Point each update at its real declaration, as a lookup would.
        for dep in &mut report.repositories[0].package_files[0].dependencies {
            let start = manifest
                .find(&format!("\"{}\"", dep.current.value.as_ref().unwrap()))
                .unwrap();
            dep.metadata.as_mut().unwrap().locations[0].start = start;
        }
        let mut model = Model::new(
            &report,
            Origin::Local(Box::new(local(dir.path(), Some(config)).unwrap())),
        );
        press(&mut model, KeyCode::Char('w'));
        assert!(model.message.as_deref().unwrap().contains("select updates"));
        press(&mut model, KeyCode::Char(' '));
        press(&mut model, KeyCode::Char('w'));
        assert_eq!(model.mode, Mode::ConfirmApply);
        press(&mut model, KeyCode::Char('y'));
        assert!(
            model
                .message
                .as_deref()
                .unwrap()
                .contains("wrote 1 update into 1 file"),
            "{:?}",
            model.message
        );
        let written = std::fs::read_to_string(dir.path().join("package.json")).unwrap();
        assert_eq!(written, manifest.replace("3.22.0", "4.1.0"));
        assert_eq!(model.updates[0].pick, Pick::Applied);
    }

    /// A release the Count could not write into a declaration is a warning that
    /// names the file, so a dependency declared in two manifests can be told apart,
    /// and it is never a template for copier.
    #[test]
    fn notices_with_a_reason_are_warnings_that_name_their_file() {
        let mut report = report(&[("zod", "3.22.0", "4.1.0", "major")]);
        report.repositories[0].notices = serde_json::from_value(serde_json::json!([
            {"manager":"composer","path":"a/composer.json","line":3,"name":"acme/lib","package":"acme/lib","current":"^1 | ^2","available":"3.0.0","reason":"not rewritable"},
            {"manager":"composer","path":"b/composer.json","line":3,"name":"acme/lib","package":"acme/lib","current":"^1 | ^2","available":"3.0.0","reason":"not rewritable"}
        ]))
        .unwrap();
        let model = Model::new(&report, Origin::GitHub);
        assert!(model.templates.is_empty());
        let notes: Vec<_> = model
            .warnings
            .iter()
            .filter_map(|(_, warning)| match warning {
                Warning::Note(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        for path in ["a/composer.json", "b/composer.json"] {
            assert!(
                notes.contains(
                    &format!(
                        "acme/lib ^1 | ^2 → 3.0.0 available, not proposed: not rewritable ({path})"
                    )
                    .as_str()
                ),
                "{notes:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn template_notices_are_listed_and_copier_runs_only_after_a_confirmed_u() {
        let mut report = report(&[("zod", "3.22.0", "4.1.0", "major")]);
        report.repositories[0].notices = serde_json::from_value(serde_json::json!([{
            "manager":"copier","path":".copier-answers.yml","line":1,"name":"gh:acme/template",
            "package":"acme/template","current":"v1.4.0","available":"v1.6.0",
            "compareUrl":"https://github.com/acme/template/compare/v1.4.0...v1.6.0"
        }]))
        .unwrap();
        let mut remote = Model::new(&report, Origin::GitHub);
        assert!(snapshot(&remote).contains(
            "notice  template gh:acme/template: v1.4.0 → v1.6.0 available (.copier-answers.yml)"
        ));
        press(&mut remote, KeyCode::Char('4'));
        assert_eq!(remote.tab, Tab::Templates);
        let text = screen(&mut remote);
        assert!(
            text.contains("changes: https://github.com/acme/template/compare/v1.4.0...v1.6.0"),
            "{text}"
        );
        assert!(
            text.contains("gh:acme/template") && text.contains("v1.6.0"),
            "{text}"
        );
        press(&mut remote, KeyCode::Char(' '));
        press(&mut remote, KeyCode::Char('a'));
        assert!(remote.updates.iter().all(|u| u.pick != Pick::Selected));
        press(&mut remote, KeyCode::Char('u'));
        assert!(remote.message.as_deref().unwrap().contains("working tree"));
        assert_eq!(remote.mode, Mode::Browse);

        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join(".copier-answers.yml"), "_commit: v1.4.0\n").unwrap();
        std::fs::write(tree.join("patchula.pkl"), "amends \"PatchulaConfig.pkl\"\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."],
            &["commit", "-qm", "init"],
        ] {
            crate::copier::test_git(&tree, args);
        }
        let fake = dir.path().join("copier");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '_commit: v1.6.0\\n' > .copier-answers.yml\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = patchula_config::load(&tree).unwrap();
        let mut model = Model::new(
            &report,
            Origin::Local(Box::new(local(&tree, Some(config)).unwrap())),
        );
        press(&mut model, KeyCode::Char('4'));
        press(&mut model, KeyCode::Char('u'));
        assert!(
            model
                .prompt()
                .unwrap()
                .contains("--vcs-ref v1.6.0 for gh:acme/template? The template's code runs on this machine as you; its changes: https://github.com/acme/template/compare/v1.4.0...v1.6.0. y runs it")
        );
        press(&mut model, KeyCode::Char('n'));
        assert_eq!(model.mode, Mode::Browse);
        press(&mut model, KeyCode::Char('u'));
        press(&mut model, KeyCode::Char('y'));
        assert_eq!(model.mode, Mode::Copier(0));
        model.run_copier(&fake.to_string_lossy());
        assert_eq!(model.mode, Mode::Browse);
        assert!(model.templates[0].applied);
        assert!(
            model.message.as_deref().unwrap().contains(
                "Copier moved gh:acme/template to v1.6.0: 1 file changed (.copier-answers.yml)"
            ),
            "{:?}",
            model.message
        );
        press(&mut model, KeyCode::Char('u'));
        assert!(
            model
                .message
                .as_deref()
                .unwrap()
                .contains("already updated")
        );
    }

    fn local_model(manifest: &str) -> (tempfile::TempDir, Model) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), manifest).unwrap();
        std::fs::write(
            dir.path().join("patchula.pkl"),
            "amends \"PatchulaConfig.pkl\"\n",
        )
        .unwrap();
        let mut report = report(&[
            ("zod", "3.22.0", "4.1.0", "major"),
            ("hono", "4.6.0", "4.7.2", "minor"),
        ]);
        for dep in &mut report.repositories[0].package_files[0].dependencies {
            let start = manifest
                .find(&format!("\"{}\"", dep.current.value.as_ref().unwrap()))
                .unwrap();
            dep.metadata.as_mut().unwrap().locations[0].start = start;
        }
        let local = local(dir.path(), None).unwrap();
        (dir, Model::new(&report, Origin::Local(Box::new(local))))
    }
    const MANIFEST: &str =
        "{\n  \"dependencies\": {\n    \"zod\": \"3.22.0\",\n    \"hono\": \"4.6.0\"\n  }\n}\n";
    /// The screen with line breaks and runs of spaces collapsed, so text the
    /// footer wraps can be matched whole.
    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_key_in_the_table_does_what_the_bar_and_help_say() {
        for binding in KEYS {
            let tabs: Vec<Tab> = match binding.tab {
                Some(tab) => vec![tab],
                None => TABS.iter().map(|(t, _)| *t).collect(),
            };
            for tab in tabs {
                for key in binding.keys {
                    let modifiers = if key.ctrl {
                        KeyModifiers::CONTROL
                    } else {
                        KeyModifiers::NONE
                    };
                    let event = KeyEvent::new(key.code, modifiers);
                    assert_eq!(
                        super::binding(event, tab).map(|b| b.action),
                        Some(binding.action),
                        "{} on {tab:?}",
                        key.name()
                    );
                }
            }
        }
        // A terminal that reports Shift-g as a lowercase g with Shift.
        let shifted = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT);
        assert_eq!(
            super::binding(shifted, Tab::Updates).map(|b| b.action),
            Some(Action::Bottom)
        );
        assert_eq!(
            super::binding(KeyEvent::from(KeyCode::Char('A')), Tab::Updates).map(|b| b.action),
            None,
            "only lowercase a selects; apply is w"
        );
        assert_eq!(
            super::binding(KeyEvent::from(KeyCode::Char('w')), Tab::Stats).map(|b| b.action),
            None
        );
        let (_dir, mut model) = local_model(MANIFEST);
        for (width, height) in [(80, 24), (120, 40)] {
            model.mode = Mode::Help;
            let help = sized(&mut model, width, height);
            for binding in KEYS {
                assert!(help.contains(&binding.keys()), "{help}");
                assert!(help.contains(binding.help), "{help}");
            }
            press(&mut model, KeyCode::Char('x'));
            for (tab, _) in TABS {
                model.tab = tab;
                let bar = flat(&sized(&mut model, width, height));
                for binding in KEYS.iter().filter(|b| b.bar.is_some()) {
                    let label = format!("{} {}", binding.keys[0].name(), binding.bar.unwrap());
                    assert_eq!(
                        bar.contains(&label),
                        binding.on(tab),
                        "{label} on {tab:?} at {width}x{height}: {bar}"
                    );
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bar_says_why_apply_is_off() {
        let report = report(&[("zod", "3.22.0", "4.1.0", "major")]);
        let mut server = Model::new(&report, Origin::Server);
        let text = flat(&sized(&mut server, 80, 24));
        assert!(
            text.contains("w apply (off: this lookup ran on the server; pass --here"),
            "{text}"
        );
        let mut github = Model::new(&report, Origin::GitHub);
        let text = flat(&sized(&mut github, 80, 24));
        assert!(
            text.contains("w apply (off: these repositories come from GitHub"),
            "{text}"
        );
        let (_dir, mut model) = local_model(MANIFEST);
        let text = flat(&sized(&mut model, 80, 24));
        assert!(
            text.contains("w apply (off: select updates with space or a first)"),
            "{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_confirm_and_write_through_the_screen() {
        for (width, height) in [(80, 24), (120, 40)] {
            let (dir, mut model) = local_model(MANIFEST);
            press(&mut model, KeyCode::Char(' '));
            let text = flat(&sized(&mut model, width, height));
            assert!(text.contains("w apply 1 selected"), "{text}");
            press(&mut model, KeyCode::Char('w'));
            let text = flat(&sized(&mut model, width, height));
            assert!(
                text.contains(
                    "Write 1 selected update into the working tree? y writes, any other key cancels."
                ),
                "{text}"
            );
            press(&mut model, KeyCode::Char('n'));
            let text = flat(&sized(&mut model, width, height));
            assert!(
                text.contains("Apply cancelled; nothing was written."),
                "{text}"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("package.json")).unwrap(),
                MANIFEST
            );
            press(&mut model, KeyCode::Char('w'));
            press(&mut model, KeyCode::Char('y'));
            let text = flat(&sized(&mut model, width, height));
            assert!(
                text.contains("The Count wrote 1 update into 1 file."),
                "{text}"
            );
            assert!(text.contains("done"), "{text}");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("package.json")).unwrap(),
                MANIFEST.replace("3.22.0", "4.1.0")
            );
            press(&mut model, KeyCode::Char('j'));
            assert_eq!(model.message, None, "a key press clears the message");
            let text = flat(&sized(&mut model, width, height));
            assert!(text.contains("w apply (off: select updates"), "{text}");
        }
    }

    #[test]
    fn long_warnings_wrap_and_scroll_at_any_width() {
        let url = format!("https://registry.example.com/{}", "x".repeat(150));
        let long = format!(
            "lookup failed for a-very-long-package-name-that-keeps-going because the registry said no at {url} and then some more words until END-OF-WARNING"
        );
        let mut report = report(&[("zod", "3.22.0", "4.1.0", "major")]);
        report.repositories[0].warnings = (0..12).map(|i| format!("warning {i}: {long}")).collect();
        for (width, height) in [(30, 12), (80, 24), (120, 40)] {
            let mut model = Model::new(&report, Origin::GitHub);
            press(&mut model, KeyCode::Char('5'));
            let mut seen = String::new();
            let mut last = usize::MAX;
            loop {
                let text = sized(&mut model, width, height);
                for line in text.lines().skip(3) {
                    assert!(line.chars().count() <= usize::from(width));
                }
                seen.push_str(&text);
                seen.push('\n');
                if model.cursor == last {
                    break;
                }
                last = model.cursor;
                press(&mut model, KeyCode::Char('j'));
            }
            let joined: String = seen.split_whitespace().collect();
            for i in 0..12 {
                assert!(seen.contains(&format!("warning {i}:")), "{width}x{height}");
            }
            assert!(
                seen.matches("END-OF-WARNING").count() >= 12,
                "{width}x{height}"
            );
            assert!(joined.contains(&"x".repeat(20)));
            assert!(joined.contains("a-very-long-package-name-that-keeps-going") || width < 50);
        }
    }

    #[test]
    fn wrap_keeps_every_character_within_the_width() {
        let text = "short words then averyveryverylongunbrokentoken and é wide 日本語テキスト end";
        for width in [1, 3, 7, 12, 40] {
            let lines = wrap(text, width);
            for line in &lines {
                assert!(
                    Span::raw(line.as_str()).width() <= width.max(2),
                    "{line:?} at {width}"
                );
            }
            let joined: String = lines.concat().split_whitespace().collect();
            let original: String = text.split_whitespace().collect();
            assert_eq!(joined, original, "at {width}");
        }
    }

    #[test]
    fn long_messages_wrap_in_the_footer() {
        let report = report(&[("zod", "3.22.0", "4.1.0", "major")]);
        let mut model = Model::new(&report, Origin::Server);
        press(&mut model, KeyCode::Char('w'));
        let message = model.message.clone().unwrap();
        for (width, height) in [(40, 16), (80, 24)] {
            let text = flat(&sized(&mut model, width, height));
            assert!(text.contains(&flat(&message)), "{text}");
        }
    }

    #[test]
    fn targets_stay_whole_and_long_names_are_cut_in_the_middle() {
        let name = "cloud.google.com/go/auth/oauth2adapt/with/a/very/long/module/path";
        let target = "18-alpine@8d6421d663b4";
        let mut model = Model::new(
            &report(&[
                (name, "18-alpine", target, "pin"),
                ("zod", "3.22.0", "4.1.0", "major"),
            ]),
            Origin::GitHub,
        );
        for width in [80, 120] {
            let text = sized(&mut model, width, 24);
            assert!(text.contains(target), "{text}");
            assert!(text.contains("cloud.goog") && text.contains("…"), "{text}");
            assert!(text.contains("module/path"), "{text}");
            assert!(text.contains("package.json"), "{text}");
        }
        assert_eq!(middle("abcdefghij", 5), "ab…ij");
        assert_eq!(middle("abc", 5), "abc");
        assert_eq!(
            fit(30, &[(10, 4), (20, 4)], &[(0, 4)]),
            vec![8, 20],
            "the first column gives way to fit"
        );
        assert_eq!(
            fit(40, &[(5, 4), (5, 4)], &[]),
            vec![5, 33],
            "room goes to the last column"
        );
    }

    #[test]
    fn each_tab_keeps_its_cursor() {
        let mut model = Model::new(
            &report(&[
                ("zod", "3.22.0", "4.1.0", "major"),
                ("hono", "4.6.0", "4.7.2", "minor"),
                ("ms", "2.1.2", "2.1.3", "patch"),
            ]),
            Origin::GitHub,
        );
        screen(&mut model);
        press(&mut model, KeyCode::Char('j'));
        press(&mut model, KeyCode::Char('j'));
        press(&mut model, KeyCode::Char('3'));
        assert_eq!(model.cursor, 0, "a tab not visited yet starts at the top");
        press(&mut model, KeyCode::Char('j'));
        press(&mut model, KeyCode::Char('1'));
        assert_eq!(model.cursor, 2);
        press(&mut model, KeyCode::Tab);
        press(&mut model, KeyCode::Tab);
        assert_eq!((model.tab, model.cursor), (Tab::Dependencies, 1));
        press(&mut model, KeyCode::BackTab);
        press(&mut model, KeyCode::BackTab);
        assert_eq!((model.tab, model.cursor), (Tab::Updates, 2));
    }

    #[test]
    fn warnings_read_like_the_report_sections() {
        let mut report = report(&[("zod", "3.22.0", "4.1.0", "major")]);
        report.repositories[0].warnings = vec!["advisory lookup on OSV.dev failed".into()];
        report.repositories[0].lookup_failures = serde_json::from_value(serde_json::json!([
            {"manager": "mise", "path": "mise.toml", "name": "pipx:acme/tidy", "package": "pipx:acme/tidy",
             "datasource": "mise", "kind": "not-found", "message": "mise lists no versions of pipx:acme/tidy"},
            {"manager": "npm", "path": "package.json", "name": "@acme/gone", "package": "@acme/gone",
             "datasource": "npm", "kind": "not-found", "message": "HTTP 404"}
        ]))
        .unwrap();
        let mut model = Model::new(&report, Origin::GitHub);
        press(&mut model, KeyCode::Char('5'));
        let text = sized(&mut model, 100, 24);
        assert!(
            !text.contains("local/app"),
            "one repository needs no name: {text}"
        );
        assert!(text.contains("Lookup failures (2)"), "{text}");
        assert!(
            text.contains("  pipx:acme/tidy  mise.toml     mise, not-found"),
            "{text}"
        );
        assert!(
            text.contains("    mise lists no versions of pipx:acme/tidy"),
            "{text}"
        );
        assert!(
            text.contains("  @acme/gone      package.json  npm, not-found"),
            "{text}"
        );
        assert!(
            text.contains("Warnings (1)") && text.contains("  advisory lookup"),
            "{text}"
        );
        assert!(text.contains("j/k scroll  PgUp/PgDn page"), "{text}");
        assert!(
            snapshot(&model)
                .contains("warning local/app: lookup failed: pipx:acme/tidy in mise.toml")
        );
        press(&mut model, KeyCode::Char('1'));
        assert!(!flat(&screen(&mut model)).contains("j/k scroll"));
    }
}
