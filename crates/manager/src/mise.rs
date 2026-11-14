use crate::roles::leaf;
use crate::*;
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};
use toml_edit::{Document, Value};

/// Backends that list versions only from options in the tool's table, which
/// the Count never passes to mise, so a lookup would always come back empty.
const UNLISTED: &[(&str, &str)] = &[
    (
        "http:",
        "mise's http backend lists versions only with version_list_url, which the Count does not pass",
    ),
    (
        "s3:",
        "mise's s3 backend lists versions only with version_list_url or version_prefix, which the Count does not pass",
    ),
];

/// Whether a directory walk for task files skips `below`, a path relative
/// to the walk's root, as configuration. mise's
/// `is_mise_config_file_in_task_include` (`src/config/mod.rs`) matches
/// `DEFAULT_CONFIG_FILENAMES` (see `config_root`) against the relative
/// path and the file name. The walk never enters a hidden folder, and
/// with no `MISE_ENV` that leaves `mise.toml` and `mise.local.toml` under
/// any folder, and `mise/config.toml`, `mise/config.local.toml`,
/// `mise/conf.d/*.toml` and `mise/conf.d/*/mise.toml` and
/// `mise.local.toml` at the root.
fn walk_config(below: &str) -> bool {
    matches!(leaf(below), "mise.toml" | "mise.local.toml")
        || matches!(
            below.split('/').collect::<Vec<_>>()[..],
            ["mise", "config.toml" | "config.local.toml"]
                | ["mise", "conf.d", _]
                | ["mise", "conf.d", _, "mise.toml" | "mise.local.toml"]
        ) && below.ends_with(".toml")
}

/// The TOML files in `paths` that `include` names for a config in `directory`.
/// mise accepts a file, a glob such as `tasks/*.toml`, or a directory, whose
/// TOML files at any depth are task files, and `.` is the repository root. A
/// directory walk skips hidden files and what mise reads as configuration
/// (`walk_config`). A path that leaves the repository names nothing.
pub fn included_files<'a>(
    directory: &str,
    include: &str,
    paths: impl Iterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut parts: Vec<&str> = directory.split('/').filter(|p| !p.is_empty()).collect();
    for component in Path::new(include).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str().unwrap_or_default()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Vec::new();
                }
            }
            Component::RootDir | Component::Prefix(_) => return Vec::new(),
        }
    }
    let pattern = parts.join("/");
    let below = if pattern.is_empty() {
        "**".to_owned()
    } else {
        format!("{pattern}/**")
    };
    let matcher = |glob: &str| {
        globset::GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
            .ok()
            .map(|glob| glob.compile_matcher())
    };
    let (Some(file), Some(directory)) = (matcher(&pattern), matcher(&below)) else {
        return Vec::new();
    };
    // mise's walk prunes hidden directories as well as hidden files.
    let configuration = |path: &str| {
        let below = path
            .strip_prefix(pattern.as_str())
            .map_or(path, |below| below.trim_start_matches('/'));
        below.split('/').any(|part| part.starts_with('.')) || walk_config(below)
    };
    paths
        .filter(|path| {
            path.ends_with(".toml")
                && (*path == pattern
                    || ((file.is_match(path) || directory.is_match(path)) && !configuration(path)))
        })
        .collect()
}

/// The TOML task files configs include, each with the config that includes it.
/// mise reads a task file as tasks only: a top-level `[tools]` there is a task
/// named `tools`, not the tool section a config has.
pub(super) fn task_files<'a>(
    files: impl Iterator<Item = (&'a String, &'a crate::Text)>,
    all: &Files,
) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for (path, text) in files.filter(|(path, _)| !path.ends_with(".tool-versions")) {
        // An unread config is skipped and its own lookups fail.
        let Some(Ok(doc)) = text.read().ok().map(Document::parse) else {
            continue;
        };
        let includes = doc
            .get("task_config")
            .and_then(|config| config.get("includes"))
            .and_then(|includes| includes.as_array());
        for include in includes.into_iter().flatten().filter_map(Value::as_str) {
            let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
            for task_file in included_files(directory, include, all.keys().map(String::as_str))
                .into_iter()
                .filter(|task_file| task_file != path)
            {
                found
                    .entry(task_file.to_owned())
                    .or_insert_with(|| path.clone());
            }
        }
    }
    // A task file's own `[task_config]` is a task, not an include.
    let tasks: BTreeSet<_> = found.keys().cloned().collect();
    found.retain(|_, config| !tasks.contains(config));
    found
}

struct Reader<'a, 'b> {
    path: &'a str,
    text: &'a Lines<'b>,
    included_by: Option<&'a str>,
    out: Vec<Dependency>,
    skipped: &'a mut Vec<Skipped>,
}

