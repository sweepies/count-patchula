//! Which of a project's packages come from an index the Count cannot pick
//! for itself. The tools' lockfiles decide where a package comes from; these
//! settings only say whether the project configures any index at all, in
//! which case a package no lockfile records is not assumed to be PyPI's.
//!
//! uv and Poetry settings are read into types that describe every key the
//! Count knows and reject the rest, so `[[x]]` and `x = [{...}]` read alike.
//! Every key uv 0.13.6 accepts is either one that configures where packages
//! come from or listed as irrelevant; a test with the real uv checks the
//! lists against uv's own. A section that does not fit its type, a file that
//! does not parse, and PDM, Rye, Pixi or Hatch index settings, which the
//! Count cannot check against those tools, make every package's lookup fail.
//!
//! Errors are never repeated in reports: serde's messages can quote values.
use crate::origins::{Named, Origin, Origins, shown};
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
    torch_backend: Option<Value>,
    #[serde(default)]
    sources: BTreeMap<String, OneOrMany<UvSource>>,
    #[serde(default)]
    override_dependencies: Vec<String>,
    #[serde(default)]
    constraint_dependencies: Vec<String>,
    workspace: Option<UvWorkspace>,
    pip: Option<UvPip>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

/// Every other setting uv 0.13.6 accepts in `uv.toml` and `[tool.uv]`,
/// none of which moves a package to another index.
pub const UV_IRRELEVANT: &[&str] = &[
    // Only matters where sources exist, which configure the project anyway.
    "no-sources",
    "no-sources-package",
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
];

/// The uv settings the Count reads, for the test against uv's own list.
pub const UV_READ: &[&str] = &[
    "index",
    "index-url",
    "extra-index-url",
    "find-links",
    "no-index",
    "torch-backend",
    "sources",
    "override-dependencies",
    "constraint-dependencies",
    "workspace",
    "pip",
];

/// `uv pip` settings, which `uv pip install .` and `uv pip compile
/// pyproject.toml` read for a project's own dependencies: their indexes are
/// candidates too. Every other key is in [`UV_PIP_IRRELEVANT`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct UvPip {
    index_url: Option<String>,
    #[serde(default)]
    extra_index_url: Vec<String>,
    #[serde(default)]
    find_links: Vec<String>,
    #[serde(default)]
    no_index: bool,
    torch_backend: Option<Value>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

/// The `[pip]` settings the Count reads, for the test against uv's list.
pub const UV_PIP_READ: &[&str] = &[
    "index-url",
    "extra-index-url",
    "find-links",
    "no-index",
    "torch-backend",
];

