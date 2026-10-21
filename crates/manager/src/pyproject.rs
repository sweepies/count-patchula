//! `pyproject.toml` (PEP 621, PEP 735 groups, uv and Poetry tables) and
//! `.python-version`. `uv.lock` and `poetry.lock` resolve direct declarations.
use crate::*;
use toml_edit::{Document, Item, Value};

/// PEP 503 normalised project name.
fn normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
            continue;
        }
        if separator && !out.is_empty() {
            out.push('-');
        }
        separator = false;
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// The name and the byte range of the version specifiers in one PEP 508
/// requirement. URL requirements and unversioned names have no specifier.
fn pep508(requirement: &str) -> Option<(&str, usize, usize)> {
    let name_end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(requirement.len());
    let name = &requirement[..name_end];
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    let mut start = name_end;
    let skip = |at: usize| {
        at + requirement[at..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(requirement.len() - at)
    };
    start = skip(start);
    if requirement[start..].starts_with('[') {
        start += requirement[start..].find(']')? + 1;
        start = skip(start);
    }
    let parenthesised = requirement[start..].starts_with('(');
    if parenthesised {
        start = skip(start + 1);
    }
    let rest = &requirement[start..];
    if !rest.starts_with(['=', '!', '<', '>', '~']) {
        return None;
    }
    let mut end = start + rest.find([';', ')']).unwrap_or(rest.len());
    end = start + requirement[start..end].trim_end().len();
    (end > start).then_some((name, start, end))
}

fn resolved(lock: Option<&Document<&str>>, package: &str) -> Option<String> {
    lock?
        .get("package")?
        .as_array_of_tables()?
        .iter()
        .find(|p| {
            p.get("name")
                .and_then(Item::as_str)
                .map(normalise)
                .as_deref()
                == Some(package)
        })?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

struct Scan<'a> {
    path: &'a str,
    text: &'a str,
    lock: Option<Document<&'a str>>,
    out: Vec<Dependency>,
}

impl Scan<'_> {
    /// PEP 508 strings, edited inside the TOML string at the specifier only.
    fn requirements(&mut self, values: Option<&Item>, pointer: &str, kind: DependencyKind) {
        let Some(array) = values.and_then(Item::as_array) else {
            return;
        };
        for (index, value) in array.iter().enumerate() {
            let (Value::String(string), Some(span)) = (value, value.span()) else {
                continue;
            };
            let raw = &self.text[span.clone()];
            // Escapes and multiline strings would shift offsets; skip them.
            let quote = raw.chars().next();
            if !matches!(quote, Some('"' | '\''))
                || raw.starts_with("\"\"\"")
                || raw.starts_with("'''")
                || (quote == Some('"') && raw.contains('\\'))
            {
                continue;
            }
            let requirement = string.value();
            let Some((name, start, end)) = pep508(requirement) else {
                continue;
            };
            let offset = span.start + 1;
            let current = &requirement[start..end];
            let package = normalise(name);
            self.out.push(Dependency {
                manager: Manager::Pyproject,
                file: self.path.into(),
                location: Location::new(
                    self.text,
                    offset + start,
                    offset + end,
                    format!("{pointer}/{index}"),
                ),
                name: name.into(),
                resolved: resolved(self.lock.as_ref(), &package),
                package,
                kind,
                current: current.into(),
                digest: None,
                source: Source::Pypi,
                scheme: Scheme::Pep440,
                extract: None,
            });
        }
    }
    /// Poetry tables map names to a constraint string or a table with `version`.
    fn poetry(&mut self, table: Option<&Item>, pointer: &str, kind: DependencyKind) {
        let Some(table) = table.and_then(Item::as_table_like) else {
            return;
        };
        for (name, value) in table.iter() {
            if name == "python" {
                continue;
            }
            let version = if value.is_str() {
                Some(value)
            } else {
                if ["git", "path", "url", "file"]
                    .iter()
                    .any(|key| value.get(key).is_some())
                {
                    continue;
                }
                value.get("version")
            };
            let (Some(version), Some(current)) = (version, version.and_then(Item::as_str)) else {
                continue;
            };
            let Some(span) = version.span() else {
                continue;
            };
            if current.trim() == "*" {
                continue;
            }
            let package = normalise(name);
            self.out.push(Dependency {
                manager: Manager::Pyproject,
                file: self.path.into(),
                location: Location::new(
                    self.text,
                    span.start,
                    span.end,
                    format!("{pointer}/{name}"),
                ),
                name: name.into(),
                resolved: resolved(self.lock.as_ref(), &package),
                package,
                kind,
                current: current.into(),
                digest: None,
                source: Source::Pypi,
                scheme: Scheme::Poetry,
                extract: None,
            });
        }
    }
}