/// `included_by` is the config that includes this file as a task file.
pub(super) fn extract(
    path: &str,
    text: &Lines<'_>,
    included_by: Option<&str>,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let mut reader = Reader {
        path,
        text,
        included_by,
        out: Vec::new(),
        skipped,
    };
    if path.ends_with(".tool-versions") {
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let code = line.split('#').next().unwrap_or("");
            let mut words = code.split_whitespace();
            if let Some(name) = words.next() {
                let mut search = name.len();
                for version in words {
                    if let Some(start) = line[search..].find(version) {
                        let start = start + search;
                        reader.add(
                            "",
                            name,
                            version,
                            offset + start..offset + start + version.len(),
                        );
                        search = start + version.len();
                    }
                }
            }
            offset += line.len();
        }
    } else {
        let doc = Document::parse(text).map_err(|e| e.to_string())?;
        if included_by.is_some() {
            // A task file holds tasks as top-level tables, written without the
            // `tasks.` prefix.
            for (task, item) in doc.iter() {
                reader.task_tools(item, &format!("/{}", pointer_token(task)));
            }
        } else {
            if let Some(tools) = doc.get("tools").and_then(|v| v.as_table_like()) {
                reader.tools_in(tools, "");
            }
            if let Some(tasks) = doc.get("tasks").and_then(|v| v.as_table_like()) {
                for (task, item) in tasks.iter() {
                    reader.task_tools(item, &format!("/tasks/{}", pointer_token(task)));
                }
            }
        }
    }
    Ok(reader.out)
}
impl Reader<'_, '_> {
    fn task_tools(&mut self, task: &toml_edit::Item, prefix: &str) {
        if let Some(tools) = task.get("tools").and_then(|v| v.as_table_like()) {
            self.tools_in(tools, prefix);
        }
    }
    fn tools_in(&mut self, tools: &dyn toml_edit::TableLike, prefix: &str) {
        for (name, item) in tools.iter() {
            if let Some(value) = item.as_value() {
                self.collect(prefix, name, value);
            } else if let Some(value) = item.get("version").and_then(|v| v.as_value()) {
                self.collect(prefix, name, value);
            }
        }
    }
    fn collect(&mut self, prefix: &str, name: &str, value: &Value) {
        match value {
            Value::String(v) => {
                let span = value.span().expect("immutable parser retains spans");
                self.add(prefix, name, v.value(), span);
            }
            Value::Array(values) => {
                for value in values.iter() {
                    self.collect(prefix, name, value);
                }
            }
            Value::InlineTable(table) => {
                if let Some(value) = table.get("version") {
                    self.collect(prefix, name, value);
                }
            }
            _ => {}
        }
    }
    fn add(&mut self, prefix: &str, name: &str, version: &str, span: std::ops::Range<usize>) {
        let (path, text) = (self.path, self.text);
        let (start, end) = (span.start, span.end);
        if ["path:", "ref:", "prefix:", "system"]
            .iter()
            .any(|p| version.starts_with(p))
        {
            return;
        }
        if let Some((_, reason)) = UNLISTED.iter().find(|(p, _)| name.starts_with(p)) {
            self.skipped.push(Skipped {
                manager: Manager::Mise,
                file: path.into(),
                line: Some(Location::new(text, start, end, String::new()).line),
                value: name.into(),
                reason: (*reason).into(),
            });
            return;
        }
        let version = version.strip_prefix("tag:").unwrap_or(version);
        // Mise versions are values, unlike npm ranges. A numeric extracted value
        // is already known even before querying a source; floating selectors are not.
        let resolved = (version
            .trim_start_matches('v')
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            || version
                .trim_start_matches('v')
                .split_once('-')
                .is_some_and(|(v, _)| {
                    v.split('.')
                        .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                }))
        .then(|| version.to_owned());
        self.out.push(Dependency {
            cooldown: None,
            manager: Manager::Mise,
            file: path.into(),
            location: Location::new(
                text,
                start,
                end,
                format!("{prefix}/tools/{}", pointer_token(name)),
            ),
            form: if self.path.ends_with(".tool-versions") {
                EditForm::Scalar(Syntax::Bare)
            } else {
                EditForm::Scalar(Syntax::Toml)
            },
            name: name.into(),
            package: name.into(),
            kind: DependencyKind::Tool,
            current: version.into(),
            resolved,
            digest: None,
            source: Source::Mise,
            scheme: Scheme::Loose,
            extract: None,
            included_by: self.included_by.map(str::to_owned),
            registry: None,
        });
    }
}

