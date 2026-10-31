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
    let configuration = |path: &str| {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        leaf.starts_with('.') || matches!(leaf, "mise.toml" | "mise.local.toml")
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
        });
    }
}
