//! The report as a person reads it: counts first, then each repository's
//! updates, vulnerabilities, failed lookups, notices, skipped values and
//! warnings. On a terminal it is coloured and fitted to the width; anywhere
//! else it is plain text, one item per line, for grep and logs, with only the
//! summary wrapped, at 80 columns.
use crate::{cli, palette, tui};
use patchula_report::{ActionStatus, Report, Repository, Status};
use std::fmt::Write;

/// The width the summary wraps to when the stream is not a terminal.
const PIPED_WIDTH: usize = 80;

/// How text is drawn on one stream.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    color: bool,
    /// The terminal's width; none when the stream is not a terminal, so
    /// nothing is wrapped or cut.
    width: Option<usize>,
}

/// What a span means, painted from [`crate::palette`].
#[derive(Clone, Copy)]
pub enum Paint {
    Plain,
    Bold,
    Dim,
    Accent,
    AccentDim,
    Success,
    Warning,
    Error,
}

impl Style {
    /// Plain text: no colour, no width, no emoji.
    #[cfg(test)]
    pub const PLAIN: Self = Self {
        color: false,
        width: None,
    };

    /// The style for a stream. NO_COLOR turns colour off unless
    /// --color always asks for it.
    pub fn new(choice: cli::Color, stream: &console::Term) -> Self {
        let terminal = stream.is_term();
        Self {
            color: colored(choice, terminal),
            width: terminal.then(|| usize::from(stream.size().1).max(40)),
        }
    }

    pub fn stdout(choice: cli::Color) -> Self {
        Self::new(choice, &console::Term::stdout())
    }

    pub fn stderr(choice: cli::Color) -> Self {
        Self::new(choice, &console::Term::stderr())
    }

