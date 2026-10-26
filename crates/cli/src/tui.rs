//! `patchula tui`: browse a lookup report, and in a working tree, apply
//! selected updates to the files on disk. Rendering is a pure function of [`Model`], so
//! tests draw frames on a test backend and drive it with key events.
use patchula_report::Report;
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

const CORAL: Color = Color::Rgb(232, 90, 79);
const GOLD: Color = Color::Rgb(242, 181, 68);
const PURPLE: Color = Color::Rgb(160, 120, 180);
const DIM: Color = Color::Rgb(140, 130, 150);

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

pub struct Model {
    pub tab: Tab,
    pub updates: Vec<Update>,
    pub held: Vec<Held>,
    pub templates: Vec<Template>,
    pub found: Vec<Found>,
    pub warnings: Vec<(String, String)>,
    pub stats: Vec<(String, String)>,
    pub repositories: usize,
    pub filter: Filter,
    pub search: String,
    pub searching: bool,
    pub cursor: usize,
    pub scroll: usize,
    pub confirming: bool,
    /// The template awaiting a y/n before copier runs.
    pub confirming_template: Option<usize>,
    /// The confirmed template; the terminal loop hands it to copier.
    pub copier: Option<usize>,
    pub help: bool,
    pub message: Option<String>,
    pub local: Option<Local>,
    pub quit: bool,
}