/// A backend mise installs tools with, and how the Count checks whether
/// a project can send its installs elsewhere.
struct Backend {
    /// The names `backends()` reports and `[settings]` tables spell it
    /// by. `pypi` and `pipx` share a row: pipx tools read the same index
    /// settings, so either name's settings or pip variables redirect
    /// both.
    names: &'static [&'static str],
    /// `[env]` key prefixes that can redirect it.
    env: &'static [&'static str],
    /// The project files `project_redirect` checks for it. `None` states
    /// that no project file redirects the backend — `gem` and `go`
    /// included, whose installs read no per-project registry settings.
    project: Option<ProjectCheck>,
    /// A package backend installs from a registry project settings can
    /// redirect, so the variables an `[env]` `_` directive loads could
    /// reach it.
    package: bool,
}

/// The project files a backend's settings live in, returning why a
/// lookup there fails.
type ProjectCheck = fn(&str, &str, &Files, &crate::nuget::Feeds) -> Option<String>;

const BACKENDS: &[Backend] = &[
    Backend {
        names: &["npm"],
        env: &["NPM_CONFIG_", "YARN_", "COREPACK_"],
        project: Some(npm_project),
        package: true,
    },
    Backend {
        names: &["pypi", "pipx"],
        env: &["PIP_", "UV_", "PIPX_"],
        project: Some(python_project),
        package: true,
    },
    Backend {
        names: &["cargo"],
        env: &["CARGO_"],
        project: Some(cargo_project),
        package: true,
    },
    Backend {
        names: &["gem"],
        env: &["GEM_", "BUNDLE_"],
        project: None,
        package: true,
    },
    Backend {
        names: &["dotnet"],
        env: &["NUGET_", "DOTNET_"],
        project: Some(dotnet_project),
        package: true,
    },
    Backend {
        names: &["go"],
        env: &["GO"],
        project: None,
        package: true,
    },
    // Backend kinds that install tools but read no registry settings the
    // Count checks: no env prefix or project file redirects them, though
    // a `[settings.<name>]` table still moves their tools.
    Backend {
        names: &["aqua"],
        env: &[],
        project: None,
        package: false,
    },
    Backend {
        names: &["ubi"],
        env: &[],
        project: None,
        package: false,
    },
    Backend {
        names: &["github"],
        env: &[],
        project: None,
        package: false,
    },
    Backend {
        names: &["gitlab"],
        env: &[],
        project: None,
        package: false,
    },
    Backend {
        names: &["http"],
        env: &[],
        project: None,
        package: false,
    },
    Backend {
        names: &["conda"],
        env: &[],
        project: None,
        package: false,
    },
    Backend {
        names: &["spm"],
        env: &[],
        project: None,
        package: false,
    },
];

/// Top-level tables of a mise configuration that cannot change where a tool
/// installs from, apart from `env` and `settings`, which are read below.
const PLAIN_TABLES: [&str; 9] = [
    "tools",
    "tasks",
    "task_config",
    "vars",
    "env",
    "settings",
    "min_version",
    "redactions",
    "wrappers",
];

/// Core tools, whose `[settings]` tables move only themselves.
const CORE_TOOLS: [&str; 12] = [
    "node", "python", "ruby", "java", "go", "rust", "erlang", "elixir", "bun", "deno", "zig",
    "swift",
];

/// `[settings]` keys that cannot change where a tool installs from.
const PLAIN_SETTINGS: [&str; 18] = [
    "experimental",
    "lockfile",
    "idiomatic_version_file",
    "idiomatic_version_file_enable_tools",
    "idiomatic_version_file_disable_tools",
    "legacy_version_file",
    "not_found_auto_install",
    "auto_install",
    "exec_auto_install",
    "task_run_auto_install",
    "jobs",
    "yes",
    "quiet",
    "verbose",
    "color",
    "status",
    "task_output",
    "task_timings",
];

/// The folders mise reads configuration from inside a directory, beside
/// that directory's own `mise.toml`.
const CONFIG_FOLDERS: [&str; 3] = [".config/mise/", ".mise/", "mise/"];

/// Whether `name` is `<stem>.toml` or `<stem>.<x>.toml` with `x` not
/// empty: the plain file and its `.local`, environment and environment
/// `.local` variants.
fn variant(name: &str, stem: &str) -> bool {
    name.strip_prefix(stem)
        .and_then(|rest| rest.strip_suffix(".toml"))
        .is_some_and(|rest| rest.is_empty() || rest.len() > 1 && rest.starts_with('.'))
}

