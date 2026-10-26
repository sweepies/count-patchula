//! `patchula tui`: browse a lookup report, and in a working tree, apply
//! selected updates to the files on disk. Rendering is a pure function of [`Model`], so
//! tests draw frames on a test backend and drive it with key events.
use crate::palette;
use patchula_report::{LookupFailure, Report};
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
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// One proposed update, flattened from the report.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub repository: String,
    pub file: String,
    pub manager: String,
    pub name: String,
    pub package: String,
    pub current: String,
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    pub kind: String,
    pub published_at: Option<String>,
    /// Advisories this update leaves behind, with their CVE aliases.
    pub fixes: Vec<String>,
    /// Byte offsets of every occurrence, used to apply the edit locally.
    pub starts: Vec<usize>,
    /// A package only its lockfile names, which only its package manager can update.
    pub transitive: bool,
    /// The current version came from a lockfile, which an edit here leaves stale.
    pub locked: bool,
    pub selected: bool,
    pub applied: bool,
}
impl Update {
    pub fn target(&self) -> String {
        match (&self.value, &self.version, &self.digest) {
            (Some(value), _, Some(digest)) => format!("{value}@{}", short(digest)),
            (Some(value), _, None) => value.clone(),
            (None, Some(version), _) => version.clone(),
            (None, None, Some(digest)) => short(digest),
            (None, None, None) => self.current.clone(),
        }
    }
}
fn short(digest: &str) -> String {
    digest
        .split(':')
        .next_back()
        .unwrap_or(digest)
        .chars()
        .take(12)
        .collect()
}

/// Every proposed update in the report, one per dependency and target.
pub fn updates(report: &Report) -> Vec<Update> {
    let mut updates = Vec::new();
    for repo in &report.repositories {
        for file in &repo.package_files {
            for dep in &file.dependencies {
                let current = dep
                    .current
                    .value
                    .clone()
                    .or_else(|| dep.current.version.clone())
                    .unwrap_or_default();
                let metadata = dep.metadata.as_ref();
                let starts: Vec<usize> = metadata
                    .map(|m| m.locations.iter().map(|l| l.start).collect())
                    .unwrap_or_default();
                for update in &dep.updates {
                    updates.push(Update {
                        repository: repo.repository.clone(),
                        file: file.path.clone(),
                        manager: dep.manager.clone(),
                        name: dep.name.clone(),
                        package: dep.package.clone(),
                        current: current.clone(),
                        value: update.new_value.clone(),
                        version: update.new_version.clone(),
                        digest: update.new_digest.clone(),
                        kind: update.update_type.clone(),
                        published_at: update.release_age.as_ref().map(|a| a.published_at.clone()),
                        fixes: update
                            .fixes
                            .iter()
                            .map(|id| {
                                let cves: Vec<_> = metadata
                                    .into_iter()
                                    .flat_map(|m| &m.advisories)
                                    .filter(|a| &a.id == id)
                                    .flat_map(|a| &a.aliases)
                                    .filter(|alias| alias.starts_with("CVE-"))
                                    .cloned()
                                    .collect();
                                if cves.is_empty() {
                                    id.clone()
                                } else {
                                    format!("{id} ({})", cves.join(", "))
                                }
                            })
                            .collect(),
                        starts: starts.clone(),
                        transitive: metadata
                            .is_some_and(|m| m.kinds.iter().any(|k| k == "transitive")),
                        locked: metadata
                            .and_then(|m| m.resolution.as_ref())
                            .is_some_and(|r| r.kind == "lockfile"),
                        selected: false,
                        applied: false,
                    });
                }
            }
        }
    }
    updates
}

#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub repository: String,
    pub name: String,
    pub version: String,
    pub kind: String,
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
    pub manager: String,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Major,
    Minor,
    Patch,
    Pin,
    Digest,
}
impl Filter {
    fn next(self) -> Self {
        match self {
            Filter::All => Filter::Major,
            Filter::Major => Filter::Minor,
            Filter::Minor => Filter::Patch,
            Filter::Patch => Filter::Pin,
            Filter::Pin => Filter::Digest,
            Filter::Digest => Filter::All,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Major => "major",
            Filter::Minor => "minor",
            Filter::Patch => "patch",
            Filter::Pin => "pin",
            Filter::Digest => "digest",
        }
    }
    fn matches(self, kind: &str) -> bool {
        self == Filter::All || self.label() == kind
    }
}

