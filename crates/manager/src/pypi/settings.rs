//! uv and Poetry index settings, read into types that describe every key
//! the Count understands and reject the rest, so `[[x]]` and `x = [{...}]`
//! read alike and nothing a repository wrote is skipped unread. A section
//! that does not fit its type is unreadable, and its packages' lookups fail.
//!
//! Every key uv 0.13.6 accepts is either read here or listed as irrelevant
//! to where packages come from; a test with the real uv checks the lists
//! against uv's own. PDM, Rye, Pixi and Hatch settings that could name an
//! index are not read, and any present fails the lookups they could affect.
//!
//! Errors are never repeated in reports: serde's messages can quote values.
use super::{Indexes, Named, Origin, shown};
use crate::*;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A section read strictly: `None` when present but not understood.
#[derive(Debug)]
struct Strict<T>(Option<T>);

impl<'de, T: DeserializeOwned> Deserialize<'de> for Strict<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self(serde_json::from_value(value).ok()))
    }
}

/// One item or a list of them, as uv writes sources.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn items(&self) -> &[T] {
        match self {
            Self::One(item) => std::slice::from_ref(item),
            Self::Many(items) => items,
        }
    }
}

/// uv settings the Count reads; every other key uv accepts is in
/// [`UV_IRRELEVANT`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct UvSettings {
    #[serde(default)]
    index: Vec<UvIndex>,
    index_url: Option<String>,
    #[serde(default)]
    extra_index_url: Vec<String>,
    #[serde(default)]
    find_links: Vec<String>,
    #[serde(default)]
    no_index: bool,
    #[serde(default)]
    no_sources: bool,
    #[serde(default)]
    no_sources_package: Vec<String>,
    torch_backend: Option<Value>,
    #[serde(default)]
    sources: BTreeMap<String, OneOrMany<UvSource>>,
    #[serde(default)]
    override_dependencies: Vec<String>,
    #[serde(default)]
    constraint_dependencies: Vec<String>,
    workspace: Option<UvWorkspace>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

/// Every other setting uv 0.13.6 accepts in `uv.toml` and `[tool.uv]`,
/// none of which moves a package to another index.
pub const UV_IRRELEVANT: &[&str] = &[
    "required-version",
    "system-certs",
    "native-tls",
    "offline",
    "no-cache",
    "cache-dir",
    "preview",
    "preview-features",
    "python-preference",
    "python-downloads",
    "concurrent-downloads",
    "concurrent-builds",
    "concurrent-installs",
    // Only chooses among several indexes, which are ambiguous anyway.
    "index-strategy",
    "keyring-provider",
    "http-proxy",
    "https-proxy",
    "no-proxy",
    "allow-insecure-host",
    "resolution",
    "prerelease",
    "prerelease-package",
    "fork-strategy",
    // Static metadata for a package, not where it comes from.
    "dependency-metadata",
    "config-settings",
    "config-settings-package",
    "no-build-isolation",
    "require-build-hashes",
    "no-build-isolation-package",
    "extra-build-dependencies",
    "extra-build-variables",
    "exclude-newer",
    "exclude-newer-package",
    "link-mode",
    "compile-bytecode",
    "upgrade",
    "upgrade-package",
    "reinstall",
    "reinstall-package",
    "no-build",
    "no-build-package",
    "no-binary",
    "no-binary-package",
    "python-install-mirror",
    "pypy-install-mirror",
    "graalpy-install-mirror",
    "pyodide-install-mirror",
    "python-downloads-json-url",
    "publish-url",
    "trusted-publishing",
    "check-url",
    "add-bounds",
    "audit",
    "cache-keys",
    "exclude-dependencies",
    "build-constraint-dependencies",
    "environments",
    "required-environments",
    "minimum-libc-version",
    "conflicts",
    "managed",
    "package",
    "default-groups",
    "dependency-groups",
    "dev-dependencies",
    "build-backend",
    // Settings for `uv pip` commands, which do not lock projects.
    "pip",
];