impl Model {
    pub fn new(report: &Report, local: Option<Local>) -> Self {
        let updates = updates(report);
        let mut held = Vec::new();
        let mut templates = Vec::new();
        let mut found = Vec::new();
        let mut warnings = Vec::new();
        let mut managers = BTreeMap::<String, usize>::new();
        for repo in &report.repositories {
            for warning in &repo.warnings {
                warnings.push((repo.repository.clone(), warning.clone()));
            }
            for notice in &repo.notices {
                templates.push(Template {
                    repository: repo.repository.clone(),
                    notice: notice.clone(),
                    applied: false,
                });
            }
            if let Some(reason) = &repo.reason {
                warnings.push((
                    repo.repository.clone(),
                    format!("{:?}: {reason}", repo.status),
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
            confirming: false,
            confirming_template: None,
            copier: None,
            help: false,
            message: None,
            local,
            quit: false,
        }
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
            Tab::Warnings => self.warnings.len(),
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
        if let Some(index) = self.confirming_template.take() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.copier = Some(index);
                self.message = Some("Copier is running in your terminal.".into());
            } else {
                self.message = Some("Copier was not run.".into());
            }
            return;
        }
        if self.confirming {
            self.confirming = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.message = Some(match self.apply() {
                    Ok((edits, files)) => format!(
                        "The Count applied {edits} update{} to {files} file{}. Refresh lockfiles with your package manager.",
                        plural(edits),
                        plural(files)
                    ),
                    Err(error) => format!("Nothing was written: {error}"),
                });
            } else {
                self.message = Some("Apply cancelled.".into());
            }
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        let rows = self.rows();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab | KeyCode::Right => self.switch(1),
            KeyCode::BackTab | KeyCode::Left => self.switch(TABS.len() - 1),
            KeyCode::Char(c @ '1'..='6') => {
                self.tab = TABS[(c as usize) - ('1' as usize)].0;
                self.cursor = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(rows.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::PageDown => self.cursor = (self.cursor + 10).min(rows.saturating_sub(1)),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = rows.saturating_sub(1),
            KeyCode::Char('/') => {
                self.searching = true;
                self.search.clear();
            }
            KeyCode::Char('t') if self.tab == Tab::Updates => {
                self.filter = self.filter.next();
                self.cursor = 0;
            }
            KeyCode::Char(' ') if self.tab == Tab::Updates => {
                if let Some(&index) = self.visible().get(self.cursor) {
                    let update = &mut self.updates[index];
                    if !update.applied {
                        update.selected = !update.selected;
                    }
                }
            }
            KeyCode::Char('a') if self.tab == Tab::Updates => {
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
            KeyCode::Char('u') if self.tab == Tab::Templates => {
                let Some(template) = self.templates.get(self.cursor) else {
                    return;
                };
                self.message = Some(if self.local.is_none() {
                    "Copier only runs on a working tree: run patchula tui in a checkout with no repository named.".into()
                } else if template.applied {
                    "Copier already updated this template. Review and commit its changes.".into()
                } else {
                    self.confirming_template = Some(self.cursor);
                    format!(
                        "Run copier update --vcs-ref {} for {}? The template's code runs on this machine as you. y/n",
                        template.notice.available, template.notice.name
                    )
                });
            }
            KeyCode::Char('A') if self.tab == Tab::Updates => {
                self.message = Some(if self.local.is_none() {
                    "Apply only works on a working tree: run patchula tui in a checkout with no repository named. For GitHub repositories, let `patchula run` open the PRs.".into()
                } else if self.selected() == 0 {
                    "Select updates with space first.".into()
                } else {
                    self.confirming = true;
                    format!(
                        "Apply {} selected update{}? y/n",
                        self.selected(),
                        plural(self.selected())
                    )
                });
            }
            _ => {}
        }
    }
    fn switch(&mut self, step: usize) {
        let index = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        self.tab = TABS[(index + step) % TABS.len()].0;
        self.cursor = 0;
    }

    /// Write the selected updates into the local directory's files, all or none.
    fn apply(&mut self) -> Result<(usize, usize), String> {
        let local = self.local.as_ref().ok_or("not a local directory")?;
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
        for &index in &selected {
            self.updates[index].selected = false;
            self.updates[index].applied = true;
        }
        Ok((selected.len(), plan.files.len()))
    }
    /// Run copier for the confirmed template, with the terminal handed over
    /// to it, and say what it changed.
    pub fn run_copier(&mut self, program: &str) {
        let (Some(index), Some(local)) = (self.copier.take(), self.local.as_ref()) else {
            return;
        };
        let notice = self.templates[index].notice.clone();
        self.message = Some(
            match crate::copier::update(program, &local.dir, &notice.path, &notice.available, false)
            {
                Ok(outcome) => {
                    self.templates[index].applied = true;
                    format!(
                        "Copier moved {} to {}: {} file{} changed{}. Review and commit them yourself.",
                        notice.name,
                        notice.available,
                        outcome.touched.len(),
                        plural(outcome.touched.len()),
                        if outcome.conflicts.is_empty() {
                            String::new()
                        } else {
                            format!(", conflicts in {}", outcome.conflicts.join(", "))
                        }
                    )
                }
                Err(error) => format!("Copier did not finish: {error}"),
            },
        );
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
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
fn kind_style(kind: &str) -> Style {
    Style::new().fg(match kind {
        "major" => CORAL,
        "minor" => GOLD,
        "patch" => Color::Green,
        _ => Color::Cyan,
    })
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
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let selected = model.selected();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Count Patchula ",
                Style::new().fg(Color::Black).bg(CORAL).bold(),
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
                Style::new().fg(GOLD),
            ),
            Span::styled(
                if model.local.is_some() { "  local" } else { "" },
                Style::new().fg(PURPLE),
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
        .highlight_style(Style::new().fg(CORAL).bold())
        .style(Style::new().fg(DIM)),
        tabs,
    );
    match model.tab {
        Tab::Updates => draw_updates(frame, model, body),
        Tab::Held => draw_held(frame, model, body),
        Tab::Templates => draw_templates(frame, model, body),
        Tab::Dependencies => draw_found(frame, model, body),
        Tab::Warnings => draw_list(
            frame,
            model,
            body,
            "Warnings",
            model
                .warnings
                .iter()
                .map(|(r, w)| format!("{r}: {w}"))
                .collect(),
        ),
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
    let hint = if model.searching {
        format!(" search: {}_", model.search)
    } else if let Some(message) = &model.message {
        format!(" {message}")
    } else {
        format!(
            " q quit · ? help · tab switch · j/k move · / search{} · t type: {}{}",
            if model.search.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", model.search)
            },
            model.filter.label(),
            match (model.local.is_some(), model.tab) {
                (true, Tab::Templates) => " · u copier update",
                (true, _) => " · space select · A apply",
                (false, _) => "",
            }
        )
    };
    frame.render_widget(Paragraph::new(hint).style(Style::new().fg(DIM)), footer);
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
    let rows: Vec<TableRow> = visible
        .iter()
        .map(|&i| {
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
                    Color::Green
                } else {
                    GOLD
                })),
                if u.fixes.is_empty() {
                    Cell::from(u.name.clone())
                } else {
                    Cell::from(format!("{} (security)", u.name)).style(Style::new().fg(Color::Red))
                },
                Cell::from(u.current.clone()).style(Style::new().fg(DIM)),
                Cell::from(u.target()),
                Cell::from(u.kind.clone()).style(kind_style(&u.kind)),
                Cell::from(age(u.published_at.as_deref())).style(Style::new().fg(DIM)),
                Cell::from(if many {
                    format!("{}:{}", u.repository, u.file)
                } else {
                    u.file.clone()
                })
                .style(Style::new().fg(DIM)),
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
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Fill(3),
                Constraint::Length(14),
                Constraint::Length(20),
                Constraint::Length(6),
                Constraint::Length(5),
                Constraint::Fill(2),
            ],
        )
        .header(
            TableRow::new(["", "Dependency", "Current", "Target", "Type", "Age", "File"])
                .style(Style::new().fg(PURPLE).add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::new().bg(Color::Rgb(50, 36, 60)))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(title)
                .title_style(Style::new().fg(CORAL)),
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
                    Style::new().fg(DIM),
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
                .style(Style::new().fg(DIM))
            } else {
                Line::from(format!("fixes {}", u.fixes.join(", ")))
                    .style(Style::new().fg(Color::Red))
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
                .border_style(Style::new().fg(DIM)),
        ),
        detail,
    );
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
                Cell::from(h.published_at.clone()).style(Style::new().fg(DIM)),
                Cell::from(h.eligible_at.clone()).style(Style::new().fg(GOLD)),
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
                .style(Style::new().fg(PURPLE).bold()),
        )
        .row_highlight_style(Style::new().bg(Color::Rgb(50, 36, 60)))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(
                    " Held back by the release-age wait ({}) ",
                    model.held.len()
                ))
                .title_style(Style::new().fg(CORAL)),
        ),
        area,
        &mut state,
    );
}