/// Every other `[pip]` setting uv 0.13.6 accepts, none of which moves a
/// package to another index.
pub const UV_PIP_IRRELEVANT: &[&str] = &[
    // Only matters where sources exist, which configure the project anyway.
    "no-sources",
    "no-sources-package",
    "python",
    "system",
    "break-system-packages",
    "target",
    "prefix",
    "index-strategy",
    "keyring-provider",
    "no-build",
    "no-binary",
    "only-binary",
    "no-build-isolation",
    "require-build-hashes",
    "no-build-isolation-package",
    "extra-build-dependencies",
    "extra-build-variables",
    "strict",
    "extra",
    "all-extras",
    "no-extra",
    "no-deps",
    "group",
    "allow-empty-requirements",
    "resolution",
    "prerelease",
    "fork-strategy",
    "dependency-metadata",
    "output-file",
    "no-strip-extras",
    "no-strip-markers",
    "no-annotate",
    "no-header",
    "custom-compile-command",
    "generate-hashes",
    "config-settings",
    "config-settings-package",
    "python-version",
    "python-platform",
    "universal",
    "exclude-newer",
    "exclude-newer-package",
    "no-emit-package",
    "emit-index-url",
    "emit-find-links",
    "emit-build-options",
    "emit-marker-expression",
    "emit-index-annotation",
    "annotation-style",
    "link-mode",
    "compile-bytecode",
    "require-hashes",
    "verify-hashes",
    "upgrade",
    "upgrade-package",
    "reinstall",
    "reinstall-package",
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

/// Where the packages of one `pyproject.toml` come from, and the lockfile
/// whose versions are its current ones.
pub(crate) struct Discovery {
    pub origins: Origins,
    pub resolved_lock: Option<String>,
}

/// What applies to the `pyproject.toml` at `path`: the indexes configured at
/// or above it, the lockfiles uv and Poetry would read for it, and anything
/// the Count cannot read.
pub(crate) fn discover(path: &str, files: &Files) -> Discovery {
    let dirs = ancestors(path);
    let mut origins = Origins::default();
    let mut pyprojects: Vec<(&str, Loaded<PyprojectFile>)> = Vec::new();
    for dir in &dirs {
        let file = join(dir, "pyproject.toml");
        if let Some(loaded) = load(files, &file) {
            match &loaded {
                Loaded::Read(PyprojectFile {
                    tool: Strict(Some(tools)),
                }) => {
                    project_settings(tools, &file, &mut origins);
                }
                _ => origins.uncertain.push(unreadable(&file)),
            }
            pyprojects.push((dir, loaded));
        }
        let file = join(dir, "uv.toml");
        match load::<Strict<UvSettings>>(files, &file) {
            Some(Loaded::Read(Strict(Some(settings)))) => {
                uv_settings(&settings, &file, &mut origins);
            }
            Some(_) => origins.uncertain.push(unreadable(&file)),
            None => {}
        }
        for (leaf, names_index) in [
            ("pdm.toml", pdm_names_index as fn(&Value) -> bool),
            ("pixi.toml", pixi_names_index),
            ("hatch.toml", hatch_sets_installer),
        ] {
            let file = join(dir, leaf);
            match load::<Value>(files, &file) {
                Some(Loaded::Read(value)) if !names_index(&value) => {}
                Some(_) => origins.uncertain.push(Named::Unusable(format!(
                    "index settings in {file}, which the Count does not read"
                ))),
                None => {}
            }
        }
    }
    let project_dir = dirs[0];
    // uv locks a workspace at its root: the nearest one whose `members` name
    // the project and whose `exclude` leaves it in. A member's own `uv.lock`
    // is left over from before it joined.
    let workspace = |dir: &str| -> Option<&UvWorkspace> {
        pyprojects.iter().find_map(|(d, loaded)| match loaded {
            Loaded::Read(PyprojectFile {
                tool: Strict(Some(tools)),
            }) if *d == dir => tools.uv.as_ref()?.0.as_ref()?.workspace.as_ref(),
            _ => None,
        })
    };
    let root = dirs
        .iter()
        .find_map(|dir| Some((*dir, workspace(dir)?)))
        .and_then(|(dir, workspace)| {
            if dir == project_dir {
                return Some(dir);
            }
            let member = project_dir
                .strip_prefix(dir)
                .map_or(project_dir, |rest| rest.trim_start_matches('/'));
            match membership(workspace, member) {
                Some(true) => Some(dir),
                Some(false) => None,
                None => {
                    origins.uncertain.push(unreadable(format!(
                        "workspace globs in {}",
                        join(dir, "pyproject.toml")
                    )));
                    None
                }
            }
        });
    let uv_lock = join(root.unwrap_or(project_dir), "uv.lock");
    let poetry_lock = join(project_dir, "poetry.lock");
    for (lock, poetry) in [(&uv_lock, false), (&poetry_lock, true)] {
        if let Some(text) = files.get(lock) {
            match locked(text, poetry) {
                Some(recorded) => origins.locks.push(recorded),
                None => origins.uncertain.push(unreadable(lock)),
            }
        }
    }
    let resolved_lock = [&uv_lock, &poetry_lock]
        .into_iter()
        .find(|lock| files.contains_key(*lock))
        .cloned();
    Discovery {
        origins,
        resolved_lock,
    }
}

/// The index settings of one `pyproject.toml`'s tool tables.
fn project_settings(tools: &Tools, file: &str, origins: &mut Origins) {
    match tools.uv.as_ref() {
        Some(Strict(Some(settings))) => uv_settings(settings, file, origins),
        Some(Strict(None)) => origins
            .uncertain
            .push(unreadable(format!("[tool.uv] in {file}"))),
        None => {}
    }
    match tools.poetry.as_ref() {
        Some(Strict(Some(poetry))) => poetry_settings(poetry, origins),
        Some(Strict(None)) => origins
            .uncertain
            .push(unreadable(format!("[tool.poetry] in {file}"))),
        None => {}
    }
    unverified(tools, &mut origins.uncertain);
}

/// The indexes and other sources one uv settings table configures, `[pip]`
/// included, since `uv pip` reads it for the project too.
fn uv_settings(settings: &UvSettings, file: &str, origins: &mut Origins) {
    for key in settings.rest.keys() {
        if !UV_IRRELEVANT.contains(&key.as_str()) {
            origins
                .uncertain
                .push(unreadable(format!("uv setting {} in {file}", shown(key))));
        }
    }
    let configured = &mut origins.configured;
    configured.extend(settings.index.iter().map(uv_index));
    configured.extend(settings.index_url.as_deref().map(super::named));
    configured.extend(settings.extra_index_url.iter().map(|u| super::named(u)));
    configured.extend(settings.find_links.iter().map(|u| find_links(u)));
    if settings.no_index {
        configured.push(Named::Unusable("uv --no-index".into()));
    }
    if settings.torch_backend.is_some() {
        configured.push(Named::Unusable(
            "uv torch-backend, which routes packages to PyTorch's indexes".into(),
        ));
    }
    // A source that names git, a path, a URL or a workspace member says the
    // package comes from no index; one that names an index leaves it to the
    // lockfile.
    for (package, sources) in &settings.sources {
        let outside = sources.items().iter().all(|source| {
            source.index.is_none()
                && (source.git.is_some()
                    || source.url.is_some()
                    || source.path.is_some()
                    || source.workspace.is_some())
        });
        if outside {
            origins.outside.insert(super::normalise(package));
        } else {
            origins
                .configured
                .push(Named::Unusable(format!("[tool.uv.sources] in {file}")));
        }
    }
    let configured = &mut origins.configured;
    if settings
        .override_dependencies
        .iter()
        .chain(&settings.constraint_dependencies)
        .any(|requirement| direct_reference(requirement).is_some())
    {
        configured.push(Named::Unusable(format!(
            "an override naming a URL in {file}"
        )));
    }
    if let Some(pip) = &settings.pip {
        for key in pip.rest.keys() {
            if !UV_PIP_IRRELEVANT.contains(&key.as_str()) {
                origins.uncertain.push(unreadable(format!(
                    "uv pip setting {} in {file}",
                    shown(key)
                )));
            }
        }
        let configured = &mut origins.configured;
        configured.extend(pip.index_url.as_deref().map(super::named));
        configured.extend(pip.extra_index_url.iter().map(|u| super::named(u)));
        configured.extend(pip.find_links.iter().map(|u| find_links(u)));
        if pip.no_index {
            configured.push(Named::Unusable("uv pip --no-index".into()));
        }
        if pip.torch_backend.is_some() {
            configured.push(Named::Unusable("uv pip torch-backend".into()));
        }
    }
}

/// The sources Poetry's table configures: its sources, and dependencies
/// pinned to one or taken from git, a path or a URL.
fn poetry_settings(poetry: &Poetry, origins: &mut Origins) {
    for source in &poetry.source {
        origins.configured.push(match (&source.url, source.links) {
            (Some(url), Some(true)) => find_links(url),
            (Some(url), _) => super::named(url),
            (None, _) if source.name.eq_ignore_ascii_case("pypi") => Named::Public,
            (None, _) => Named::Unusable(format!(
                "Poetry source {} without a URL",
                shown(&source.name)
            )),
        });
    }
    let groups = poetry.group.values().map(|g| &g.dependencies);
    let pinned = [&poetry.dependencies, &poetry.dev_dependencies]
        .into_iter()
        .chain(groups)
        .flat_map(|dependencies| dependencies.values())
        .flat_map(|dependency| match dependency {
            PoetryDependency::Version(_) => &[][..],
            PoetryDependency::One(spec) => std::slice::from_ref(spec.as_ref()),
            PoetryDependency::Many(specs) => specs.as_slice(),
        })
        .any(|spec| {
            spec.source
                .as_deref()
                .is_some_and(|name| !name.eq_ignore_ascii_case("pypi"))
                || spec.git.is_some()
                || spec.path.is_some()
                || spec.file.is_some()
                || spec.url.is_some()
        });
    if pinned {
        origins.configured.push(Named::Unusable(
            "Poetry dependencies pinned to a source".into(),
        ));
    }
}

/// What a lockfile records of each package, by normalised name; none when
/// it does not parse, in which case it could record anything.
fn locked(text: &str, poetry: bool) -> Option<BTreeMap<String, Origin>> {
    let doc = text.parse::<toml_edit::Document<String>>().ok()?;
    let mut origins = BTreeMap::<String, Vec<Origin>>::new();
    for entry in doc
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .into_iter()
        .flatten()
    {
        let Some(name) = entry.get("name").and_then(toml_edit::Item::as_str) else {
            continue;
        };
        origins
            .entry(super::normalise(name))
            .or_default()
            .push(lock_entry(entry, poetry));
    }
    Some(
        origins
            .into_iter()
            .filter_map(|(name, entries)| Some((name, Origin::of(entries)?)))
            .collect(),
    )
}

/// Where one lockfile entry says its package came from. Entries for one
/// package that disagree, as a forked uv resolution can record, are
/// ambiguous.
fn lock_entry(entry: &toml_edit::Table, poetry: bool) -> Origin {
    let Some(source) = entry.get("source") else {
        // Poetry records no source for PyPI; uv records one for every package.
        return Origin::Index(if poetry {
            Named::Public
        } else {
            Named::Unusable("a locked package without a source".into())
        });
    };
    if let Some(registry) = source.get("registry").and_then(toml_edit::Item::as_str) {
        return Origin::Index(super::named(registry));
    }
    match source.get("type").and_then(toml_edit::Item::as_str) {
        // A source the lockfile names without its URL is not PyPI's.
        Some("legacy" | "pypi") => source
            .get("url")
            .and_then(toml_edit::Item::as_str)
            .map_or_else(
                || Origin::Index(Named::Unusable("a locked source without a URL".into())),
                |url| Origin::Index(super::named(url)),
            ),
        _ => Origin::Other,
    }
}

/// Whether `member` (relative to the root) is in the workspace: none when
/// the globs do not compile or use syntax uv may read differently, braces,
/// or a `*` whose match depends on crossing `/`.
fn membership(workspace: &UvWorkspace, member: &str) -> Option<bool> {
    let all = workspace.members.iter().chain(&workspace.exclude);
    if all.clone().any(|glob| glob.contains(['{', '}'])) {
        return None;
    }
    let matches = |values: &[String], literal: bool| -> Option<bool> {
        Some(!values.is_empty() && globs(values, literal)?.is_match(member))
    };
    let member_in = matches(&workspace.members, true)?;
    let excluded = matches(&workspace.exclude, true)?;
    if member_in != matches(&workspace.members, false)?
        || excluded != matches(&workspace.exclude, false)?
    {
        return None;
    }
    Some(member_in && !excluded)
}

/// Workspace globs as uv joins them to the root, `./packages/*/` being
/// `packages/*`. None when one does not compile.
fn globs(values: &[String], literal_separator: bool) -> Option<globset::GlobSet> {
    let mut set = globset::GlobSetBuilder::new();
    for glob in values {
        let mut glob = glob.trim_end_matches('/');
        while let Some(rest) = glob.strip_prefix("./") {
            glob = rest;
        }
        set.add(
            globset::GlobBuilder::new(glob)
                .literal_separator(literal_separator)
                .build()
                .ok()?,
        );
    }
    set.build().ok()
}

/// Whether `text`, a `pyproject.toml`, could move a package off PyPI: an
/// index configured or anything the Count cannot read there.
pub(crate) fn private_index(text: &str) -> bool {
    let mut origins = Origins::default();
    let read = toml_edit::de::from_str::<Value>(text)
        .ok()
        .and_then(|value| serde_json::from_value::<PyprojectFile>(value).ok());
    match read {
        Some(PyprojectFile {
            tool: Strict(Some(tools)),
        }) => {
            project_settings(&tools, "pyproject.toml", &mut origins);
            !origins.public_only()
        }
        _ => true,
    }
}

/// Whether `text`, a `uv.toml`, could move a package off PyPI.
pub(crate) fn uv_private_index(text: &str) -> bool {
    let mut origins = Origins::default();
    let read = toml_edit::de::from_str::<Value>(text)
        .ok()
        .and_then(|value| serde_json::from_value::<Strict<UvSettings>>(value).ok());
    match read {
        Some(Strict(Some(settings))) => {
            uv_settings(&settings, "uv.toml", &mut origins);
            !origins.public_only()
        }
        _ => true,
    }
}

/// Whether `text`, a `pip.conf` or `pip.ini`, names an index besides PyPI
/// or says something the Count does not read. pip's parser — configparser
/// — takes `=` and `:` alike and options case-insensitively with `-` or
/// `_` alike, and an indented line continues the previous key's value, so
/// every line of an `index-url` or `extra-index-url` value is checked; a
/// continuation with no key above it, a section header without its `]`,
/// or a line with neither delimiter is one pip would reject — private,
/// since the Count cannot say what it meant. Whitespace inside a value
/// splits it into several URLs to pip, so it is private too.
pub(crate) fn pip_private_index(text: &str) -> bool {
    // Whether indented lines continue an index key's value.
    let mut index_value = false;
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with(|c: char| c.is_whitespace()) {
            // configparser joins it to the previous key's value.
            if !index_value {
                return true;
            }
            let value = line.trim();
            if value.contains(char::is_whitespace) || !matches!(super::named(value), Named::Public)
            {
                return true;
            }
            continue;
        }
        index_value = false;
        if line.starts_with('[') {
            if !line.trim_end().ends_with(']') {
                return true;
            }
            continue;
        }
        let Some(at) = line.find(['=', ':']) else {
            return true;
        };
        let key = line[..at].trim().to_ascii_lowercase().replace('_', "-");
        match key.as_str() {
            "index-url" | "extra-index-url" => {
                index_value = true;
                let value = line[at + 1..].trim();
                if value.contains(char::is_whitespace)
                    || !matches!(super::named(value), Named::Public)
                {
                    return true;
                }
            }
            "find-links" | "no-index" => return true,
            _ => {}
        }
    }
    false
}