/// The uv settings the Count reads, for the test against uv's own list.
pub const UV_READ: &[&str] = &[
    "index",
    "index-url",
    "extra-index-url",
    "find-links",
    "no-index",
    "no-sources",
    "no-sources-package",
    "torch-backend",
    "sources",
    "override-dependencies",
    "constraint-dependencies",
    "workspace",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields that are never read exist so their keys are accepted"
)]
struct UvIndex {
    name: Option<String>,
    url: String,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    explicit: bool,
    #[serde(default)]
    format: UvFormat,
    authenticate: Option<Value>,
    ignore_error_codes: Option<Value>,
    cache_control: Option<Value>,
    publish_url: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UvFormat {
    #[default]
    Simple,
    Flat,
}

/// A uv source. Its keys are uv 0.13.6's, checked against uv's own list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields that are never read exist so their keys are accepted"
)]
struct UvSource {
    index: Option<String>,
    git: Option<Value>,
    url: Option<Value>,
    path: Option<Value>,
    workspace: Option<Value>,
    subdirectory: Option<Value>,
    rev: Option<Value>,
    tag: Option<Value>,
    branch: Option<Value>,
    lfs: Option<Value>,
    editable: Option<Value>,
    package: Option<Value>,
    marker: Option<Value>,
    extra: Option<Value>,
    group: Option<Value>,
}

/// The keys of a uv source, for the test against uv's own list.
pub const UV_SOURCE_KEYS: &[&str] = &[
    "index",
    "git",
    "url",
    "path",
    "workspace",
    "subdirectory",
    "rev",
    "tag",
    "branch",
    "lfs",
    "editable",
    "package",
    "marker",
    "extra",
    "group",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UvWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

/// Poetry's table; keys other than these do not name sources.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Poetry {
    #[serde(default)]
    source: Vec<PoetrySource>,
    #[serde(default)]
    dependencies: BTreeMap<String, PoetryDependency>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, PoetryDependency>,
    #[serde(default)]
    group: BTreeMap<String, PoetryGroup>,
}

/// A Poetry source, with Poetry 2.5.1's keys (checked against its schema).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields that are never read exist so their keys are accepted"
)]
struct PoetrySource {
    name: String,
    url: Option<String>,
    priority: Option<PoetryPriority>,
    links: Option<bool>,
    indexed: Option<bool>,
}

/// The keys of a Poetry source, for the test against Poetry's schema.
pub const POETRY_SOURCE_KEYS: &[&str] = &["name", "url", "priority", "links", "indexed"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PoetryPriority {
    Primary,
    Supplemental,
    Explicit,
}

#[derive(Debug, Deserialize)]
struct PoetryGroup {
    #[serde(default)]
    dependencies: BTreeMap<String, PoetryDependency>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(
    dead_code,
    reason = "fields that are never read exist so their keys are accepted"
)]
enum PoetryDependency {
    Version(String),
    One(Box<PoetrySpec>),
    Many(Vec<PoetrySpec>),
}

/// A Poetry dependency table, with every key Poetry 2.5.1's schema allows.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(
    dead_code,
    reason = "fields that are never read exist so their keys are accepted"
)]
struct PoetrySpec {
    source: Option<String>,
    version: Option<Value>,
    python: Option<Value>,
    platform: Option<Value>,
    markers: Option<Value>,
    optional: Option<Value>,
    extras: Option<Value>,
    allow_prereleases: Option<Value>,
    allows_prereleases: Option<Value>,
    develop: Option<Value>,
    git: Option<Value>,
    branch: Option<Value>,
    rev: Option<Value>,
    tag: Option<Value>,
    subdirectory: Option<Value>,
    path: Option<Value>,
    file: Option<Value>,
    url: Option<Value>,
}

/// The keys of a Poetry dependency table, for the test against its schema.
pub const POETRY_DEPENDENCY_KEYS: &[&str] = &[
    "source",
    "version",
    "python",
    "platform",
    "markers",
    "optional",
    "extras",
    "allow-prereleases",
    "allows-prereleases",
    "develop",
    "git",
    "branch",
    "rev",
    "tag",
    "subdirectory",
    "path",
    "file",
    "url",
];

