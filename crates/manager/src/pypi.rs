//! Which Python package index each package comes from: pinned in the
//! manifest, recorded in a lockfile, or the only index a project names. A
//! package that could come from several is ambiguous: its lookup fails rather
//! than guess, since the public package of its name may be someone else's.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// What an index setting names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    /// PyPI itself.
    Public,
    Url(String),
    /// A value the Count cannot read from, such as `${VAR}` or plain http,
    /// kept as written so its packages' lookups fail.
    Unusable(String),
}

impl Named {
    pub(crate) fn parse(value: &str) -> Self {
        let raw = value.trim().trim_matches(['"', '\'']);
        if public(raw) {
            return Self::Public;
        }
        match crate::registry_url(raw) {
            Some(url) => Self::Url(url),
            None => Self::Unusable(crate::redacted(raw)),
        }
    }
    /// The registry a dependency records: none for PyPI.
    pub(crate) fn recorded(&self) -> Option<String> {
        match self {
            Self::Public => None,
            Self::Url(value) | Self::Unusable(value) => Some(value.clone()),
        }
    }
    fn label(&self) -> String {
        self.recorded()
            .unwrap_or_else(|| "https://pypi.org/simple".into())
    }
}

/// Whether `url` is PyPI's own index, over https or plain http.
fn public(url: &str) -> bool {
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let host = host
        .strip_suffix(":443")
        .or_else(|| host.strip_suffix(":80"))
        .unwrap_or(&host);
    matches!(
        host,
        "pypi.org" | "pypi.python.org" | "files.pythonhosted.org"
    )
}

/// Where a package comes from, as a pin or a lockfile records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Index(Named),
    /// Any of several indexes, which the Count does not choose between.
    Ambiguous(Vec<Named>),
    /// Not an index at all: git, a path, a URL or a workspace member. The
    /// Count leaves such a package alone, since its name on PyPI may be
    /// someone else's.
    Other,
}

impl Origin {
    /// One origin for several that should agree: the same, or ambiguous.
    pub(crate) fn of(origins: impl IntoIterator<Item = Origin>) -> Option<Self> {
        let mut indexes: Vec<Named> = Vec::new();
        for origin in origins {
            match origin {
                Origin::Other => return Some(Origin::Other),
                Origin::Index(named) => indexes.push(named),
                Origin::Ambiguous(named) => indexes.extend(named),
            }
        }
        indexes.dedup();
        let mut unique: Vec<Named> = Vec::new();
        for named in indexes {
            if !unique.contains(&named) {
                unique.push(named);
            }
        }
        match unique.len() {
            0 => None,
            1 => unique.pop().map(Origin::Index),
            _ => Some(Origin::Ambiguous(unique)),
        }
    }
}

/// A project's indexes.
#[derive(Debug, Default)]
pub(crate) struct Indexes {
    /// The index that replaces PyPI for packages pinned nowhere.
    pub default: Option<Named>,
    /// Indexes searched besides it. Any that is not PyPI makes every package
    /// pinned nowhere ambiguous.
    pub extras: Vec<Named>,
    /// Packages pinned by the project, by normalised name.
    pub pinned: BTreeMap<String, Origin>,
}

impl Indexes {
    /// Where `package` (normalised) comes from: its pin, else what its
    /// lockfile records, else the project's only index, else ambiguous.
    fn origin(&self, package: &str, locked: Option<Origin>) -> Origin {
        if let Some(origin) = self.pinned.get(package).cloned().or(locked) {
            return origin;
        }
        let default = self.default.clone().unwrap_or(Named::Public);
        let candidates = std::iter::once(Origin::Index(default))
            .chain(self.extras.iter().cloned().map(Origin::Index));
        match Origin::of(candidates) {
            Some(origin) => origin,
            None => Origin::Index(Named::Public),
        }
    }
    /// Give each PyPI dependency in `deps` its registry, and drop those that
    /// do not come from an index, returning them.
    pub(crate) fn assign(
        &self,
        deps: &mut Vec<Dependency>,
        locked: impl Fn(&str) -> Option<Origin>,
    ) -> Vec<Dependency> {
        let mut dropped = Vec::new();
        deps.retain_mut(|dep| {
            if dep.source != Source::Pypi {
                return true;
            }
            match self.origin(&dep.package, locked(&dep.package)) {
                Origin::Index(named) => dep.registry = named.recorded(),
                Origin::Ambiguous(named) => {
                    let labels: Vec<_> = named.iter().map(Named::label).collect();
                    dep.registry = Some(PackageId::ambiguous_registry(&labels));
                }
                Origin::Other => {
                    dropped.push(dep.clone());
                    return false;
                }
            }
            true
        });
        dropped
    }
}

/// One requirements file's options that decide where packages come from.
#[derive(Debug, Default)]
struct RequirementsOptions {
    indexes: Vec<Named>,
    extras: Vec<Named>,
    find_links: Vec<String>,
    no_index: bool,
    /// `-r` and `-c` files, as repository paths.
    includes: Vec<String>,
    /// `-r` and `-c` values that name no file in the repository: absolute
    /// paths, URLs, `${VAR}` paths, or paths outside it.
    unresolved: Vec<String>,
}