fn draw_templates(frame: &mut Frame, model: &mut Model, area: Rect) {
    let rows: Vec<TableRow> = if model.templates.is_empty() {
        vec![TableRow::new(["No template updates. The night is quiet."])]
    } else {
        model
            .templates
            .iter()
            .map(|t| {
                TableRow::new(vec![
                    Cell::from(if t.applied { "done" } else { "" })
                        .style(Style::new().fg(Color::Green)),
                    Cell::from(t.notice.name.clone()),
                    Cell::from(t.notice.current.clone()).style(Style::new().fg(DIM)),
                    Cell::from(t.notice.available.clone()).style(Style::new().fg(GOLD)),
                    Cell::from(t.notice.path.clone()).style(Style::new().fg(DIM)),
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
                .style(Style::new().fg(PURPLE).bold()),
        )
        .row_highlight_style(Style::new().bg(Color::Rgb(50, 36, 60)))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(
                    " Template updates: reported, never edited ({}) ",
                    model.templates.len()
                ))
                .title_style(Style::new().fg(CORAL)),
        ),
        area,
        &mut state,
    );
}

fn draw_found(frame: &mut Frame, model: &mut Model, area: Rect) {
    let found: Vec<&Found> = model
        .found
        .iter()
        .filter(|f| model.query_matches(&[&f.name, &f.file, &f.manager]))
        .collect();
    let rows: Vec<TableRow> = found
        .iter()
        .map(|f| {
            TableRow::new(vec![
                Cell::from(f.manager.clone()).style(Style::new().fg(PURPLE)),
                Cell::from(f.name.clone()),
                Cell::from(f.current.clone()),
                Cell::from(f.resolved.clone().unwrap_or_default()).style(Style::new().fg(DIM)),
                Cell::from(if f.updates > 0 {
                    f.updates.to_string()
                } else {
                    String::new()
                })
                .style(Style::new().fg(GOLD)),
                Cell::from(f.file.clone()).style(Style::new().fg(DIM)),
            ])
        })
        .collect();
    let count = found.len();
    let mut state = table_state(model, count, area.height);
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(14),
                Constraint::Percentage(30),
                Constraint::Percentage(16),
                Constraint::Percentage(14),
                Constraint::Length(7),
                Constraint::Fill(1),
            ],
        )
        .header(
            TableRow::new([
                "Manager",
                "Dependency",
                "Current",
                "Resolved",
                "Updates",
                "File",
            ])
            .style(Style::new().fg(PURPLE).bold()),
        )
        .row_highlight_style(Style::new().bg(Color::Rgb(50, 36, 60)))
        .block(
            Block::new()
                .borders(Borders::TOP)
                .title(format!(" Dependencies ({count}) "))
                .title_style(Style::new().fg(CORAL)),
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
            .row_highlight_style(Style::new().bg(Color::Rgb(50, 36, 60)))
            .block(
                Block::new()
                    .borders(Borders::TOP)
                    .title(format!(" {title} ({count}) "))
                    .title_style(Style::new().fg(CORAL)),
            ),
        area,
        &mut state,
    );
}

