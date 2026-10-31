use crate::*;
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

pub(super) fn extract(
    path: &str,
    text: &str,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let mut out = Vec::new();
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
                        add(
                            path,
                            text,
                            "",
                            name,
                            version,
                            offset + start..offset + start + version.len(),
                            &mut out,
                            skipped,
                        );
                        search = start + version.len();
                    }
                }
            }
            offset += line.len();
        }
    } else {
        let doc = Document::parse(text).map_err(|e| e.to_string())?;
        if let Some(tools) = doc.get("tools").and_then(|v| v.as_table_like()) {
            tools_in(path, text, tools, "", &mut out, skipped);
        }
        // A task's own tools: `[tasks.<name>]` in a config file, and a
        // top-level `[<name>]` in an included task file, which is written
        // without the `tasks.` prefix.
        if let Some(tasks) = doc.get("tasks").and_then(|v| v.as_table_like()) {
            for (task, item) in tasks.iter() {
                task_tools(
                    path,
                    text,
                    item,
                    &format!("/tasks/{task}"),
                    &mut out,
                    skipped,
                );
            }
        }
        for (task, item) in doc.iter().filter(|(k, _)| !matches!(*k, "tools" | "tasks")) {
            task_tools(path, text, item, &format!("/{task}"), &mut out, skipped);
        }
    }
    Ok(out)
}
fn task_tools(
    path: &str,
    text: &str,
    task: &toml_edit::Item,
    prefix: &str,
    out: &mut Vec<Dependency>,
    skipped: &mut Vec<Skipped>,
) {
    if let Some(tools) = task.get("tools").and_then(|v| v.as_table_like()) {
        tools_in(path, text, tools, prefix, out, skipped);
    }
}
fn tools_in(
    path: &str,
    text: &str,
    tools: &dyn toml_edit::TableLike,
    prefix: &str,
    out: &mut Vec<Dependency>,
    skipped: &mut Vec<Skipped>,
) {
    for (name, item) in tools.iter() {
        if let Some(value) = item.as_value() {
            collect(path, text, prefix, name, value, out, skipped);
        } else if let Some(value) = item.get("version").and_then(|v| v.as_value()) {
            collect(path, text, prefix, name, value, out, skipped);
        }
    }
}
fn collect(
    path: &str,
    text: &str,
    prefix: &str,
    name: &str,
    value: &Value,
    out: &mut Vec<Dependency>,
    skipped: &mut Vec<Skipped>,
) {
    match value {
        Value::String(v) => {
            let span = value.span().expect("immutable parser retains spans");
            add(path, text, prefix, name, v.value(), span, out, skipped);
        }
        Value::Array(values) => {
            for value in values.iter() {
                collect(path, text, prefix, name, value, out, skipped);
            }
        }
        Value::InlineTable(table) => {
            if let Some(value) = table.get("version") {
                collect(path, text, prefix, name, value, out, skipped);
            }
        }
        _ => {}
    }
}
#[allow(clippy::too_many_arguments)]
fn add(
    path: &str,
    text: &str,
    prefix: &str,
    name: &str,
    version: &str,
    span: std::ops::Range<usize>,
    out: &mut Vec<Dependency>,
    skipped: &mut Vec<Skipped>,
) {
    let (start, end) = (span.start, span.end);
    if ["path:", "ref:", "prefix:", "system"]
        .iter()
        .any(|p| version.starts_with(p))
    {
        return;
    }
    if let Some((_, reason)) = UNLISTED.iter().find(|(p, _)| name.starts_with(p)) {
        skipped.push(Skipped {
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
    out.push(Dependency {
        manager: Manager::Mise,
        file: path.into(),
        location: Location::new(text, start, end, format!("{prefix}/tools/{name}")),
        name: name.into(),
        package: name.into(),
        kind: DependencyKind::Tool,
        current: version.into(),
        resolved,
        digest: None,
        source: Source::Mise,
        scheme: Scheme::Loose,
        extract: None,
    });
}