/// The tools' tables of a `pyproject.toml`.
#[derive(Debug, Default, Deserialize)]
struct Tools {
    uv: Option<Strict<UvSettings>>,
    poetry: Option<Strict<Poetry>>,
    pdm: Option<Value>,
    rye: Option<Value>,
    pixi: Option<Value>,
    hatch: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct PyprojectFile {
    #[serde(default)]
    tool: Strict<Tools>,
}

impl Default for Strict<Tools> {
    fn default() -> Self {
        Self(Some(Tools::default()))
    }
}

/// A configuration file: read, or present but not understood.
enum Loaded<T> {
    Read(T),
    Unreadable,
}

fn load<T: DeserializeOwned>(files: &Files, path: &str) -> Option<Loaded<T>> {
    let text = files.get(path)?;
    let read = toml_edit::de::from_str::<Value>(text)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok());
    Some(read.map_or(Loaded::Unreadable, Loaded::Read))
}

fn join(dir: &str, leaf: &str) -> String {
    if dir.is_empty() {
        leaf.to_owned()
    } else {
        format!("{dir}/{leaf}")
    }
}

/// The directory of `path` and those above it, nearest first.
fn ancestors(path: &str) -> Vec<&str> {
    let mut dirs = vec![path.rsplit_once('/').map_or("", |(dir, _)| dir)];
    while let Some(&dir) = dirs.last()
        && !dir.is_empty()
    {
        dirs.push(dir.rsplit_once('/').map_or("", |(parent, _)| parent));
    }
    dirs
}

fn unreadable(what: impl std::fmt::Display) -> Named {
    Named::Unusable(format!("{what} the Count cannot read"))
}

fn find_links(url: &str) -> Named {
    Named::Unusable(format!("--find-links {}", crate::redacted(url)))
}

/// Where the packages of one `pyproject.toml` come from, and the
/// directories whose lockfiles resolve them.
pub(crate) struct Discovery {
    pub indexes: Indexes,
    pub lock_dirs: Vec<String>,
}