/// The directory a mise configuration at `path` configures, with its
/// trailing slash, or none when mise reads no configuration by that name.
///
/// The names, from mise 2026.10.11 (the version `defaults.toml` pins):
/// `src/config/mod.rs` builds `DEFAULT_CONFIG_FILENAMES` from
/// `SHARED_CONFIG_FILENAMES`, `LOCAL_OVERRIDE_CONFIG_FILENAMES` and
/// `env_config_patterns`, each relative to a directory mise walks up
/// through. With `<dir>` each of `.config/mise`, `.mise` and `mise`:
///
/// - `mise.toml`, `.mise.toml`, `.config/mise.toml`, and the
///   `.local`, `<env>` and `<env>.local` variants of each
///   (`mise.local.toml`, `.mise.prod.toml`, `.config/mise.prod.local.toml`);
/// - `<dir>/config.toml` and its variants (`mise/config.local.toml`);
/// - `.config/mise/mise.toml` and `.config/mise/mise.local.toml`, with
///   no environment variants;
/// - `<dir>/conf.d/*.toml`, any name, environment suffixed or not, but
///   not a hidden one (`config_glob`);
/// - `<dir>/conf.d/<folder>/mise.toml` and its variants, the folder not
///   hidden (`is_conf_d_folder_file`).
///
/// `MISE_DEFAULT_CONFIG_FILENAME` and `MISE_OVERRIDE_CONFIG_FILENAMES`
/// replace these only from the environment or a `.miserc.toml`, which
/// counts below. `src/config/miserc.rs` (`find_miserc_files`) reads
/// `.miserc.toml`, `.miserc.local.toml` and `.config/miserc.toml` in each
/// directory; they can choose the environment and those overrides.
/// `.tool-versions`, also in the list, holds versions only.
///
/// Every file named `mise.toml` or a variant configures its own
/// directory, where mise reads it when run there. One mise reads from an
/// ancestor, such as `.config/mise.toml` or `mise/conf.d/x.toml`,
/// configures that ancestor: from there mise reads it in every folder
/// below.
fn config_root(path: &str) -> Option<&str> {
    let leaf = leaf(path);
    let dir = &path[..path.len() - leaf.len()];
    // A folder form names the directory above the folder.
    for folder in CONFIG_FOLDERS {
        for (at, _) in path.match_indices(folder) {
            if at != 0 && path.as_bytes()[at - 1] != b'/' {
                continue;
            }
            let rest = &path[at + folder.len()..];
            let read = match rest.split('/').collect::<Vec<_>>()[..] {
                [name] => {
                    variant(name, "config")
                        || folder == ".config/mise/"
                            && matches!(name, "mise.toml" | "mise.local.toml")
                }
                ["conf.d", name] => name.ends_with(".toml") && !name.starts_with('.'),
                ["conf.d", fragment, name] => !fragment.starts_with('.') && variant(name, "mise"),
                _ => false,
            };
            if read {
                return Some(&path[..at]);
            }
        }
    }
    // `.config/mise.toml` and `.config/miserc.toml` name the directory
    // above `.config`.
    if let Some(above) = dir
        .strip_suffix(".config/")
        .filter(|above| above.is_empty() || above.ends_with('/'))
        && (variant(leaf, "mise") || leaf == "miserc.toml")
    {
        return Some(above);
    }
    (variant(leaf, "mise")
        || variant(leaf, ".mise")
        || matches!(leaf, ".miserc.toml" | ".miserc.local.toml"))
    .then_some(dir)
}

/// Whether `path` is a mise configuration file: one `config_root` names a
/// directory for.
pub(crate) fn config_file(path: &str) -> bool {
    config_root(path).is_some()
}

/// Whether `path` is a `.miserc.toml`, which can rename every other
/// configuration file and choose the environment.
fn miserc(path: &str) -> bool {
    matches!(leaf(path), ".miserc.toml" | ".miserc.local.toml")
        || path == ".config/miserc.toml"
        || path.ends_with("/.config/miserc.toml")
}

/// The backends `tool` can install with: its `backend:` prefix, or those
/// mise's registry lists for a short name; `core` for one it does not list.
fn backends(tool: &str) -> Vec<String> {
    if let Some((backend, _)) = tool.split_once(':') {
        return vec![backend.to_owned()];
    }
    let listed: Vec<String> = patchula_mise::backends(tool)
        .iter()
        .filter_map(|backend| backend.split_once(':').map(|(kind, _)| kind.to_owned()))
        .collect();
    if listed.is_empty() {
        vec!["core".to_owned()]
    } else {
        listed
    }
}