/// Where selected updates can be written. Only local directories are editable.
pub struct Local {
    pub dir: PathBuf,
    pub config: patchula_config::Config,
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
    pub searching: bool,
    pub cursor: usize,
    pub scroll: usize,
    /// Each tab's cursor and scroll while another tab is shown.
    places: [(usize, usize); TABS.len()],
    pub confirming: bool,
    /// The template awaiting a y/n before copier runs.
    pub confirming_template: Option<usize>,
    /// The confirmed template; the terminal loop hands it to copier.
    pub copier: Option<usize>,
    pub help: bool,
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
                templates.push(Template {
                    repository: repo.repository.clone(),
                    notice: notice.clone(),
                    applied: false,
                });
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
                    *managers.entry(dep.manager.clone()).or_default() += 1;
                    let current = dep
                        .current
                        .value
                        .clone()
                        .or_else(|| dep.current.version.clone())
                        .unwrap_or_default();
                    found.push(Found {
                        repository: repo.repository.clone(),
                        file: file.path.clone(),
                        manager: dep.manager.clone(),
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
                            kind: hold.update_type.clone(),
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
            filter: Filter::All,
            search: String::new(),
            searching: false,
            cursor: 0,
            scroll: 0,
            places: [(0, 0); TABS.len()],
            confirming: false,
            confirming_template: None,
            copier: None,
            help: false,
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
        if self.confirming {
            let n = self.selected();
            return Some(format!(
                "Write {n} selected update{} into the working tree? y writes, any other key cancels.",
                plural(n)
            ));
        }
        let template = self.templates.get(self.confirming_template?)?;
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
                self.filter.matches(&u.kind)
                    && self.query_matches(&[&u.name, &u.package, &u.file, &u.manager])
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
                .filter(|f| self.query_matches(&[&f.name, &f.file, &f.manager]))
                .count(),
            Tab::Warnings => self.warning_rows,
            Tab::Stats => self.stats.len(),
        }
    }
    fn selected(&self) -> usize {
        self.updates.iter().filter(|u| u.selected).count()
    }