/// The index settings that apply to the `pyproject.toml` at `path`.
pub(crate) fn discover(path: &str, files: &Files) -> Discovery {
    let dirs = ancestors(path);
    let mut indexes = Indexes::default();
    let mut pyprojects: Vec<(&str, Loaded<PyprojectFile>)> = Vec::new();
    let mut uv_tomls: Vec<(&str, Loaded<Strict<UvSettings>>)> = Vec::new();
    for dir in &dirs {
        if let Some(loaded) = load(files, &join(dir, "pyproject.toml")) {
            pyprojects.push((dir, loaded));
        }
        if let Some(loaded) = load(files, &join(dir, "uv.toml")) {
            uv_tomls.push((dir, loaded));
        }
    }
    // A configuration file at or above the project that cannot be read
    // could name any index.
    for (dir, loaded) in &pyprojects {
        if matches!(loaded, Loaded::Unreadable)
            || matches!(loaded, Loaded::Read(file) if file.tool.0.is_none())
        {
            indexes.extras.push(unreadable(join(dir, "pyproject.toml")));
        }
    }
    for (dir, loaded) in &uv_tomls {
        if matches!(loaded, Loaded::Unreadable | Loaded::Read(Strict(None))) {
            indexes.extras.push(unreadable(join(dir, "uv.toml")));
        }
    }
    let tools = |dir: &str| -> Option<&Tools> {
        pyprojects.iter().find_map(|(d, loaded)| match loaded {
            Loaded::Read(file) if *d == dir => file.tool.0.as_ref(),
            _ => None,
        })
    };
    // A uv table that cannot be read, here or in the workspace root.
    let uv = |dir: &str| -> Option<Option<&UvSettings>> {
        tools(dir)?.uv.as_ref().map(|strict| strict.0.as_ref())
    };
    let project_dir = dirs[0];
    // The nearest uv workspace root takes in the projects its `members`
    // name and `exclude` leaves out; one it does not take in stands alone.
    let root = dirs
        .iter()
        .find_map(|dir| match uv(dir) {
            Some(Some(settings)) => settings.workspace.as_ref().map(|w| (*dir, w)),
            _ => None,
        })
        .and_then(|(dir, workspace)| {
            if dir == project_dir {
                return Some(dir);
            }
            let member = project_dir
                .strip_prefix(dir)
                .map_or(project_dir, |rest| rest.trim_start_matches('/'));
            match (globs(&workspace.members), globs(&workspace.exclude)) {
                (Some(members), Some(exclude)) => (!workspace.members.is_empty()
                    && members.matches(member)
                    && (workspace.exclude.is_empty() || !exclude.matches(member)))
                .then_some(dir),
                _ => {
                    indexes.extras.push(unreadable(format!(
                        "workspace globs in {}",
                        join(dir, "pyproject.toml")
                    )));
                    None
                }
            }
        });
    for dir in dirs.iter().filter(|dir| Some(**dir) != root) {
        if let Some(None) = uv(dir) {
            indexes.extras.push(unreadable(format!(
                "[tool.uv] in {}",
                join(dir, "pyproject.toml")
            )));
        }
    }
    // uv reads its settings from the nearest `uv.toml`, or `pyproject.toml`
    // with `[tool.uv]`, at or above the project; in a workspace, its root's.
    let search: Vec<&str> = root.map_or_else(|| dirs.clone(), |root| vec![root]);
    let settings: Option<(String, Option<&UvSettings>)> = search.iter().find_map(|dir| {
        if let Some((_, loaded)) = uv_tomls.iter().find(|(d, _)| d == dir) {
            let read = match loaded {
                Loaded::Read(Strict(Some(settings))) => Some(settings),
                _ => None,
            };
            return Some((join(dir, "uv.toml"), read));
        }
        uv(dir).map(|read| (join(dir, "pyproject.toml"), read))
    });
    let mut names = BTreeMap::<String, Named>::new();
    if let Some((file, Some(settings))) = &settings {
        if file.ends_with("uv.toml")
            && (!settings.sources.is_empty() || settings.workspace.is_some())
        {
            indexes
                .extras
                .push(unreadable(format!("sources or a workspace in {file}")));
        }
        uv_settings(settings, &mut indexes);
    }
    // Sources and the index names they pin to come from the project and its
    // workspace root, the project's winning. uv does not resolve a pin to an
    // index only a `uv.toml` defines, so neither does the Count.
    let mut projects: Vec<&UvSettings> = Vec::new();
    if let Some(Some(project)) = uv(project_dir) {
        projects.push(project);
    }
    if let Some(root) = root
        && root != project_dir
        && let Some(Some(settings)) = uv(root)
    {
        projects.push(settings);
    }
    for project in projects.iter().rev() {
        for index in &project.index {
            if let Some(name) = &index.name {
                names.insert(name.clone(), uv_index(index));
            }
        }
    }
    let no_sources = settings
        .as_ref()
        .and_then(|(_, s)| *s)
        .map_or((false, &[][..]), |s| {
            (s.no_sources, &s.no_sources_package[..])
        });
    for project in projects.iter().rev() {
        for requirement in project
            .override_dependencies
            .iter()
            .chain(&project.constraint_dependencies)
        {
            // An override or constraint naming a URL takes its package from it.
            if let Some(name) = direct_reference(requirement) {
                indexes.pinned.insert(name, Origin::Other);
            }
        }
        if no_sources.0 {
            continue;
        }
        for (package, sources) in &project.sources {
            let package = super::normalise(package);
            if no_sources.1.iter().any(|p| super::normalise(p) == package) {
                continue;
            }
            let origin = Origin::of(sources.items().iter().map(|source| uv_pin(source, &names)));
            if let Some(origin) = origin {
                indexes.pinned.insert(package, origin);
            }
        }
    }
    let project_tools = tools(project_dir);
    match project_tools.and_then(|t| t.poetry.as_ref()) {
        Some(Strict(Some(poetry))) => poetry_settings(poetry, &mut indexes),
        Some(Strict(None)) => indexes
            .extras
            .push(unreadable(format!("[tool.poetry] in {path}"))),
        None => {}
    }
    if let Some(tools) = project_tools {
        unverified(tools, &mut indexes);
    }
    let mut lock_dirs = vec![project_dir.to_owned()];
    if let Some(root) = root
        && root != project_dir
    {
        lock_dirs.push(root.to_owned());
    }
    Discovery { indexes, lock_dirs }
}

