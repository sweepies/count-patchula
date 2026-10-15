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
    let (source, package) = if let Some(p) = name.strip_prefix("npm:") {
        (Source::Npm, p)
    } else if let Some(p) = name
        .strip_prefix("github:")
        .or_else(|| name.strip_prefix("aqua:"))
    {
        (Source::GithubReleases, p)
    } else {
        match name {
            "bun" => (Source::GithubReleases, "oven-sh/bun"),
            "node" => (Source::GithubTags, "nodejs/node"),
            "rust" => (Source::GithubTags, "rust-lang/rust"),
            "python" => (Source::GithubTags, "python/cpython"),
            "go" => (Source::GithubTags, "golang/go"),
            "zig" => (Source::GithubTags, "ziglang/zig"),
            "mise" => (Source::GithubReleases, "jdx/mise"),
            "gh" => (Source::GithubReleases, "cli/cli"),
            "fnox" => (Source::GithubReleases, "jdx/fnox"),
            "tak" => (Source::GithubReleases, "jdx/tak"),
            "hk" => (Source::GithubReleases, "jdx/hk"),
            "prettier" => (Source::Npm, "prettier"),
            "usage" => (Source::GithubReleases, "jdx/usage"),
            "mr-boxington" => (Source::GithubReleases, "jdx/mr-boxington"),
            "terraform" => (Source::GithubReleases, "hashicorp/terraform"),
            "opentofu" => (Source::GithubReleases, "opentofu/opentofu"),
            _ => (Source::Mise, name),
        }
    };
    out.push(Dependency {
        manager: Manager::Mise,
        file: path.into(),
        location: Location::new(text, start, end, format!("/tools/{name}")),
        name: name.into(),
        package: if matches!(source, Source::GithubReleases | Source::GithubTags) {
            package.split('/').take(2).collect::<Vec<_>>().join("/")
        } else {
            package.into()
        },
        kind: DependencyKind::Tool,
        current: version.into(),
        resolved: None,
        digest: None,
        source,
        scheme: Scheme::Loose,
        extract: if name == "go" {
            Some("^go(?<version>.*)$".into())
        } else {
            None
        },
    });
}