/// The package a PEP 508 requirement takes from a URL, if it does.
pub(crate) fn direct_reference(requirement: &str) -> Option<String> {
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
        UvFormat::Simple => super::named(&index.url),
        UvFormat::Flat => find_links(&index.url),
    }
}

/// Settings of tools the Count cannot check against the tool itself, which
/// could name an index: every package's lookup fails.
fn unverified(tools: &Tools, uncertain: &mut Vec<Named>) {
    for (present, what) in [
        (
            tools.pdm.as_ref().is_some_and(pdm_names_index),
            "PDM sources or overrides",
        ),
        (
            tools
                .rye
                .as_ref()
                .is_some_and(|rye| has_key(rye, &|key| key == "sources")),
            "Rye sources",
        ),
        (
            tools.pixi.as_ref().is_some_and(pixi_names_index),
            "Pixi PyPI options",
        ),
        (
            tools.hatch.as_ref().is_some_and(hatch_sets_installer),
            "Hatch installer settings",
        ),
    ] {
        if present {
            uncertain.push(Named::Unusable(format!(
                "{what}, which the Count does not read"
            )));
        }
    }
}

/// Whether `value` holds, at any depth, a key `wanted` accepts.
fn has_key(value: &Value, wanted: &dyn Fn(&str) -> bool) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| wanted(key) || has_key(value, wanted)),
        Value::Array(items) => items.iter().any(|item| has_key(item, wanted)),
        _ => false,
    }
}