    pub fn key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.searching {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.searching = false,
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
        if let Some(index) = self.confirming_template.take() {
            self.message = Some(if yes {
                self.copier = Some(index);
                "Copier is running in your terminal.".into()
            } else {
                "Copier was not run.".into()
            });
            return;
        }
        if self.confirming {
            self.confirming = false;
            self.message = Some(if yes {
                match self.apply() {
                    Ok((edits, files)) => format!(
                        "The Count wrote {edits} update{} into {files} file{}. Refresh lockfiles with your package manager.",
                        plural(edits),
                        plural(files)
                    ),
                    Err(error) => format!("Nothing was written: {error}"),
                }
            } else {
                "Apply cancelled; nothing was written.".into()
            });
            return;
        }
        self.message = None;
        if self.help {
            self.help = false;
            return;
        }
        let Some(binding) = binding(key, self.tab) else {
            return;
        };
        let rows = self.rows();
        let tab = self.tab;
        match binding.action {
            Action::Quit => self.quit = true,
            Action::Help => self.help = true,
            Action::NextTab => self.switch(1),
            Action::PreviousTab => self.switch(TABS.len() - 1),
            Action::JumpTab => {
                if let KeyCode::Char(c @ '1'..='6') = key.code {
                    self.show((c as usize) - ('1' as usize));
                }
            }
            Action::Down => self.cursor = (self.cursor + 1).min(rows.saturating_sub(1)),
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::PageDown => self.cursor = (self.cursor + 10).min(rows.saturating_sub(1)),
            Action::PageUp => self.cursor = self.cursor.saturating_sub(10),
            Action::Top => self.cursor = 0,
            Action::Bottom => self.cursor = rows.saturating_sub(1),
            Action::Search => {
                self.searching = true;
                self.search.clear();
            }
            Action::Filter => {
                self.filter = self.filter.next();
                self.cursor = 0;
            }
            Action::Select => {
                if let Some(&index) = self.visible().get(self.cursor) {
                    let update = &mut self.updates[index];
                    if !update.applied {
                        update.selected = !update.selected;
                    }
                }
            }
            Action::SelectAll => {
                let visible = self.visible();
                let select = visible
                    .iter()
                    .any(|&i| !self.updates[i].selected && !self.updates[i].applied);
                for i in visible {
                    if !self.updates[i].applied {
                        self.updates[i].selected = select;
                    }
                }
            }
            Action::Apply => match self.apply_blocked() {
                Some(reason) => self.message = Some(format!("The Count cannot apply: {reason}.")),
                None => self.confirming = true,
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
                    None => self.confirming_template = Some(self.cursor),
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

    /// Write the selected updates into the local directory's files, all or none.
    fn apply(&mut self) -> Result<(usize, usize), String> {
        let Origin::Local(local) = &self.origin else {
            return Err("not a local directory".into());
        };
        let selected: Vec<usize> = (0..self.updates.len())
            .filter(|&i| self.updates[i].selected)
            .collect();
        let plan = plan(
            local,
            &selected
                .iter()
                .map(|&i| &self.updates[i])
                .collect::<Vec<_>>(),
        )?;
        if let Some((_, reason)) = plan.skipped.first() {
            return Err(reason.clone());
        }
        plan.write(&local.dir)?;
        self.uncommitted = uncommitted(&local.dir);
        for &index in &selected {
            self.updates[index].selected = false;
            self.updates[index].applied = true;
        }
        Ok((selected.len(), plan.files.len()))
    }
    /// Run copier for the confirmed template, with the terminal handed over
    /// to it, and say what it changed.
    pub fn run_copier(&mut self, program: &str) {
        let (Some(index), Origin::Local(local)) = (self.copier.take(), &self.origin) else {
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

/// What writing a set of updates into a working tree changes.
pub struct Plan {
    /// Each changed file's text before and after.
    pub files: BTreeMap<String, (String, String)>,
    /// Positions, in the updates planned, of those the new files carry.
    pub applied: Vec<usize>,
    /// Positions of the updates that cannot be written in place, with why.
    pub skipped: Vec<(usize, String)>,
}
impl Plan {
    pub fn write(&self, dir: &Path) -> Result<(), String> {
        for (file, (_, content)) in &self.files {
            std::fs::write(dir.join(file), content).map_err(|e| format!("{file}: {e}"))?;
        }
        Ok(())
    }
}

/// Plan the edits for these updates in the working tree, the way the Count
/// edits a branch. An update that cannot be written in place is skipped with
/// its reason, and the first update to claim a declaration keeps it.
pub fn plan(local: &Local, updates: &[&Update]) -> Result<Plan, String> {
    let scanner = patchula_manager::Scanner::new(&local.config)?;
    // Declarations are found again from the tree as it is now, every file the
    // lookup read included, so an edit can only land where the same
    // dependency still is.
    let (_, files) =
        patchula_engine::read_tree(&local.dir, &scanner).map_err(|e| format!("{e}"))?;
    let dependencies = scanner.extract(&files)?.dependencies;
    let mut skipped = Vec::new();
    let mut claimed = BTreeMap::<(&str, usize), &Update>::new();
    let mut edits = BTreeMap::<String, Vec<patchula_manager::Edit>>::new();
    let mut applied = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        if update.transitive {
            skipped.push((
                index,
                format!(
                    "{} is only in {}; update it with your package manager",
                    update.name, update.file
                ),
            ));
            continue;
        }
        let targets: Vec<_> = dependencies
            .iter()
            .filter(|d| {
                d.file == update.file
                    && d.name == update.name
                    && d.package == update.package
                    && d.current == update.current
                    && update.starts.contains(&d.location.start)
            })
            .collect();
        if targets.is_empty() {
            skipped.push((
                index,
                format!(
                    "{} in {} has changed since the lookup; run it again",
                    update.name, update.file
                ),
            ));
            continue;
        }
        if let Some(other) = targets
            .iter()
            .find_map(|d| claimed.get(&(d.file.as_str(), d.location.start)))
        {
            skipped.push((
                index,
                format!(
                    "{} in {} already takes {} {}",
                    update.name,
                    update.file,
                    other.kind,
                    other.target()
                ),
            ));
            continue;
        }
        let value = update.value.as_deref().unwrap_or(&update.current);
        let text = &files[&update.file];
        let mut found = Vec::new();
        let mut failed = None;
        for dep in &targets {
            match patchula_manager::edits(dep, text, value, update.digest.as_deref()) {
                Ok(more) => found.extend(more),
                Err(error) => failed = Some(format!("{}: {error}", update.file)),
            }
        }
        if let Some(reason) = failed {
            skipped.push((index, reason));
            continue;
        }
        if found
            .iter()
            .all(|e| text.get(e.start..e.end) == Some(e.replacement.as_str()))
        {
            skipped.push((
                index,
                format!(
                    "{} in {} already allows {}; only its lockfile changes, so update it with your package manager",
                    update.name,
                    update.file,
                    update.target()
                ),
            ));
            continue;
        }
        for dep in targets {
            claimed.insert((&dep.file, dep.location.start), update);
        }
        edits.entry(update.file.clone()).or_default().extend(found);
        applied.push(index);
    }
    let mut changed = BTreeMap::new();
    for (file, edits) in edits {
        let old = &files[&file];
        let new = patchula_manager::apply(old, edits)?;
        if &new != old {
            changed.insert(file, (old.clone(), new));
        }
    }
    skipped.sort();
    Ok(Plan {
        files: changed,
        applied,
        skipped,
    })
}
/// Whether the checkout holding `dir` has changes Git would commit; false
/// when Git cannot tell, and copier then says why itself.
fn uncommitted(dir: &Path) -> bool {
    crate::copier::changed(dir).is_ok_and(|paths| !paths.is_empty())
}
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
fn kind_style(kind: &str) -> Style {
    Style::new().fg(match kind {
        "major" => palette::ERROR,
        "minor" => palette::WARNING,
        "patch" => palette::SUCCESS,
        _ => palette::ACCENT_DIM,
    }
    .tui())
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
    let (status, style) = if model.searching {
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
    if model.help {
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
                u.kind.clone(),
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
            let mark = if u.applied {
                "done"
            } else if u.selected {
                "[x]"
            } else {
                "[ ]"
            };
            TableRow::new(vec![
                Cell::from(mark).style(Style::new().fg(if u.applied {
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
                Cell::from(kind).style(kind_style(&u.kind)),
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
                Cell::from(h.kind.clone()).style(kind_style(&h.kind)),
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
        .filter(|f| model.query_matches(&[&f.name, &f.file, &f.manager]))
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
            (natural(0, &|f| width(&f.manager)), 6),
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
                Cell::from(f.manager.clone()).style(Style::new().fg(palette::ACCENT_DIM.tui())),
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
            let name = failures
                .iter()
                .map(|f| f.name.chars().count())
                .max()
                .unwrap_or(0);
            let path = failures
                .iter()
                .map(|f| f.path.chars().count())
                .max()
                .unwrap_or(0);
            for failure in failures {
                let mut source = failure.datasource.clone();
                if failure.package != failure.name {
                    source = format!("{source} {}", failure.package);
                }
                let source = format!("{source}, {}", failure.kind.as_str());
                let row = format!(
                    "{:pad$}{:<name$}  {:<path$}  {source}",
                    "",
                    failure.name,
                    failure.path,
                    pad = indent + 2
                );
                if Span::raw(row.as_str()).width() <= width {
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(indent + 2)),
                        Span::styled(format!("{:<name$}  ", failure.name), Style::new().bold()),
                        Span::styled(
                            format!("{:<path$}  ", failure.path),
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
    let width = width.max(1);
    let columns = |s: &str| Span::raw(s).width();
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let mut word = word;
            loop {
                let gap = usize::from(!line.is_empty());
                if columns(&line) + gap + columns(word) <= width {
                    if gap == 1 {
                        line.push(' ');
                    }
                    line.push_str(word);
                    break;
                }
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                let mut split = 0;
                for (index, c) in word.char_indices() {
                    let end = index + c.len_utf8();
                    if split > 0 && columns(&word[..end]) > width {
                        break;
                    }
                    split = end;
                }
                lines.push(word[..split].to_owned());
                word = &word[split..];
                if word.is_empty() {
                    break;
                }
            }
        }
        lines.push(line);
    }
    lines
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
            if model.copier.is_some() {
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

/// The local directory's config, as the lookup used it.
pub fn local(dir: &Path, config: Option<patchula_config::Config>) -> Result<Local, String> {
    let config = match config {
        Some(config) => config,
        None => patchula_config::load(dir).map_err(|e| format!("{e:?}"))?,
    };
    Ok(Local {
        dir: dir.to_owned(),
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
            "schemaVersion": 2,
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
        assert_eq!(model.filter, Filter::Major);
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
        assert!(!model.confirming);
        press(&mut model, KeyCode::Char('3'));
        assert_eq!(model.tab, Tab::Dependencies);
        press(&mut model, KeyCode::Char('?'));
        assert!(screen(&mut model).contains("The Count's keys"));
        press(&mut model, KeyCode::Char('x'));
        press(&mut model, KeyCode::Char('q'));
        assert!(model.quit);
        assert!(snapshot(&model).contains("major   zod 3.22.0 -> 4.1.0  (package.json)"));
    }

    #[test]
    fn selected_updates_are_written_to_local_files() {
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
        assert!(model.confirming);
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
        assert!(model.updates[0].applied && !model.updates[0].selected);
    }

    #[test]
    fn template_notices_are_listed_and_copier_runs_only_after_a_confirmed_u() {
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
        assert!(remote.updates.iter().all(|u| !u.selected));
        press(&mut remote, KeyCode::Char('u'));
        assert!(remote.message.as_deref().unwrap().contains("working tree"));
        assert_eq!((remote.confirming_template, remote.copier), (None, None));

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
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&tree)
                .status()
                .unwrap();
            assert!(status.success());
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
        assert_eq!(model.copier, None);
        press(&mut model, KeyCode::Char('u'));
        press(&mut model, KeyCode::Char('y'));
        assert_eq!(model.copier, Some(0));
        model.run_copier(&fake.to_string_lossy());
        assert_eq!(model.copier, None);
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

    #[test]
    fn plan_skips_what_cannot_be_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        // hono was edited after the lookup, at the same place.
        let manifest = "{\n  \"dependencies\": {\n    \"zod\": \"3.22.0\",\n    \"hono\": \"4.7.0\",\n    \"ms\": \"^2.1.0\"\n  }\n}\n";
        std::fs::write(dir.path().join("package.json"), manifest).unwrap();
        std::fs::write(
            dir.path().join("patchula.pkl"),
            "amends \"PatchulaConfig.pkl\"\n",
        )
        .unwrap();
        let mut report = report(&[
            ("zod", "3.22.0", "4.1.0", "major"),
            ("hono", "4.6.0", "4.7.2", "minor"),
            ("lodash", "4.17.20", "4.17.21", "patch"),
            ("ms", "^2.1.0", "^2.1.0", "patch"),
        ]);
        let deps = &mut report.repositories[0].package_files[0].dependencies;
        for (dep, value) in deps.iter_mut().zip(["3.22.0", "4.7.0", "", "^2.1.0"]) {
            if let Some(start) = manifest.find(&format!("\"{value}\"")) {
                dep.metadata.as_mut().unwrap().locations[0].start = start;
            }
        }
        deps[3].updates[0].new_version = Some("2.1.3".into());
        let mut minor = deps[0].updates[0].clone();
        minor.new_value = Some("3.25.0".into());
        minor.update_type = "minor".into();
        deps[0].updates.push(minor);
        let mut updates = updates(&report);
        let lodash = updates.iter_mut().find(|u| u.name == "lodash").unwrap();
        lodash.transitive = true;
        lodash.file = "package-lock.json".into();
        let local = local(dir.path(), None).unwrap();
        let plan = plan(&local, &updates.iter().collect::<Vec<_>>()).unwrap();
        let targets: Vec<_> = plan.applied.iter().map(|&i| updates[i].target()).collect();
        assert_eq!(targets, ["4.1.0"]);
        let reasons: Vec<_> = plan.skipped.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "zod in package.json already takes major 4.1.0",
                "hono in package.json has changed since the lookup; run it again",
                "lodash is only in package-lock.json; update it with your package manager",
                "ms in package.json already allows ^2.1.0; only its lockfile changes, so update it with your package manager",
            ]
        );
        assert_eq!(
            plan.files["package.json"].1,
            manifest.replace("3.22.0", "4.1.0")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("package.json")).unwrap(),
            manifest,
            "planning writes nothing"
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

    #[test]
    fn every_key_in_the_table_does_what_the_bar_and_help_say() {
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
            model.help = true;
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

    #[test]
    fn the_bar_says_why_apply_is_off() {
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

    #[test]
    fn apply_confirm_and_write_through_the_screen() {
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
             "datasource": "npm", "kind": "not-found", "message": "registry HTTP 404"}
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
