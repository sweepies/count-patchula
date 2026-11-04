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

/// The TOML files in `paths` that `include` names for a config in `directory`.
/// mise accepts a file, a glob such as `tasks/*.toml`, or a directory, whose
/// TOML files at any depth are task files, and `.` is the repository root. A
/// directory walk skips what mise reads as configuration: hidden files and
/// `mise.toml` or `mise.local.toml`. A path that leaves the repository names
/// nothing.
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
        let leaf = path.rsplit('/').next().unwrap_or(path);
        let below = path.strip_prefix(pattern.as_str()).unwrap_or(path);
        below.split('/').any(|part| part.starts_with('.'))
            || matches!(leaf, "mise.toml" | "mise.local.toml")
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
    files: impl Iterator<Item = (&'a String, &'a String)>,
    all: &Files,
) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for (path, text) in files.filter(|(path, _)| !path.ends_with(".tool-versions")) {
        let Ok(doc) = Document::parse(text.as_str()) else {
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

struct Reader<'a> {
    path: &'a str,
    text: &'a str,
    included_by: Option<&'a str>,
    out: Vec<Dependency>,
    skipped: &'a mut Vec<Skipped>,
}

/// `included_by` is the config that includes this file as a task file.
pub(super) fn extract(
    path: &str,
    text: &str,
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
impl Reader<'_> {
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
            manager: Manager::Mise,
            file: path.into(),
            location: Location::new(
                text,
                start,
                end,
                format!("{prefix}/tools/{}", pointer_token(name)),
            ),
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

/// Backends that install from a package registry a project's settings can
/// redirect: npm and Python indexes, Cargo sources and NuGet feeds.
const PACKAGE_BACKENDS: [&str; 6] = ["npm", "pypi", "pipx", "cargo", "gem", "dotnet"];

/// Top-level tables of a mise configuration that cannot change where a tool
/// installs from, apart from `env` and `settings`, which are read below.
const PLAIN_TABLES: [&str; 8] = [
    "tools",
    "tasks",
    "task_config",
    "vars",
    "env",
    "settings",
    "min_version",
    "redactions",
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

/// Whether `path` is a mise configuration file, under any name mise reads
/// one by: `mise.toml` and `.mise.toml` with their `.local` and
/// environment variants, `mise/` and `.mise/` folders, `.config/mise.toml`,
/// `.config/mise/`, and their `conf.d` folders. `.miserc` files, which can
/// choose the environment, count too.
pub(crate) fn config_file(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let named = leaf
        .strip_prefix('.')
        .unwrap_or(leaf)
        .strip_prefix("mise")
        .is_some_and(|rest| rest == ".toml" || rest.starts_with('.') && rest.ends_with(".toml"));
    let in_folder = ["mise/", ".mise/", ".config/mise/"].iter().any(|dir| {
        let at = path
            .find(dir)
            .filter(|at| *at == 0 || path.as_bytes()[at - 1] == b'/');
        at.is_some_and(|at| path[at + dir.len()..].ends_with(".toml"))
    });
    named || in_folder || path.ends_with(".config/mise.toml") || leaf.starts_with(".miserc")
}

/// The folder a mise configuration file governs: the one holding it, or
/// the one above its `mise/`, `.mise/` or `.config/` folder.
fn governed(config: &str) -> &str {
    for dir in [".config/mise/", ".mise/", "mise/", ".config/"] {
        if let Some(at) = config
            .find(dir)
            .filter(|at| *at == 0 || config.as_bytes()[at - 1] == b'/')
        {
            return &config[..at];
        }
    }
    &config[..config.rfind('/').map_or(0, |i| i + 1)]
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
    if path
        .rsplit('/')
        .next()
        .is_some_and(|leaf| leaf.starts_with(".miserc"))
    {
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
            let kinds: &[&str] = if key == "_" && !loads {
                &[]
            } else if key == "_" {
                &PACKAGE_BACKENDS
            } else if key.starts_with("MISE_") {
                &["*"]
            } else if key.starts_with("NPM_CONFIG_") || key.starts_with("YARN_") {
                &["npm"]
            } else if ["PIP_", "UV_", "PIPX_"].iter().any(|p| key.starts_with(p)) {
                &["pypi", "pipx"]
            } else if key.starts_with("CARGO_") {
                &["cargo"]
            } else if key.starts_with("GEM_") || key.starts_with("BUNDLE_") {
                &["gem"]
            } else if key.starts_with("NUGET_") || key.starts_with("DOTNET_") {
                &["dotnet"]
            } else {
                &[]
            };
            out.extend(kinds.iter().map(|k| (*k).to_owned()));
        }
    }
    if let Some(settings) = doc.get("settings") {
        let settings = settings.as_table_like()?;
        for (key, _) in settings.iter() {
            if !PLAIN_SETTINGS.contains(&key) {
                out.insert("*".to_owned());
            }
        }
    }
    Some(out)
}

/// The project's own registry settings in or below `dir`, or above it, that
/// a package backend reads when mise runs it there: `.npmrc` and
/// `.yarnrc.yml` naming a registry for npm, Python index settings for pypi,
/// Cargo source replacement or registries for cargo, and NuGet feeds for
/// dotnet. One the Count cannot read counts.
fn project_redirect(
    kind: &str,
    dir: &str,
    tool: &str,
    files: &Files,
    feeds: &crate::nuget::Feeds,
) -> Option<String> {
    let related = |path: &str| {
        let folder = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        dir.starts_with(folder) || folder.starts_with(dir)
    };
    let leaf = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    let unreadable = |text: &str| text == crate::UNREADABLE_SETTINGS;
    let found = |test: &dyn Fn(&str, &str) -> bool, what: &str| {
        files
            .iter()
            .find(|(path, text)| related(path) && test(path, text))
            .map(|(path, _)| format!("the {what} {path} may name"))
    };
    match kind {
        "npm" => found(
            &|path, text| {
                matches!(leaf(path).as_str(), ".npmrc" | ".yarnrc.yml")
                    && (unreadable(text) || text.to_ascii_lowercase().contains("registry"))
            },
            "npm registry",
        ),
        "pypi" | "pipx" => found(
            &|path, text| match leaf(path).as_str() {
                "uv.toml" | "pip.conf" | "pip.ini" => true,
                "pyproject.toml" => {
                    unreadable(text)
                        || ["index", "find-links", "source"]
                            .iter()
                            .any(|w| text.contains(w))
                }
                _ => false,
            },
            "Python index",
        ),
        "cargo" => crate::cargo::replaced(files, &format!("{dir}x")).or_else(|| {
            found(
                &|path, text| {
                    crate::cargo::config_file(path)
                        && (unreadable(text) || text.contains("registr"))
                },
                "Cargo registry",
            )
        }),
        "dotnet" => {
            let package = tool.split_once(':').map_or(tool, |(_, name)| name);
            match feeds.origin(&format!("{dir}x"), package) {
                crate::origins::Origin::Index(crate::origins::Named::Public) => None,
                _ => Some(format!("NuGet feeds around {dir} that are not nuget.org's")),
            }
        }
        _ => None,
    }
}

/// Options on a tool's own entry beyond its version and platforms, such as
/// install arguments or an API URL, which can change where it comes from.
fn tool_options(dep: &Dependency, files: &Files) -> Option<String> {
    let doc = files
        .get(&dep.file)
        .and_then(|text| Document::parse(text.as_str()).ok())?;
    let mut entries = Vec::new();
    if let Some(tools) = doc.get("tools").and_then(|t| t.as_table_like()) {
        entries.extend(tools.get(&dep.name));
    }
    let tasks = doc.get("tasks").and_then(|t| t.as_table_like());
    for (_, task) in tasks.into_iter().flat_map(|t| t.iter()) {
        entries.extend(task.get("tools").and_then(|t| t.get(&dep.name)));
    }
    entries
        .into_iter()
        .filter_map(|entry| {
            entry
                .as_table_like()
                .map(|t| t.iter().map(|(k, _)| k.to_owned()).collect::<Vec<_>>())
        })
        .flatten()
        .any(|key| !matches!(key.as_str(), "version" | "os"))
        .then(|| {
            format!(
                "options on {} in {} that may move where mise installs it from",
                crate::origins::shown(&dep.name),
                dep.file
            )
        })
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
    let mut dirs = vec![&dep.file[..dep.file.rfind('/').map_or(0, |i| i + 1)]];
    if let Some(including) = &dep.included_by {
        dirs.push(governed(including));
    }
    let kinds = backends(&dep.package);
    for config in configs {
        let folder = governed(config);
        if !dirs
            .iter()
            .any(|dir| dir.starts_with(folder) || folder.starts_with(*dir))
        {
            continue;
        }
        let text = files.get(*config)?;
        let Some(redirected) = (text.as_str() != crate::UNREADABLE_SETTINGS)
            .then(|| redirected_backends(config, text))
            .flatten()
        else {
            return Some(format!("{config}, which the Count cannot read"));
        };
        if redirected.contains("*") || kinds.iter().any(|kind| redirected.contains(kind)) {
            return Some(format!(
                "settings in {config} that may move where mise installs it from"
            ));
        }
    }
    if let Some(reason) = tool_options(dep, files) {
        return Some(reason);
    }
    dirs.iter().find_map(|dir| {
        kinds
            .iter()
            .find_map(|kind| project_redirect(kind, dir, &dep.package, files, feeds))
    })
}