/// Whether PDM settings, in `[tool.pdm]` or `pdm.toml`, name an index:
/// sources, `pypi` settings or resolution overrides anywhere.
fn pdm_names_index(pdm: &Value) -> bool {
    has_key(pdm, &|key| matches!(key, "source" | "pypi" | "overrides"))
}

/// Whether Pixi settings, in `[tool.pixi]` or `pixi.toml`, name a PyPI
/// index: options or a dependency's index in any feature or target.
fn pixi_names_index(pixi: &Value) -> bool {
    has_key(pixi, &|key| {
        matches!(
            key,
            "pypi-options" | "index-url" | "extra-index-urls" | "find-links" | "index"
        )
    })
}

/// Whether Hatch settings, in `[tool.hatch]` or `hatch.toml`, give the
/// installer flags or pip or uv variables, in any environment or override.
fn hatch_sets_installer(hatch: &Value) -> bool {
    fn installer_variable(value: &Value) -> bool {
        match value {
            Value::Object(map) => map.iter().any(|(key, value)| {
                key.starts_with("PIP_")
                    || key.starts_with("UV_")
                    || key == "pip-install-flags"
                    || (key == "key"
                        && value
                            .as_str()
                            .is_some_and(|v| v.starts_with("PIP_") || v.starts_with("UV_")))
                    || installer_variable(value)
            }),
            Value::Array(items) => items.iter().any(installer_variable),
            _ => false,
        }
    }
    installer_variable(hatch)
}