/// The backends a mise configuration could send elsewhere, `*` standing
/// for every tool; none when the Count cannot read it. Anything but the
/// plain tables and settings above counts against every tool.
fn redirected_backends(path: &str, text: &str) -> Option<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    if miserc(path) {
        out.insert("*".to_owned());
        return Some(out);
    }
    let doc = Document::parse(text).ok()?;
    for (key, _) in doc.iter() {
        if !PLAIN_TABLES.contains(&key) {
            out.insert("*".to_owned());
        }
    }
    if let Some(env) = doc.get("env") {
        let env = env.as_table_like()?;
        for (key, _) in env.iter() {
            let key = key.to_ascii_uppercase();
            // Directives: `path` and `python` only set `PATH` and a virtual
            // environment; others, such as `file` and `source`, load
            // variables from files.
            let loads = key == "_"
                && env
                    .get("_")
                    .and_then(|d| d.as_table_like())
                    .is_none_or(|d| d.iter().any(|(name, _)| !matches!(name, "path" | "python")));
            if key == "_" {
                if loads {
                    out.extend(
                        BACKENDS
                            .iter()
                            .filter(|backend| backend.package)
                            .flat_map(|backend| backend.names.iter().copied())
                            .map(str::to_owned),
                    );
                }
            } else if key.starts_with("MISE_") {
                out.insert("*".to_owned());
            } else {
                let mut redirected = false;
                for backend in BACKENDS {
                    if backend.env.iter().any(|prefix| key.starts_with(*prefix)) {
                        out.extend(backend.names.iter().map(|name| (*name).to_owned()));
                        redirected = true;
                    }
                }
                if !redirected
                    && (["HOME", "PATH", "XDG_CONFIG_HOME", "XDG_DATA_HOME"]
                        .contains(&key.as_str())
                        || ["MIRROR", "DIST_SERVER", "PROXY", "_CONFIG", "RC"]
                            .iter()
                            .any(|part| key.contains(part)))
                {
                    // Where tools read their configuration, find commands,
                    // or download from: homes, paths, mirrors and proxies.
                    out.insert("*".to_owned());
                }
            }
        }
    }
    if let Some(settings) = doc.get("settings") {
        let settings = settings.as_table_like()?;
        for (key, _) in settings.iter() {
            // A backend's own settings move its tools only; a core tool's,
            // such as `[settings.python]`, that tool only.
            if PLAIN_SETTINGS.contains(&key) {
            } else if let Some(backend) =
                BACKENDS.iter().find(|backend| backend.names.contains(&key))
            {
                out.extend(backend.names.iter().map(|name| (*name).to_owned()));
            } else if CORE_TOOLS.contains(&key) {
                out.insert(format!("tool:{key}"));
            } else {
                out.insert("*".to_owned());
            }
        }
    }
    Some(out)
}

/// Whether `path` is in or below `dir`, or governs it: the folder a
/// file's settings apply to. A `.cargo` configuration applies to its
/// parent's folder, Cargo reading it from wherever it runs up to the
/// repository root.
fn related(dir: &str, path: &str) -> bool {
    let folder = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let folder = folder.strip_suffix(".cargo/").unwrap_or(folder);
    dir.starts_with(folder) || folder.starts_with(dir)
}

/// The first file `related` to `dir` that `test` accepts, as the reason
/// a lookup there fails. A test sees `Err` for a file the Count did not
/// read, which may say anything.
fn found(
    dir: &str,
    files: &Files,
    test: &dyn Fn(&str, Result<&str, crate::Unread>) -> bool,
    what: &str,
) -> Option<String> {
    files
        .iter()
        .find(|(path, text)| related(dir, path) && test(path, text.read()))
        .map(|(path, _)| format!("the {what} {path} may name"))
}

/// The project's own npm registry settings in or below `dir`, or above
/// it, that mise's npm backend reads when it runs there.
fn npm_project(
    dir: &str,
    _tool: &str,
    files: &Files,
    _feeds: &crate::nuget::Feeds,
) -> Option<String> {
    found(
        dir,
        files,
        // npm's own config parser accepts forms the Count's reader does
        // not (quoted keys, lone `\r` line breaks), so any mention of a
        // registry counts.
        &|path, text| {
            matches!(leaf(path), ".npmrc" | ".yarnrc.yml")
                && text.map_or(true, |text| text.to_ascii_lowercase().contains("registry"))
        },
        "npm registry",
    )
}