/// `path` joined to the directory of `from`, with `.` and `..` resolved; none
/// outside the repository.
fn relative(from: &str, path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains("://") || path.contains('$') {
        return None;
    }
    let mut parts: Vec<&str> = from
        .rsplit_once('/')
        .map_or(Vec::new(), |(dir, _)| dir.split('/').collect());
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn requirements_options(path: &str, text: &str) -> RequirementsOptions {
    let mut options = RequirementsOptions::default();
    // pip drops a byte order mark, and a backslash at the end of a line
    // continues it.
    let text = text.trim_start_matches('\u{feff}');
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    for line in joined.lines() {
        let line = line.split(" #").next().unwrap_or(line).trim();
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue };
        // `--option=value`, `-xvalue` and `-x value` are all one option.
        let joined_short = ["-r", "-c", "-i", "-f"]
            .iter()
            .find(|short| first.starts_with(*short) && first.len() > 2)
            .map(|short| (*short, Some(first[2..].to_owned())));
        let (option, value) = match (joined_short, first.split_once('=')) {
            (Some(joined), _) => joined,
            (None, Some((option, value))) if option.starts_with("--") => {
                (option, Some(value.to_owned()))
            }
            _ => (first, words.next().map(str::to_owned)),
        };
        match (option, value) {
            ("--no-index", _) => options.no_index = true,
            ("-i" | "--index-url", Some(value)) => options.indexes.push(Named::parse(&value)),
            ("--extra-index-url", Some(value)) => options.extras.push(Named::parse(&value)),
            ("-f" | "--find-links", Some(value)) => options.find_links.push(value),
            ("-r" | "--requirement" | "-c" | "--constraint", Some(value)) => {
                let value = value.trim_matches(['"', '\'']);
                match relative(path, value) {
                    Some(target) => options.includes.push(target),
                    None => options.unresolved.push(crate::redacted(value)),
                }
            }
            _ => {}
        }
    }
    options
}

/// The requirements files of a scan and every file they include with `-r`
/// or `-c`, whatever its name, with the options each sets.
pub(crate) struct RequirementsGraph {
    options: BTreeMap<String, RequirementsOptions>,
}

impl RequirementsGraph {
    /// Start from the files `selected` picks and follow their includes
    /// through `files`.
    pub(crate) fn new(files: &Files, selected: impl Fn(&str) -> bool) -> Self {
        let mut options = BTreeMap::new();
        let mut pending: Vec<String> = files.keys().filter(|p| selected(p)).cloned().collect();
        while let Some(path) = pending.pop() {
            if options.contains_key(&path) {
                continue;
            }
            let Some(text) = files.get(&path) else {
                continue;
            };
            let parsed = requirements_options(&path, text);
            pending.extend(parsed.includes.iter().cloned());
            options.insert(path, parsed);
        }
        Self { options }
    }
    /// The files the graph includes but `files` does not hold.
    pub(crate) fn missing(&self) -> BTreeSet<String> {
        self.options
            .values()
            .flat_map(|o| &o.includes)
            .filter(|path| !self.options.contains_key(*path))
            .cloned()
            .collect()
    }
    /// The files one pip run reading `root` reads: it and what it includes.
    fn closure<'a>(&'a self, root: &'a str) -> Vec<&'a str> {
        let mut seen = vec![root];
        let mut index = 0;
        while let Some(&path) = seen.get(index) {
            index += 1;
            for target in self.options.get(path).into_iter().flat_map(|o| &o.includes) {
                if !seen.contains(&target.as_str()) {
                    seen.push(target);
                }
            }
        }
        seen
    }
}

/// The indexes of a requirements file. pip applies index options across
/// every file one run reads, so a file gets the options of each run that
/// reads it: those of every file no other includes whose includes reach it.
/// `--find-links` adds a source that is not an index, `--no-index` leaves
/// only those, and an include the Count cannot read could set anything.
pub(crate) fn requirements(path: &str, graph: &RequirementsGraph) -> Indexes {
    let included: BTreeSet<&str> = graph
        .options
        .values()
        .flat_map(|o| &o.includes)
        .map(String::as_str)
        .collect();
    let mut runs: Vec<Vec<&str>> = graph
        .options
        .keys()
        .map(String::as_str)
        .filter(|root| !included.contains(root))
        .map(|root| graph.closure(root))
        .filter(|run| run.contains(&path))
        .collect();
    if runs.is_empty() {
        runs.push(graph.closure(path));
    }
    let mut read: Vec<&str> = runs.concat();
    read.sort_unstable();
    read.dedup();
    let mut indexes = Indexes::default();
    let mut defaults: Vec<Named> = Vec::new();
    let mut find_links = Vec::new();
    let mut no_index = false;
    for file in &read {
        let Some(options) = graph.options.get(*file) else {
            indexes.extras.push(Named::Unusable(format!("-r {file}")));
            continue;
        };
        for named in &options.indexes {
            if !defaults.contains(named) {
                defaults.push(named.clone());
            }
        }
        indexes.extras.extend(options.extras.iter().cloned());
        indexes.extras.extend(
            options
                .unresolved
                .iter()
                .map(|value| Named::Unusable(format!("-r {value}"))),
        );
        find_links.extend(options.find_links.iter().map(|link| crate::redacted(link)));
        no_index |= options.no_index;
    }
    if no_index {
        let mut only: Vec<String> = vec!["--no-index".into()];
        only.extend(find_links.iter().map(|link| format!("--find-links {link}")));
        indexes.default = Some(Named::Unusable(only.join(" ")));
        indexes.extras.clear();
        return indexes;
    }
    let mut defaults = defaults.into_iter();
    indexes.default = defaults.next();
    indexes.extras.extend(defaults);
    indexes.extras.extend(
        find_links
            .into_iter()
            .map(|link| Named::Unusable(format!("--find-links {link}"))),
    );
    indexes
}
