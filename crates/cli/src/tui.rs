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
    pub selected: bool,
    pub applied: bool,
}
impl Update {
    fn target(&self) -> String {
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

#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub repository: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub published_at: String,
    pub eligible_at: String,
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
    Warnings,
    Stats,
}
const TABS: [(Tab, &str); 5] = [
    (Tab::Updates, "Updates"),
    (Tab::Held, "Held back"),
    (Tab::Dependencies, "Dependencies"),
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
    pub help: bool,
    pub message: Option<String>,
    pub local: Option<Local>,
    pub quit: bool,
}

impl Model {
    pub fn new(report: &Report, local: Option<Local>) -> Self {
        let mut updates = Vec::new();
        let mut held = Vec::new();
        let mut found = Vec::new();
        let mut warnings = Vec::new();
        let mut managers = BTreeMap::<String, usize>::new();
        for repo in &report.repositories {
            for warning in &repo.warnings {
                warnings.push((repo.repository.clone(), warning.clone()));
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
                        current: current.clone(),
                        resolved: dep.current.version.clone(),
                        updates: dep.updates.len(),
                    });
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
                            published_at: update
                                .release_age
                                .as_ref()
                                .map(|a| a.published_at.clone()),
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
                            selected: false,
                            applied: false,
                        });
                    }
                    for hold in metadata.map(|m| m.held_back.as_slice()).unwrap_or_default() {
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
            KeyCode::Char(c @ '1'..='5') => {
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

    /// Write the selected updates into the local directory's files.
    fn apply(&mut self) -> Result<(usize, usize), String> {
        let local = self.local.as_ref().ok_or("not a local directory")?;
        let scanner = patchula_manager::Scanner::new(&local.config)?;
        let mut files = patchula_manager::Files::new();
        for update in self.updates.iter().filter(|u| u.selected) {
            if !files.contains_key(&update.file) {
                let text = std::fs::read_to_string(local.dir.join(&update.file))
                    .map_err(|e| format!("{}: {e}", update.file))?;
                files.insert(update.file.clone(), text);
            }
        }
        // Declarations are found again from the files as they are now, so an
        // edit can only land where the same dependency still is.
        let dependencies = scanner.extract(&files)?.dependencies;
        let mut edits = BTreeMap::<String, Vec<patchula_manager::Edit>>::new();
        let mut applied = Vec::new();
        for (index, update) in self.updates.iter().enumerate().filter(|(_, u)| u.selected) {
            let targets: Vec<_> = dependencies
                .iter()
                .filter(|d| {
                    d.file == update.file
                        && d.name == update.name
                        && update.starts.contains(&d.location.start)
                })
                .collect();
            if targets.is_empty() {
                return Err(format!(
                    "{} in {} has changed since the lookup; run the TUI again",
                    update.name, update.file
                ));
            }
            let value = update.value.as_deref().unwrap_or(&update.current);
            for dep in targets {
                let text = &files[&update.file];
                edits
                    .entry(update.file.clone())
                    .or_default()
                    .extend(patchula_manager::edits(
                        dep,
                        text,
                        value,
                        update.digest.as_deref(),
                    )?);
            }
            applied.push(index);
        }
        let mut written = BTreeMap::new();
        for (file, edits) in edits {
            written.insert(file.clone(), patchula_manager::apply(&files[&file], edits)?);
        }
        for (file, content) in &written {
            std::fs::write(local.dir.join(file), content).map_err(|e| format!("{file}: {e}"))?;
        }
        for &index in &applied {
            self.updates[index].selected = false;
            self.updates[index].applied = true;
        }
        Ok((applied.len(), written.len()))
    }
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
            if model.local.is_some() {
                " · space select · A apply"
            } else {
                ""
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
    let height = area.height.min(17);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let lines = [
        ("q, Esc, Ctrl-C", "quit"),
        ("Tab, Shift-Tab, ←, →", "switch tabs"),
        ("1 to 5", "jump to a tab"),
        ("j, k, ↑, ↓", "move"),
        ("PgUp, PgDn, g, G", "page, top, bottom"),
        ("/", "search names, packages and files"),
        ("t", "cycle the update type filter"),
        ("space", "select an update"),
        ("a", "select every visible update"),
        ("A", "apply the selection to the working tree"),
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
}