pub(super) fn extract(path: &str, text: &str, files: &Files) -> Result<Vec<Dependency>, String> {
    if path.ends_with(".python-version") {
        return Ok(python_version(path, text));
    }
    if !path.ends_with("pyproject.toml") {
        return Ok(Vec::new());
    }
    let doc = Document::parse(text).map_err(|e| e.to_string())?;
    // The nearest lockfile beside or above the manifest resolves it; uv
    // workspaces keep one lock at the workspace root.
    let base = path
        .strip_suffix("pyproject.toml")
        .expect("manifest suffix");
    let lock = files
        .iter()
        .filter(|(p, _)| {
            ["uv.lock", "poetry.lock"].iter().any(|leaf| {
                p.strip_suffix(leaf)
                    .is_some_and(|dir| base.starts_with(dir))
            })
        })
        .max_by_key(|(p, _)| p.len())
        .map(|(_, t)| Document::parse(t.as_str()).map_err(|e| e.to_string()))
        .transpose()?;
    let mut scan = Scan {
        path,
        text,
        lock,
        out: Vec::new(),
    };
    let project = doc.get("project");
    scan.requirements(
        project.and_then(|p| p.get("dependencies")),
        "/project/dependencies",
        DependencyKind::Production,
    );
    if let Some(extras) = project
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(Item::as_table_like)
    {
        for (extra, values) in extras.iter() {
            scan.requirements(
                Some(values),
                &format!("/project/optional-dependencies/{extra}"),
                DependencyKind::Optional,
            );
        }
    }
    if let Some(groups) = doc.get("dependency-groups").and_then(Item::as_table_like) {
        for (group, values) in groups.iter() {
            scan.requirements(
                Some(values),
                &format!("/dependency-groups/{group}"),
                DependencyKind::Development,
            );
        }
    }
    let tool = doc.get("tool");
    let uv = tool.and_then(|t| t.get("uv"));
    for (key, kind) in [
        ("dev-dependencies", DependencyKind::Development),
        ("constraint-dependencies", DependencyKind::Production),
        ("override-dependencies", DependencyKind::Production),
    ] {
        scan.requirements(
            uv.and_then(|u| u.get(key)),
            &format!("/tool/uv/{key}"),
            kind,
        );
    }
    let poetry = tool.and_then(|t| t.get("poetry"));
    scan.poetry(
        poetry.and_then(|p| p.get("dependencies")),
        "/tool/poetry/dependencies",
        DependencyKind::Production,
    );
    scan.poetry(
        poetry.and_then(|p| p.get("dev-dependencies")),
        "/tool/poetry/dev-dependencies",
        DependencyKind::Development,
    );
    if let Some(groups) = poetry
        .and_then(|p| p.get("group"))
        .and_then(Item::as_table_like)
    {
        for (group, table) in groups.iter() {
            scan.poetry(
                table.get("dependencies"),
                &format!("/tool/poetry/group/{group}/dependencies"),
                DependencyKind::Development,
            );
        }
    }
    Ok(scan.out)
}

/// `.python-version` pins CPython with a numeric version; names such as
/// `pypy3.10` or `system` are not CPython versions.
fn python_version(path: &str, text: &str) -> Vec<Dependency> {
    let Some(token) = text.split_whitespace().next() else {
        return Vec::new();
    };
    if !token
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Vec::new();
    }
    let start = text.find(token).expect("token from text");
    let resolution = patchula_mise::resolve("python");
    let (source, package, extract) = resolution.map_or_else(
        || (Source::Mise, "python".into(), None),
        |r| (r.source, r.package, r.extract),
    );
    vec![Dependency {
        manager: Manager::Pyproject,
        file: path.into(),
        location: Location::new(text, start, start + token.len(), "/python".into()),
        name: "python".into(),
        package,
        kind: DependencyKind::Toolchain,
        current: token.into(),
        resolved: Some(token.into()),
        digest: None,
        source,
        scheme: Scheme::Loose,
        extract,
    }]
}