    /// The Count's bat, on a terminal only.
    pub fn bat(&self) -> &'static str {
        if self.width.is_some() { "🦇 " } else { "" }
    }

    pub fn paint(&self, paint: Paint, text: &str) -> String {
        let colour = |colour: palette::Colour| colour.sgr(palette::depth());
        let code = match paint {
            Paint::Plain => return text.to_owned(),
            Paint::Bold => "1".to_owned(),
            Paint::Dim => "2".to_owned(),
            Paint::Accent => colour(palette::ACCENT),
            Paint::AccentDim => colour(palette::ACCENT_DIM),
            Paint::Success => colour(palette::SUCCESS),
            Paint::Warning => colour(palette::WARNING),
            Paint::Error => colour(palette::ERROR),
        };
        if self.color && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint(Paint::Bold, text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(Paint::Dim, text)
    }

    /// A section heading with its count.
    pub fn heading(
        &self,
        out: &mut String,
        indent: usize,
        title: &str,
        count: usize,
        paint: Paint,
    ) {
        let _ = writeln!(
            out,
            "\n{:indent$}{} {}",
            "",
            self.paint(paint, &self.bold(title)),
            self.dim(&format!("({count})"))
        );
    }

    /// Rows in aligned columns. On a terminal the flex column is cut to fit
    /// the width, down to 8 characters; the other columns are never cut, so a
    /// row whose fixed columns are wider than the terminal overflows rather
    /// than losing a version. Plain rows are never cut.
    pub fn table(
        &self,
        out: &mut String,
        indent: usize,
        rows: &[Vec<(String, Paint)>],
        flex: usize,
    ) {
        for line in self.lines(indent, rows, flex) {
            let _ = writeln!(out, "{line}");
        }
    }

    /// The lines of a table, for callers that put more under each row.
    pub fn lines(&self, indent: usize, rows: &[Vec<(String, Paint)>], flex: usize) -> Vec<String> {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths: Vec<usize> = (0..columns)
            .map(|c| {
                rows.iter()
                    .filter_map(|row| row.get(c))
                    .map(|(text, _)| console::measure_text_width(text))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        if let (Some(width), Some(last)) = (self.width, columns.checked_sub(1))
            && flex < columns
        {
            let others: usize = widths
                .iter()
                .enumerate()
                .filter(|(c, _)| *c != flex)
                .map(|(c, w)| if c == last { *w } else { w + 2 })
                .sum();
            let gap = if flex == last { 0 } else { 2 };
            widths[flex] = widths[flex].min(width.saturating_sub(indent + others + gap).max(8));
        }
        let mut lines = Vec::new();
        for row in rows {
            let mut line = " ".repeat(indent);
            for (c, (text, paint)) in row.iter().enumerate() {
                let text = if self.width.is_some() && c == flex {
                    console::truncate_str(text, widths[c], "…").into_owned()
                } else {
                    text.clone()
                };
                line.push_str(&self.paint(*paint, &text));
                if c + 1 < row.len() {
                    let pad = widths[c].saturating_sub(console::measure_text_width(&text));
                    line.push_str(&" ".repeat(pad + 2));
                }
            }
            lines.push(line.trim_end().to_owned());
        }
        lines
    }

    /// A message that wraps to the terminal's width, and stays on one line
    /// anywhere else.
    pub fn wrap(&self, out: &mut String, indent: usize, text: &str, paint: Paint) {
        let prefix = " ".repeat(indent);
        match self.width {
            Some(width) => {
                for line in textwrap::wrap(text, width.saturating_sub(indent).max(20)) {
                    let _ = writeln!(out, "{prefix}{}", self.paint(paint, &line));
                }
            }
            None => {
                let _ = writeln!(out, "{prefix}{}", self.paint(paint, text));
            }
        }
    }

    /// The report's summary: prose, so it wraps at word boundaries everywhere,
    /// to the terminal's width or to [`PIPED_WIDTH`] in a pipe, where nothing
    /// else wraps it and a terminal showing the pipe would split words.
    fn summary(&self, out: &mut String, text: &str, paint: Paint) {
        for line in textwrap::wrap(text, self.width.unwrap_or(PIPED_WIDTH)) {
            let _ = writeln!(out, "{}", self.paint(paint, &line));
        }
    }

    /// A URL on its own line: never cut, so it stays clickable.
    fn url(&self, out: &mut String, indent: usize, url: &str) {
        let _ = writeln!(out, "{:indent$}{}", "", self.dim(url));
    }
}

/// Whether to colour a stream: --color, then NO_COLOR, then whether it is a
/// terminal.
pub fn colored(choice: cli::Color, terminal: bool) -> bool {
    match choice {
        cli::Color::Always => true,
        cli::Color::Never => false,
        cli::Color::Auto => terminal && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
    }
}

/// "1 update", "2 updates".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "a, b and c".
fn sentence(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// What a report holds, counted.
#[derive(Default)]
struct Counts {
    dependencies: usize,
    updates: usize,
    vulnerabilities: usize,
    failures: usize,
    notices: usize,
    skipped: usize,
    warnings: usize,
}

impl Counts {
    fn of(repo: &Repository) -> Self {
        let deps = || repo.package_files.iter().flat_map(|f| &f.dependencies);
        Self {
            dependencies: deps().count(),
            updates: deps().map(|d| d.updates.len()).sum(),
            vulnerabilities: deps()
                .filter_map(|d| d.metadata.as_ref())
                .map(|m| m.advisories.len())
                .sum(),
            failures: repo.lookup_failures.len(),
            notices: repo.notices.len(),
            skipped: repo.skipped.len(),
            warnings: repo.warnings.len(),
        }
    }

    fn add(&mut self, other: &Self) {
        self.dependencies += other.dependencies;
        self.updates += other.updates;
        self.vulnerabilities += other.vulnerabilities;
        self.failures += other.failures;
        self.notices += other.notices;
        self.skipped += other.skipped;
        self.warnings += other.warnings;
    }

    /// Dependencies and updates always; the rest when there are any.
    fn phrases(&self) -> Vec<String> {
        let mut parts = vec![
            count(self.dependencies, "dependency", "dependencies"),
            count(self.updates, "update", "updates"),
        ];
        for (n, one, many) in [
            (self.vulnerabilities, "vulnerability", "vulnerabilities"),
            (self.failures, "failed lookup", "failed lookups"),
            (self.notices, "notice", "notices"),
            (self.skipped, "skipped value", "skipped values"),
            (self.warnings, "warning", "warnings"),
        ] {
            if n > 0 {
                parts.push(count(n, one, many));
            }
        }
        parts
    }
}

fn status_paint(status: ActionStatus) -> Paint {
    match status {
        ActionStatus::Created
        | ActionStatus::Updated
        | ActionStatus::Rebased
        | ActionStatus::Merged => Paint::Success,
        ActionStatus::Planned => Paint::AccentDim,
        ActionStatus::Skipped | ActionStatus::Protected => Paint::Warning,
        ActionStatus::Error => Paint::Error,
        ActionStatus::Unchanged | ActionStatus::Deleted | ActionStatus::Closed => Paint::Dim,
    }
}

/// An enum's name as the JSON report spells it.
fn word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn kind_paint(kind: &str) -> Paint {
    match kind {
        "major" => Paint::Error,
        "minor" => Paint::Warning,
        "patch" => Paint::Success,
        _ => Paint::AccentDim,
    }
}

fn severity_paint(severity: &str) -> Paint {
    match severity {
        "critical" | "high" => Paint::Error,
        "moderate" | "medium" => Paint::Warning,
        _ => Paint::Dim,
    }
}

/// The whole report of a lookup or run, headed by what the Count found. A
/// lookup lists each update; a run lists its branches and pull requests.
pub fn report(report: &Report, style: &Style) -> String {
    let mut out = String::new();
    let mut total = Counts::default();
    for repo in &report.repositories {
        total.add(&Counts::of(repo));
    }
    let errors = report
        .repositories
        .iter()
        .filter(|r| r.status == Status::Error)
        .count();
    let single = report.repositories.len() == 1;
    let headline = match report.repositories.as_slice() {
        [] => "The Count found no repositories to visit.".to_owned(),
        [repo] if repo.status == Status::Error => format!(
            "The Count could not read {}: {}",
            repo.repository,
            repo.reason.as_deref().unwrap_or("it failed")
        ),
        [repo] => format!(
            "The Count visited {} and found {}.",
            repo.repository,
            sentence(&total.phrases())
        ),
        repos => {
            let mut line = format!(
                "The Count visited {} and found {}.",
                count(repos.len(), "repository", "repositories"),
                sentence(&total.phrases())
            );
            if errors > 0 {
                let _ = write!(
                    line,
                    " {} could not be read.",
                    count(errors, "repository", "repositories")
                );
            }
            line
        }
    };
    let paint = if single && errors > 0 {
        Paint::Error
    } else {
        Paint::Bold
    };
    style.summary(&mut out, &format!("{}{headline}", style.bat()), paint);
    let updates = tui::updates(report);
    let indent = if single { 0 } else { 2 };
    for repo in &report.repositories {
        if !single {
            let (state, paint) = match repo.status {
                Status::Complete => (sentence(&Counts::of(repo).phrases()), Paint::Dim),
                Status::Skipped => (
                    format!("skipped: {}", repo.reason.as_deref().unwrap_or("no reason")),
                    Paint::Warning,
                ),
                Status::Error => (
                    format!("error: {}", repo.reason.as_deref().unwrap_or("no reason")),
                    Paint::Error,
                ),
            };
            let _ = writeln!(
                out,
                "\n{}  {}",
                style.bold(&repo.repository),
                style.paint(paint, &state)
            );
        } else if repo.status == Status::Skipped {
            style.wrap(
                &mut out,
                0,
                &format!("skipped: {}", repo.reason.as_deref().unwrap_or("no reason")),
                Paint::Warning,
            );
        }
        if let Some(actions) = &report.actions {
            actions_section(&mut out, style, indent, actions, &repo.repository);
        } else {
            let mut rows: Vec<&tui::Update> = updates
                .iter()
                .filter(|u| u.repository == repo.repository)
                .collect();
            rows.sort_by(|a, b| (&a.file, &a.name).cmp(&(&b.file, &b.name)));
            if !rows.is_empty() {
                style.heading(&mut out, indent, "Updates", rows.len(), Paint::Accent);
                let rows: Vec<_> = rows
                    .iter()
                    .map(|u| {
                        vec![
                            (u.kind.clone(), kind_paint(&u.kind)),
                            (u.name.clone(), Paint::Bold),
                            (format!("{} → {}", u.current, u.target()), Paint::Plain),
                            (u.file.clone(), Paint::Dim),
                        ]
                    })
                    .collect();
                style.table(&mut out, indent + 2, &rows, 3);
            }
        }
        details(&mut out, style, indent, repo);
    }
    out
}

/// One repository's branches and pull requests from a run, with each
/// dry-run diff after its branch.
fn actions_section(
    out: &mut String,
    style: &Style,
    indent: usize,
    actions: &patchula_report::Actions,
    repository: &str,
) {
    let branches: Vec<_> = actions
        .branches
        .iter()
        .filter(|b| b.repository == repository)
        .collect();
    if !branches.is_empty() {
        style.heading(out, indent, "Branches", branches.len(), Paint::Accent);
        let rows: Vec<_> = branches
            .iter()
            .map(|branch| {
                let mut notes = Vec::new();
                if !branch.files.is_empty() {
                    notes.push(count(branch.files.len(), "file", "files"));
                }
                if branch.lockfile_pending {
                    notes.push("lockfile pending".to_owned());
                }
                let note = notes.join(", ");
                vec![
                    (word(&branch.status), status_paint(branch.status)),
                    (branch.branch.clone(), Paint::Bold),
                    (note, Paint::Dim),
                ]
            })
            .collect();
        for (line, branch) in style.lines(indent + 2, &rows, 1).iter().zip(branches) {
            let _ = writeln!(out, "{line}");
            if let Some(reason) = &branch.reason {
                style.wrap(out, indent + 4, reason, Paint::Dim);
            }
            if actions.dry_run {
                for file in &branch.files {
                    for line in file.diff.lines() {
                        let paint = match line.chars().next() {
                            Some('+') => Paint::Success,
                            Some('-') => Paint::Error,
                            Some('@') => Paint::AccentDim,
                            _ => Paint::Plain,
                        };
                        let _ = writeln!(out, "{}", style.paint(paint, line));
                    }
                }
            }
        }
    }
    let prs: Vec<_> = actions
        .pull_requests
        .iter()
        .filter(|p| p.repository == repository)
        .collect();
    if !prs.is_empty() {
        style.heading(out, indent, "Pull requests", prs.len(), Paint::Accent);
        let rows: Vec<_> = prs
            .iter()
            .map(|pr| {
                vec![
                    (word(&pr.status), status_paint(pr.status)),
                    (
                        pr.number.map(|n| format!("#{n}")).unwrap_or_default(),
                        Paint::Plain,
                    ),
                    (pr.branch.clone(), Paint::Bold),
                ]
            })
            .collect();
        for (line, pr) in style.lines(indent + 2, &rows, 2).iter().zip(prs) {
            let _ = writeln!(out, "{line}");
            if let Some(url) = &pr.url {
                style.url(out, indent + 4, url);
            }
            if let Some(reason) = &pr.reason {
                style.wrap(out, indent + 4, reason, Paint::Dim);
            }
        }
    }
}

/// Vulnerabilities, failed lookups, notices, skipped values and warnings.
fn details(out: &mut String, style: &Style, indent: usize, repo: &Repository) {
    let advisories: Vec<_> = repo
        .package_files
        .iter()
        .flat_map(|f| &f.dependencies)
        .filter_map(|d| d.metadata.as_ref().map(|m| (d, m)))
        .flat_map(|(d, m)| m.advisories.iter().map(move |a| (d, a)))
        .collect();
    if !advisories.is_empty() {
        style.heading(
            out,
            indent,
            "Vulnerabilities",
            advisories.len(),
            Paint::Error,
        );
        let rows: Vec<_> = advisories
            .iter()
            .map(|(dep, advisory)| {
                let severity = advisory
                    .severity
                    .as_deref()
                    .unwrap_or("unknown")
                    .to_ascii_lowercase();
                let cves: Vec<_> = advisory
                    .aliases
                    .iter()
                    .filter(|a| a.starts_with("CVE-"))
                    .cloned()
                    .collect();
                let id = if cves.is_empty() {
                    advisory.id.clone()
                } else {
                    format!("{} ({})", advisory.id, cves.join(", "))
                };
                let current = dep
                    .current
                    .version
                    .clone()
                    .or_else(|| dep.current.value.clone())
                    .unwrap_or_default();
                let paint = severity_paint(&severity);
                vec![
                    (severity, paint),
                    (format!("{} {current}", dep.name), Paint::Bold),
                    (id, Paint::Plain),
                    (
                        advisory
                            .fixed_in
                            .as_ref()
                            .map_or("no fix yet".into(), |v| format!("fixed in {v}")),
                        Paint::Plain,
                    ),
                ]
            })
            .collect();
        let lines = style.lines(indent + 2, &rows, usize::MAX);
        for (line, (_, advisory)) in lines.iter().zip(&advisories) {
            let _ = writeln!(out, "{line}");
            if let Some(summary) = &advisory.summary {
                style.wrap(out, indent + 4, summary, Paint::Dim);
            }
        }
    }
    failures(
        out,
        style,
        indent,
        &repo.lookup_failures.iter().collect::<Vec<_>>(),
    );
    notices(
        out,
        style,
        indent,
        "Notices",
        &repo.notices.iter().collect::<Vec<_>>(),
    );
    if !repo.skipped.is_empty() {
        style.heading(out, indent, "Skipped", repo.skipped.len(), Paint::Accent);
        let rows: Vec<_> = repo
            .skipped
            .iter()
            .map(|s| {
                vec![
                    (
                        format!(
                            "{}{}",
                            s.path,
                            s.line.map_or(String::new(), |l| format!(":{l}"))
                        ),
                        Paint::Dim,
                    ),
                    (s.value.clone(), Paint::Plain),
                    (s.reason.clone(), Paint::Dim),
                ]
            })
            .collect();
        style.table(out, indent + 2, &rows, 2);
    }
    warnings(out, style, indent, &repo.warnings);
}

/// Failed lookups, each with its message under it.
pub fn failures(
    out: &mut String,
    style: &Style,
    indent: usize,
    failures: &[&patchula_report::LookupFailure],
) {
    if !failures.is_empty() {
        style.heading(out, indent, "Lookup failures", failures.len(), Paint::Error);
        let rows: Vec<_> = failures
            .iter()
            .map(|failure| {
                let mut source = failure.datasource.clone();
                if failure.package != failure.name {
                    source = format!("{source} {}", failure.package);
                }
                vec![
                    (failure.name.clone(), Paint::Bold),
                    (failure.path.clone(), Paint::Dim),
                    (format!("{source}, {}", failure.kind.as_str()), Paint::Plain),
                ]
            })
            .collect();
        let lines = style.lines(indent + 2, &rows, 2);
        for (line, failure) in lines.iter().zip(failures) {
            let _ = writeln!(out, "{line}");
            style.wrap(out, indent + 4, &failure.message, Paint::Plain);
        }
    }
}

/// Notices, each with its compare link under it.
pub fn notices(
    out: &mut String,
    style: &Style,
    indent: usize,
    title: &str,
    notices: &[&patchula_report::Notice],
) {
    if !notices.is_empty() {
        style.heading(out, indent, title, notices.len(), Paint::Accent);
        let rows: Vec<_> = notices
            .iter()
            .map(|notice| {
                vec![
                    (notice.text(), Paint::Plain),
                    (notice.path.clone(), Paint::Dim),
                ]
            })
            .collect();
        for (line, notice) in style.lines(indent + 2, &rows, 1).iter().zip(notices) {
            let _ = writeln!(out, "{line}");
            if let Some(url) = &notice.compare_url {
                style.url(out, indent + 4, url);
            }
        }
    }
}

pub fn warnings(out: &mut String, style: &Style, indent: usize, warnings: &[String]) {
    if !warnings.is_empty() {
        style.heading(out, indent, "Warnings", warnings.len(), Paint::Warning);
        for warning in warnings {
            style.wrap(out, indent + 2, warning, Paint::Plain);
        }
    }
}

#[cfg(test)]
mod tests;
