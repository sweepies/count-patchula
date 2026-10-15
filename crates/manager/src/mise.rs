use crate::*;
use toml_edit::{Document, Value};

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
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
                            name,
                            version,
                            offset + start,
                            offset + start + version.len(),
                            &mut out,
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
            for (name, item) in tools.iter() {
                if let Some(value) = item.as_value() {
                    collect(path, text, name, value, &mut out);
                } else if let Some(value) = item.get("version").and_then(|v| v.as_value()) {
                    collect(path, text, name, value, &mut out);
                }
            }
        }
    }
    Ok(out)
}
fn collect(path: &str, text: &str, name: &str, value: &Value, out: &mut Vec<Dependency>) {
    match value {
        Value::String(v) => {
            let span = value.span().expect("immutable parser retains spans");
            add(path, text, name, v.value(), span.start, span.end, out);
        }
        Value::Array(values) => {
            for value in values.iter() {
                collect(path, text, name, value, out);
            }
        }
        Value::InlineTable(table) => {
            if let Some(value) = table.get("version") {
                collect(path, text, name, value, out);
            }
        }
        _ => {}
    }
}
fn add(
    path: &str,
    text: &str,
    name: &str,
    version: &str,
    start: usize,
    end: usize,
    out: &mut Vec<Dependency>,
) {
    if ["path:", "ref:", "prefix:", "system"]
        .iter()
        .any(|p| version.starts_with(p))
    {
        return;
    }
    let version = version.strip_prefix("tag:").unwrap_or(version);
    let resolution = rustovate_mise::resolve(name);
    let (source, package, extract) = resolution.map_or_else(
        || (Source::Mise, name.into(), None),
        |r| (r.source, r.package, r.extract),
    );
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
        location: Location::new(text, start, end, format!("/tools/{name}")),
        name: name.into(),
        package,
        kind: DependencyKind::Tool,
        current: version.into(),
        resolved,
        digest: None,
        source,
        scheme: Scheme::Loose,
        extract,
    });
}
