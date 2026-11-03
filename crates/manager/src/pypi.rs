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

/// A project's indexes.
#[derive(Debug, Default)]
pub(crate) struct Indexes {
    /// The index that replaces PyPI for packages pinned nowhere.
    pub default: Option<Named>,
    /// Indexes searched besides it. Any that is not PyPI makes every package
    /// pinned nowhere ambiguous.
    pub extras: Vec<Named>,
    /// Packages pinned to one index, by normalised name.
    pub pinned: BTreeMap<String, Named>,
}

impl Indexes {
    /// The registry `package` (normalised) records: its pin, else the index
    /// its lockfile names, else the project's only index, else ambiguous.
    pub(crate) fn registry(&self, package: &str, locked: Option<Named>) -> Option<String> {
        if let Some(named) = self.pinned.get(package).or(locked.as_ref()) {
            return named.recorded();
        }
        let default = self.default.clone().unwrap_or(Named::Public);
        let mut candidates = vec![default.clone()];
        for extra in &self.extras {
            if !candidates.contains(extra) {
                candidates.push(extra.clone());
            }
        }
        if candidates.len() == 1 {
            return default.recorded();
        }
        let labels: Vec<_> = candidates.iter().map(Named::label).collect();
        Some(PackageId::ambiguous_registry(&labels))
    }
    /// Give each PyPI dependency in `deps` its registry.
    pub(crate) fn assign(&self, deps: &mut [Dependency], locked: impl Fn(&str) -> Option<Named>) {
        for dep in deps.iter_mut().filter(|dep| dep.source == Source::Pypi) {
            dep.registry = self.registry(&dep.package, locked(&dep.package));
        }
    }
}

/// `--index-url`, `-i` and `--extra-index-url` in a requirements file.
pub(crate) fn requirements(text: &str) -> Indexes {
    let mut indexes = Indexes::default();
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or(line).trim();
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue };
        let (option, value) = match first.split_once('=') {
            Some((option, value)) => (option, Some(value.to_owned())),
            None => (first, words.next().map(str::to_owned)),
        };
        let Some(value) = value else { continue };
        match option {
            "-i" | "--index-url" => indexes.default = Some(Named::parse(&value)),
            "--extra-index-url" => indexes.extras.push(Named::parse(&value)),
            _ => {}
        }
    }
    indexes
}