/// Workspace globs as uv joins them to the root: `./packages/*/` is
/// `packages/*`. None when one does not compile.
fn globs(values: &[String]) -> Option<Patterns> {
    let values: Vec<String> = values
        .iter()
        .map(|glob| {
            let mut glob = glob.trim_end_matches('/');
            while let Some(rest) = glob.strip_prefix("./") {
                glob = rest;
            }
            glob.to_owned()
        })
        .collect();
    Patterns::compile(&values).ok()
}

/// The package a PEP 508 requirement takes from a URL, if it does.
fn direct_reference(requirement: &str) -> Option<String> {
    let name: String = requirement
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let rest = requirement.trim()[name.len()..].trim_start();
    let rest = match rest.strip_prefix('[') {
        Some(extras) => extras.split_once(']')?.1.trim_start(),
        None => rest,
    };
    (!name.is_empty() && rest.starts_with('@')).then(|| super::normalise(&name))
}

fn uv_index(index: &UvIndex) -> Named {
    match index.format {
        UvFormat::Simple => Named::parse(&index.url),
        UvFormat::Flat => find_links(&index.url),
    }
}

/// The default and searched indexes of one uv settings table.
fn uv_settings(settings: &UvSettings, indexes: &mut Indexes) {
    for key in settings.rest.keys() {
        if !UV_IRRELEVANT.contains(&key.as_str()) {
            indexes
                .extras
                .push(unreadable(format!("uv setting {}", shown(key))));
        }
    }
    if settings.torch_backend.is_some() {
        indexes.extras.push(Named::Unusable(
            "uv torch-backend, which routes packages to PyTorch's indexes".into(),
        ));
    }
    let mut defaults = Vec::new();
    for index in &settings.index {
        let named = uv_index(index);
        if index.default {
            defaults.push(named);
        } else if !index.explicit {
            indexes.extras.push(named);
        }
    }
    defaults.extend(settings.index_url.as_deref().map(Named::parse));
    indexes
        .extras
        .extend(settings.extra_index_url.iter().map(|u| Named::parse(u)));
    indexes
        .extras
        .extend(settings.find_links.iter().map(|u| find_links(u)));
    let mut defaults = defaults.into_iter();
    indexes.default = defaults.next();
    indexes.extras.extend(defaults);
    if settings.no_index {
        let mut only = vec!["--no-index".to_owned()];
        only.extend(
            settings
                .find_links
                .iter()
                .map(|u| format!("--find-links {}", crate::redacted(u))),
        );
        indexes.default = Some(Named::Unusable(only.join(" ")));
        indexes
            .extras
            .retain(|named| !matches!(named, Named::Url(_) | Named::Public));
    }
}

/// Where one uv source pins its package.
fn uv_pin(source: &UvSource, names: &BTreeMap<String, Named>) -> Origin {
    let pinned = if let Some(name) = &source.index {
        Origin::Index(
            names
                .get(name)
                .cloned()
                .unwrap_or_else(|| Named::Unusable(format!("index {}", shown(name)))),
        )
    } else if source.git.is_some()
        || source.url.is_some()
        || source.path.is_some()
        || source.workspace.is_some()
    {
        return Origin::Other;
    } else {
        Origin::Index(unreadable("a uv source"))
    };
    conditional(
        pinned,
        source.marker.is_some() || source.extra.is_some() || source.group.is_some(),
    )
}

/// A pin that holds only under some markers, extras, groups or Pythons
/// leaves the package to the project's indexes everywhere else.
fn conditional(pinned: Origin, conditional: bool) -> Origin {
    match pinned {
        Origin::Index(named) if conditional => Origin::Ambiguous(vec![
            named,
            Named::Unusable("its indexes where the pin does not hold".into()),
        ]),
        pinned => pinned,
    }
}