/// The project's own Python index settings in or below `dir`, or above
/// it, that mise's pypi and pipx backends read when they run there.
fn python_project(
    dir: &str,
    _tool: &str,
    files: &Files,
    _feeds: &crate::nuget::Feeds,
) -> Option<String> {
    found(
        dir,
        files,
        &|path, text| match leaf(path) {
            "uv.toml" => text.map_or(true, crate::pypi::settings::uv_private_index),
            // pip's config parser accepts more than the Count could
            // check safely (`--` key prefixes, lone `\r`, Unicode case
            // folding, options that pull in requirement files), so any
            // pip configuration counts.
            "pip.conf" | "pip.ini" => true,
            "pyproject.toml" => text.map_or(true, crate::pypi::settings::private_index),
            _ => false,
        },
        "Python index",
    )
}

/// The project's own Cargo source replacement or registries in or below
/// `dir`, or above it, that mise's cargo backend reads when it runs
/// there.
fn cargo_project(
    dir: &str,
    _tool: &str,
    files: &Files,
    _feeds: &crate::nuget::Feeds,
) -> Option<String> {
    crate::cargo::replaced(
        files,
        &crate::cargo::parsed(files, |_| true),
        &format!("{dir}x"),
    )
    .or_else(|| {
        found(
            dir,
            files,
            &|path, text| {
                crate::cargo::config_file(path)
                    && text.map_or(true, crate::cargo::nonpublic_registry)
            },
            "Cargo registry",
        )
    })
}

/// The NuGet feeds around `dir` that are not nuget.org's, which mise's
/// dotnet backend reads when it runs there.
fn dotnet_project(
    dir: &str,
    tool: &str,
    _files: &Files,
    feeds: &crate::nuget::Feeds,
) -> Option<String> {
    let package = tool.split_once(':').map_or(tool, |(_, name)| name);
    match feeds.origin(&format!("{dir}x"), package) {
        crate::origins::Origin::Index(crate::origins::Named::Public) => None,
        _ => Some(format!("NuGet feeds around {dir} that are not nuget.org's")),
    }
}

/// The project's own registry settings a package backend reads when mise
/// runs it in `dir`: `.npmrc` and `.yarnrc.yml` naming a registry for
/// npm, Python index settings for pypi and pipx, Cargo source
/// replacement or registries for cargo, and NuGet feeds for dotnet. One
/// the Count cannot read counts; a backend whose row states `project:
/// None` has no such check.
fn project_redirect(
    kind: &str,
    dir: &str,
    tool: &str,
    files: &Files,
    feeds: &crate::nuget::Feeds,
) -> Option<String> {
    BACKENDS
        .iter()
        .find(|backend| backend.names.contains(&kind))
        .and_then(|backend| backend.project)
        .and_then(|check| check(dir, tool, files, feeds))
}

/// Options on the tool's entry, in any of `configs` or its own file,
/// beyond its version and platforms, such as install arguments or an API
/// URL, which can change where it comes from. The reason names the options
/// when every one is plainly a name.
fn tool_options(dep: &Dependency, configs: &[&str], files: &Files) -> Option<String> {
    let options = |entry: &toml_edit::Item| {
        let keys = |table: &dyn toml_edit::TableLike| {
            table
                .iter()
                .map(|(key, _)| key.to_owned())
                .filter(|key| !matches!(key.as_str(), "version" | "os" | "minimum_release_age"))
                .collect::<Vec<_>>()
        };
        match entry {
            toml_edit::Item::Value(Value::Array(values)) => values
                .iter()
                .filter_map(|v| v.as_inline_table())
                .flat_map(|t| keys(t))
                .collect(),
            entry => entry.as_table_like().map(keys).unwrap_or_default(),
        }
    };
    let task_tools =
        |task: &toml_edit::Item| task.get("tools").and_then(|t| t.get(&dep.name)).cloned();
    for file in configs.iter().copied().chain([dep.file.as_str()]) {
        let Some(doc) = files
            .get(file)
            .and_then(|t| Document::parse(t.read().ok()?).ok())
        else {
            continue;
        };
        let mut entries = Vec::new();
        if file == dep.file && dep.included_by.is_some() {
            // A task file holds tasks as top-level tables.
            entries.extend(doc.iter().filter_map(|(_, task)| task_tools(task)));
        } else {
            if let Some(tools) = doc.get("tools").and_then(|t| t.as_table_like()) {
                entries.extend(tools.get(&dep.name).cloned());
            }
            let tasks = doc.get("tasks").and_then(|t| t.as_table_like());
            entries.extend(
                tasks
                    .into_iter()
                    .flat_map(|t| t.iter())
                    .filter_map(|(_, task)| task_tools(task)),
            );
        }
        let mut keys: Vec<String> = Vec::new();
        for key in entries.iter().flat_map(options) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        if keys.is_empty() {
            continue;
        }
        let named: Option<Vec<String>> = keys
            .iter()
            .map(|key| crate::origins::shown(key).map(|key| format!("`{key}`")))
            .collect();
        return Some(match named.as_deref() {
            Some([one]) => format!(
                "its entry in {file} sets {one}, which may move where mise installs it from"
            ),
            Some([rest @ .., last]) => format!(
                "its entry in {file} sets {} and {last}, which may move where mise installs it from",
                rest.join(", ")
            ),
            _ => {
                format!("its entry in {file} has options that may move where mise installs it from")
            }
        });
    }
    None
}