fn draw_help(frame: &mut Frame) {
    let area = frame.area();
    let width = area.width.min(64);
    let height = area.height.min(18);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let lines = [
        ("q, Esc, Ctrl-C", "quit"),
        ("Tab, Shift-Tab, ←, →", "switch tabs"),
        ("1 to 6", "jump to a tab"),
        ("j, k, ↑, ↓", "move"),
        ("PgUp, PgDn, g, G", "page, top, bottom"),
        ("/", "search names, packages and files"),
        ("t", "cycle the update type filter"),
        ("space", "select an update"),
        ("a", "select every visible update"),
        ("A", "apply the selection to the working tree"),
        ("u", "run copier update for a template, after y"),
    ];
    let text: Vec<Line> = lines
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!("{key:<22}"), Style::new().fg(GOLD)),
                Span::raw(*what),
            ])
        })
        .chain([
            Line::from(""),
            Line::from("Any key closes this.").style(Style::new().fg(DIM)),
        ])
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .title(" The Count's keys ")
                .title_style(Style::new().fg(CORAL))
                .border_style(Style::new().fg(PURPLE)),
        ),
        popup,
    );
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
        out.push_str(&format!("warning {repo}: {warning}\n"));
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
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
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
            None,
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
        press(&mut model, KeyCode::Char('A'));
        assert!(model.message.as_deref().unwrap().contains("working tree"));
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
        let mut model = Model::new(&report, Some(local(dir.path(), Some(config)).unwrap()));
        press(&mut model, KeyCode::Char('A'));
        assert!(model.message.as_deref().unwrap().contains("Select updates"));
        press(&mut model, KeyCode::Char(' '));
        press(&mut model, KeyCode::Char('A'));
        assert!(model.confirming);
        press(&mut model, KeyCode::Char('y'));
        assert!(
            model
                .message
                .as_deref()
                .unwrap()
                .contains("applied 1 update to 1 file"),
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
            "package":"acme/template","current":"v1.4.0","available":"v1.6.0"
        }]))
        .unwrap();
        let mut remote = Model::new(&report, None);
        assert!(snapshot(&remote).contains(
            "notice  template gh:acme/template: v1.4.0 → v1.6.0 available (.copier-answers.yml)"
        ));
        press(&mut remote, KeyCode::Char('4'));
        assert_eq!(remote.tab, Tab::Templates);
        let text = screen(&mut remote);
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
        let mut model = Model::new(&report, Some(local(&tree, Some(config)).unwrap()));
        press(&mut model, KeyCode::Char('4'));
        press(&mut model, KeyCode::Char('u'));
        assert!(
            model
                .message
                .as_deref()
                .unwrap()
                .contains("--vcs-ref v1.6.0")
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
            model
                .message
                .as_deref()
                .unwrap()
                .contains("Copier moved gh:acme/template to v1.6.0: 1 file changed"),
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
}