/// Poetry's sources, by priority, and the packages pinned to them.
fn poetry_settings(poetry: &Poetry, indexes: &mut Indexes) {
    let mut names = BTreeMap::<String, Named>::new();
    // Poetry knows PyPI by that name, whatever its case.
    names.insert("pypi".into(), Named::Public);
    let mut primaries = Vec::new();
    for source in &poetry.source {
        let name = source.name.to_ascii_lowercase();
        let named = match (&source.url, source.links) {
            (Some(url), Some(true)) => find_links(url),
            (Some(url), _) => Named::parse(url),
            (None, _) if name == "pypi" => Named::Public,
            (None, _) => unreadable(format!(
                "Poetry source {} without a URL",
                shown(&source.name)
            )),
        };
        names.insert(name, named.clone());
        match source.priority.unwrap_or(PoetryPriority::Primary) {
            PoetryPriority::Primary => primaries.push(named),
            PoetryPriority::Supplemental => indexes.extras.push(named),
            PoetryPriority::Explicit => {}
        }
    }
    // With no primary source, PyPI is; primary sources replace it, and more
    // than one is ambiguous.
    let mut primaries = primaries.into_iter();
    if let Some(first) = primaries.next() {
        indexes.default = Some(first);
    }
    indexes.extras.extend(primaries);
    let groups = poetry.group.values().map(|g| &g.dependencies);
    for dependencies in [&poetry.dependencies, &poetry.dev_dependencies]
        .into_iter()
        .chain(groups)
    {
        for (package, dependency) in dependencies {
            let specs = match dependency {
                PoetryDependency::Version(_) => continue,
                PoetryDependency::One(spec) => std::slice::from_ref(spec.as_ref()),
                PoetryDependency::Many(specs) => specs.as_slice(),
            };
            let origin = Origin::of(specs.iter().filter_map(|spec| poetry_pin(spec, &names)));
            if let Some(origin) = origin {
                indexes.pinned.insert(super::normalise(package), origin);
            }
        }
    }
}

fn poetry_pin(spec: &PoetrySpec, names: &BTreeMap<String, Named>) -> Option<Origin> {
    let pinned = if let Some(name) = &spec.source {
        Origin::Index(
            names
                .get(&name.to_ascii_lowercase())
                .cloned()
                .unwrap_or_else(|| Named::Unusable(format!("index {}", shown(name)))),
        )
    } else if spec.git.is_some() || spec.path.is_some() || spec.file.is_some() || spec.url.is_some()
    {
        return Some(Origin::Other);
    } else {
        return None;
    };
    Some(conditional(
        pinned,
        spec.python.is_some() || spec.platform.is_some() || spec.markers.is_some(),
    ))
}

/// Settings of tools the Count cannot check against the tool itself, which
/// could name an index: their packages' lookups fail.
fn unverified(tools: &Tools, indexes: &mut Indexes) {
    let has = |value: &Option<Value>, pointer: &str| {
        value
            .as_ref()
            .and_then(|v| v.pointer(pointer))
            .is_some_and(|v| !v.is_null())
    };
    let pixi_index = tools
        .pixi
        .as_ref()
        .and_then(|p| p.pointer("/pypi-dependencies"))
        .and_then(Value::as_object)
        .is_some_and(|deps| deps.values().any(|d| d.get("index").is_some()));
    let hatch_env = tools
        .hatch
        .as_ref()
        .and_then(|h| h.pointer("/envs"))
        .and_then(Value::as_object)
        .is_some_and(|envs| {
            envs.values().any(|env| {
                env.get("env-vars")
                    .and_then(Value::as_object)
                    .is_some_and(|vars| {
                        vars.keys()
                            .any(|k| k.starts_with("PIP_") || k.starts_with("UV_"))
                    })
            })
        });
    for (present, what) in [
        (has(&tools.pdm, "/source"), "PDM sources"),
        (has(&tools.pdm, "/resolution/overrides"), "PDM overrides"),
        (has(&tools.rye, "/sources"), "Rye sources"),
        (
            has(&tools.pixi, "/pypi-options") || pixi_index,
            "Pixi PyPI options",
        ),
        (hatch_env, "Hatch environment PIP_ or UV_ variables"),
    ] {
        if present {
            indexes.extras.push(Named::Unusable(format!(
                "{what}, which the Count does not read"
            )));
        }
    }
}
