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

/// The backends a mise configuration's `[env]` and `[settings]` could send
/// elsewhere, `*` standing for all; none when the Count cannot read it.
fn redirected_backends(text: &str) -> Option<BTreeSet<String>> {
    let doc = Document::parse(text).ok()?;
    let mut out = BTreeSet::new();
    let all = || PACKAGE_BACKENDS.iter().map(|b| (*b).to_owned());
    if let Some(env) = doc.get("env").and_then(|v| v.as_table_like()) {
        for (key, _) in env.iter() {
            let key = key.to_ascii_uppercase();
            let kinds: &[&str] = if key == "_" {
                // Directives that load variables from files.
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
    if let Some(settings) = doc.get("settings").and_then(|v| v.as_table_like()) {
        for (key, _) in settings.iter() {
            match key {
                "url_replacements" | "env_file" | "env_files" => {
                    out.insert("*".to_owned());
                }
                key if PACKAGE_BACKENDS.contains(&key)
                    || [
                        "aqua", "ubi", "github", "gitlab", "http", "conda", "go", "spm",
                    ]
                    .contains(&key) =>
                {
                    out.insert(key.to_owned());
                }
                _ => {}
            }
        }
    }
    if out.contains("*") {
        out.extend(all());
    }
    Some(out)
}

/// The project's own registry settings around `dir` that a package backend
/// reads when mise runs it there: `.npmrc` and `.yarnrc.yml` naming a
/// registry for npm, Python index settings for pypi, Cargo source
/// replacement for cargo, and NuGet feeds for dotnet.
fn project_redirect(
    kind: &str,
    dir: &str,
    tool: &str,
    files: &Files,
    feeds: &crate::nuget::Feeds,
) -> Option<String> {
    let above = |path: &str| {
        let folder = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        dir.starts_with(folder)
    };
    let leaf = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    match kind {
        "npm" => files
            .iter()
            .find(|(path, text)| {
                above(path)
                    && matches!(leaf(path).as_str(), ".npmrc" | ".yarnrc.yml")
                    && text.to_ascii_lowercase().contains("registry")
            })
            .map(|(path, _)| format!("the npm registry {path} may name")),
        "pypi" | "pipx" => files
            .iter()
            .find(|(path, text)| {
                above(path)
                    && match leaf(path).as_str() {
                        "uv.toml" | "pip.conf" | "pip.ini" => true,
                        "pyproject.toml" => text.contains("index"),
                        _ => false,
                    }
            })
            .map(|(path, _)| format!("the Python index {path} may name")),
        "cargo" => crate::cargo::replaced(files, &format!("{dir}x")),
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

/// Why the lookup of the mise tool `dep` cannot be taken to list what mise
/// installs: the Count lists versions in a sandbox without the project's
/// configuration, while mise installs a package backend's tools with it.
/// A mise configuration in the tool's folder, above it or below it whose
/// `[env]` or `[settings]` could redirect one of the tool's backends, one
/// the Count cannot read, or the project's own registry settings for a
/// package backend, fails the lookup.
pub(crate) fn redirected(
    dep: &Dependency,
    configs: &[&str],
    files: &Files,
    feeds: &crate::nuget::Feeds,
) -> Option<String> {
    let dir = &dep.file[..dep.file.rfind('/').map_or(0, |i| i + 1)];
    let kinds = backends(&dep.package);
    for config in configs {
        let folder = &config[..config.rfind('/').map_or(0, |i| i + 1)];
        if !(dir.starts_with(folder) || folder.starts_with(dir)) {
            continue;
        }
        let Some(redirected) = files
            .get(*config)
            .and_then(|text| redirected_backends(text))
        else {
            return Some(format!("{config}, which the Count cannot read"));
        };
        if redirected.contains("*") || kinds.iter().any(|kind| redirected.contains(kind)) {
            return Some(format!(
                "settings in {config} that may move where mise installs it from"
            ));
        }
    }
    kinds
        .iter()
        .find_map(|kind| project_redirect(kind, dir, &dep.package, files, feeds))
}
