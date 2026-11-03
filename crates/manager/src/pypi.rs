//! Which Python package index each package comes from: pinned in the
//! manifest, recorded in a lockfile, or the only index a project names. A
//! package that could come from several is ambiguous: its lookup fails rather
//! than guess, since the public package of its name may be someone else's.
use crate::*;
use std::collections::BTreeMap;

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
            None => Self::Unusable(raw.to_owned()),
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
        Origin::of(
            std::iter::once(Origin::Index(default))
                .chain(self.extras.iter().cloned().map(Origin::Index)),
        )
        .unwrap_or_else(|| Origin::Index(Named::Public))
    }
    /// Give each PyPI dependency in `deps` its registry, and drop those that
    /// do not come from an index.
    pub(crate) fn assign(
        &self,
        deps: &mut Vec<Dependency>,
        locked: impl Fn(&str) -> Option<Origin>,
    ) {
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
                Origin::Other => return false,
            }
            true
        });
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
}

/// `path` joined to the directory of `from`, with `.` and `..` resolved.
fn relative(from: &str, path: &str) -> Option<String> {
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
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or(line).trim();
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue };
        let (option, value) = match first.split_once('=') {
            Some((option, value)) if option.starts_with('-') => (option, Some(value.to_owned())),
            _ => (first, words.next().map(str::to_owned)),
        };
        match (option, value) {
            ("--no-index", _) => options.no_index = true,
            ("-i" | "--index-url", Some(value)) => options.indexes.push(Named::parse(&value)),
            ("--extra-index-url", Some(value)) => options.extras.push(Named::parse(&value)),
            ("-f" | "--find-links", Some(value)) => options.find_links.push(value),
            ("-r" | "--requirement" | "-c" | "--constraint", Some(value)) => {
                options.includes.extend(relative(path, &value));
            }
            _ => {}
        }
    }
    options
}

/// The indexes of a requirements file: pip applies index options across
/// every file one run reads, so those of the files it includes, and of the
/// files that include it, count too. `--find-links` adds a source that is not
/// an index, and `--no-index` leaves only those.
pub(crate) fn requirements(path: &str, files: &Files) -> Indexes {
    let options: BTreeMap<&str, RequirementsOptions> = files
        .iter()
        .map(|(path, text)| (path.as_str(), requirements_options(path, text)))
        .filter(|(_, options)| {
            options.no_index
                || !options.indexes.is_empty()
                || !options.extras.is_empty()
                || !options.find_links.is_empty()
                || !options.includes.is_empty()
        })
        .collect();
    let mut connected = vec![path.to_owned()];
    let mut index = 0;
    while let Some(current) = connected.get(index).cloned() {
        index += 1;
        let linked = options
            .get(current.as_str())
            .map(|o| o.includes.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(
                options
                    .iter()
                    .filter(|(_, o)| o.includes.contains(&current))
                    .map(|(p, _)| (*p).to_owned()),
            );
        for path in linked {
            if !connected.contains(&path) {
                connected.push(path);
            }
        }
    }
    let mut indexes = Indexes::default();
    let mut defaults: Vec<Named> = Vec::new();
    let mut find_links = Vec::new();
    let mut no_index = false;
    for options in connected.iter().filter_map(|p| options.get(p.as_str())) {
        for named in &options.indexes {
            if !defaults.contains(named) {
                defaults.push(named.clone());
            }
        }
        indexes.extras.extend(options.extras.iter().cloned());
        find_links.extend(options.find_links.iter().cloned());
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