/// Why the lookup of the mise tool `dep` cannot be taken to list what mise
/// installs: the Count lists versions in a sandbox without the project's
/// configuration, while mise installs a tool with it. A mise configuration
/// governing the tool's folder, one above it or one below it that holds
/// anything but plain tables and settings, or `[env]` that could redirect
/// one of the tool's backends; one the Count cannot read; options on the
/// tool's own entry; or the project's own registry settings for a package
/// backend fail the lookup.
pub(crate) fn redirected(
    dep: &Dependency,
    configs: &[&str],
    files: &Files,
    feeds: &crate::nuget::Feeds,
) -> Option<String> {
    // An unread config may include any file through `task_config`, from any
    // folder, so it may govern this one however far away it is.
    if let Some(config) = configs
        .iter()
        .find(|config| files.get(**config).is_some_and(|text| text.read().is_err()))
    {
        return Some(format!(
            "{config}, which the Count cannot read and which may include this configuration"
        ));
    }
    let mut dirs = vec![&dep.file[..dep.file.rfind('/').map_or(0, |i| i + 1)]];
    if let Some(including) = &dep.included_by {
        dirs.push(config_root(including).unwrap_or_default());
    }
    let mut kinds = backends(&dep.package);
    kinds.push(format!("tool:{}", dep.package));
    let mut related = Vec::new();
    for config in configs {
        let folder = config_root(config).unwrap_or_default();
        if !dirs
            .iter()
            .any(|dir| dir.starts_with(folder) || folder.starts_with(*dir))
        {
            continue;
        }
        related.push(*config);
        let text = files.get(*config)?;
        let Some(redirected) = text
            .read()
            .ok()
            .and_then(|text| redirected_backends(config, text))
        else {
            return Some(Unread::Failed.reason(config));
        };
        if redirected.contains("*") || kinds.iter().any(|kind| redirected.contains(kind)) {
            return Some(format!(
                "settings in {config} may move where mise installs it from"
            ));
        }
    }
    if let Some(reason) = tool_options(dep, &related, files) {
        return Some(reason);
    }
    dirs.iter().find_map(|dir| {
        kinds
            .iter()
            .find_map(|kind| project_redirect(kind, dir, &dep.package, files, feeds))
    })
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn proposing(&self, _dep: &Dependency) -> Proposing {
        Proposing {
            selector_resolved: true,
            resolved_via_listing: true,
            ..Proposing::BASE
        }
    }
    fn selected(&self, _config: &Config, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Mise, path).selected
    }
    /// A package backend mise runs in the project reads its settings.
    fn registry_file(&self, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Mise, path).registry
    }
    fn declared_form(
        &self,
        dep: &Dependency,
        current: &str,
        value: &str,
    ) -> Result<Option<String>, String> {
        if !current.starts_with("tag:") {
            return Ok(None);
        }
        if current.strip_prefix("tag:") != Some(&dep.current) {
            return Err("mise tag location mismatch".into());
        }
        Ok(Some(format!("tag:{value}")))
    }
    fn prepare<'a>(
        &self,
        scanner: &'a Scanner,
        files: &'a Files,
        shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        let task_files = task_files(
            files
                .iter()
                .filter(|(path, _)| scanner.selected(Manager::Mise, path)),
            files,
        );
        let feeds = shared.feeds.as_ref().expect("NuGet feeds prepared");
        Ok(Prepared {
            extract: Some(Box::new(move |path, text, out| {
                let result = extract(
                    path,
                    text,
                    task_files.get(path).map(String::as_str),
                    &mut out.skipped,
                );
                file_result(out, path, result);
                Ok(())
            })),
            finish: Some(Box::new(move |out| {
                let configs: Vec<&str> = files
                    .keys()
                    .filter(|path| config_file(path))
                    .map(String::as_str)
                    .collect();
                for dep in out
                    .dependencies
                    .iter_mut()
                    .filter(|d| d.manager == Manager::Mise && d.source == Source::Mise)
                {
                    if let Some(reason) = redirected(dep, &configs, files, feeds) {
                        dep.registry = origins::Named::Unusable(reason).registry();
                    }
                }
                Ok(())
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name mise 2026.10.11 reads configuration by, root and nested,
    /// with the directory it configures.
    #[test]
    fn mise_config_names_are_the_ones_mise_reads() {
        for (name, root) in [
            ("mise.toml", ""),
            ("mise.local.toml", ""),
            ("mise.prod.toml", ""),
            ("mise.prod.local.toml", ""),
            ("mise.ci.linux.toml", ""),
            (".mise.toml", ""),
            (".mise.local.toml", ""),
            (".mise.prod.toml", ""),
            (".mise.prod.local.toml", ""),
            (".config/mise.toml", ""),
            (".config/mise.local.toml", ""),
            (".config/mise.prod.toml", ""),
            (".config/mise.prod.local.toml", ""),
            (".config/mise/config.toml", ""),
            (".config/mise/config.local.toml", ""),
            (".config/mise/config.prod.toml", ""),
            (".config/mise/config.prod.local.toml", ""),
            (".config/mise/mise.toml", ""),
            (".config/mise/mise.local.toml", ""),
            (".config/mise/conf.d/node.toml", ""),
            (".config/mise/conf.d/node.prod.toml", ""),
            (".config/mise/conf.d/git/mise.toml", ""),
            (".config/mise/conf.d/git/mise.local.toml", ""),
            (".config/mise/conf.d/git/mise.prod.toml", ""),
            (".mise/config.toml", ""),
            (".mise/config.local.toml", ""),
            (".mise/config.prod.toml", ""),
            (".mise/conf.d/node.toml", ""),
            (".mise/conf.d/git/mise.toml", ""),
            (".mise/conf.d/git/mise.prod.local.toml", ""),
            ("mise/config.toml", ""),
            ("mise/config.local.toml", ""),
            ("mise/config.prod.toml", ""),
            ("mise/conf.d/node.toml", ""),
            ("mise/conf.d/git/mise.toml", ""),
            (".miserc.toml", ""),
            (".miserc.local.toml", ""),
            (".config/miserc.toml", ""),
            // A `mise.toml` configures its own folder, wherever it is.
            ("mise/mise.toml", "mise/"),
            (".mise/mise.local.toml", ".mise/"),
            (".config/mise/mise.prod.toml", ".config/mise/"),
            ("tools/mise/mise.toml", "tools/mise/"),
            (".config/.mise.toml", ".config/"),
        ] {
            assert_eq!(config_root(name), Some(root), "{name}");
            let nested = format!("app/{name}");
            assert_eq!(
                config_root(&nested),
                Some(format!("app/{root}").as_str()),
                "{nested}"
            );
        }
    }

    /// Other TOML files in and around mise's folders are not configuration:
    /// a crate named `mise`, task files, hidden `conf.d` files, names mise
    /// does not read.
    #[test]
    fn other_toml_files_in_mise_folders_are_not_configuration() {
        for name in [
            "crates/mise/Cargo.toml",
            "mise/Cargo.toml",
            "mise/tools.toml",
            "mise/tasks.toml",
            "mise/tasks/build.toml",
            ".mise/tools.toml",
            ".mise/tasks/build.toml",
            ".config/mise/tools.toml",
            ".config/mise/settings.toml",
            "mise/conf.d/.node.toml",
            "mise/conf.d/node.json",
            "mise/conf.d/git/tools.toml",
            "nginx/conf.d/site.toml",
            "mise/config/x.toml",
            "mise/configs.toml",
            "mise.toml.bak",
            "mise..toml",
            "mymise.toml",
            "notmise/config.toml",
            ".config/mise/config..toml",
            ".miserc",
            ".miserc.yaml",
            "miserc.toml",
            "x.config/miserc.toml",
            ".tool-versions",
        ] {
            assert!(!config_file(name), "{name}");
            assert!(!config_file(&format!("app/{name}")), "app/{name}");
        }
    }

    /// A task-file walk skips what mise's walk would read as
    /// configuration, and only that.
    #[test]
    fn task_walks_skip_only_what_mise_reads_as_configuration() {
        let paths = [
            "tasks/build.toml",
            "tasks/mise.toml",
            "tasks/sub/mise.local.toml",
            "tasks/mise.prod.toml",
            "tasks/mise/config.toml",
            "tasks/mise/conf.d/x.toml",
            "tasks/mise/conf.d/git/mise.toml",
            "tasks/mise/tools.toml",
            "tasks/sub/mise/config.toml",
            "tasks/.hidden/x.toml",
        ];
        assert_eq!(
            included_files("", "tasks", paths.into_iter()),
            [
                "tasks/build.toml",
                "tasks/mise.prod.toml",
                "tasks/mise/tools.toml",
                "tasks/sub/mise/config.toml",
            ]
        );
    }
}
