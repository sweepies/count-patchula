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
            .unwrap_or_else(|| requirement.len() - at)
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

/// The index a lockfile records for `package`: `source.registry` in
/// `uv.lock`, `[package.source].url` in `poetry.lock`, where a package without
/// one came from PyPI.
fn locked_index(lock: Option<&Document<&str>>, package: &str) -> Option<crate::pypi::Named> {
    let entry = lock?
        .get("package")?
        .as_array_of_tables()?
        .iter()
        .find(|p| {
            p.get("name")
                .and_then(Item::as_str)
                .map(normalise)
                .as_deref()
                == Some(package)
        })?;
    let source = entry.get("source");
    if let Some(registry) = source
        .and_then(|s| s.get("registry"))
        .and_then(Item::as_str)
    {
        return Some(crate::pypi::Named::parse(registry));
    }
    match source.and_then(|s| s.get("url")).and_then(Item::as_str) {
        Some(url) => Some(crate::pypi::Named::parse(url)),
        // Poetry records no source for PyPI; uv always records one.
        None if source.is_none() && entry.get("files").is_some() => {
            Some(crate::pypi::Named::Public)
        }
        None => None,
    }
}

/// The indexes uv and Poetry configure, and the packages pinned to one. `uv` is
/// a `uv.toml`, or `[tool.uv]` without one.
fn indexes(uv: Option<&Item>, poetry: Option<&Item>) -> crate::pypi::Indexes {
    use crate::pypi::Named;
    let mut indexes = crate::pypi::Indexes::default();
    let mut names = BTreeMap::<String, Named>::new();
    for index in uv
        .and_then(|u| u.get("index"))
        .and_then(Item::as_array_of_tables)
        .into_iter()
        .flatten()
    {
        let Some(url) = index.get("url").and_then(Item::as_str) else {
            continue;
        };
        let named = Named::parse(url);
        if let Some(name) = index.get("name").and_then(Item::as_str) {
            names.insert(name.to_owned(), named.clone());
        }
        let flag = |key: &str| index.get(key).and_then(Item::as_bool) == Some(true);
        if flag("default") {
            indexes.default = Some(named);
        } else if !flag("explicit") {
            indexes.extras.push(named);
        }
    }
    if let Some(url) = uv.and_then(|u| u.get("index-url")).and_then(Item::as_str) {
        indexes.default = Some(Named::parse(url));
    }
    for url in uv
        .and_then(|u| u.get("extra-index-url"))
        .and_then(Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
    {
        indexes.extras.push(Named::parse(url));
    }
    let mut primaries = Vec::new();
    for source in poetry
        .and_then(|p| p.get("source"))
        .and_then(Item::as_array_of_tables)
        .into_iter()
        .flatten()
    {
        let Some(url) = source.get("url").and_then(Item::as_str) else {
            continue;
        };
        let named = Named::parse(url);
        if let Some(name) = source.get("name").and_then(Item::as_str) {
            names.insert(name.to_owned(), named.clone());
        }
        let legacy = |key: &str| source.get(key).and_then(Item::as_bool) == Some(true);
        let priority =
            source
                .get("priority")
                .and_then(Item::as_str)
                .unwrap_or(if legacy("default") {
                    "default"
                } else if legacy("secondary") {
                    "supplemental"
                } else {
                    "primary"
                });
        match priority {
            "explicit" => {}
            "supplemental" | "secondary" => indexes.extras.push(named),
            "default" => indexes.default = Some(named),
            _ => primaries.push(named),
        }
    }
    // Primary sources replace PyPI, in order; more than one is ambiguous.
    let mut primaries = primaries.into_iter();
    if indexes.default.is_none() {
        indexes.default = primaries.next();
    }
    indexes.extras.extend(primaries);
    // Pins: uv sources by index name, and Poetry dependencies' `source`.
    for (package, value) in uv
        .and_then(|u| u.get("sources"))
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|t| t.iter())
    {
        if let Some(named) = value
            .get("index")
            .and_then(Item::as_str)
            .and_then(|name| names.get(name))
        {
            indexes.pinned.insert(normalise(package), named.clone());
        }
    }
    let mut tables: Vec<&Item> = Vec::new();
    for key in ["dependencies", "dev-dependencies"] {
        tables.extend(poetry.and_then(|p| p.get(key)));
    }
    for (_, group) in poetry
        .and_then(|p| p.get("group"))
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|t| t.iter())
    {
        tables.extend(group.get("dependencies"));
    }
    for (package, value) in tables
        .into_iter()
        .filter_map(Item::as_table_like)
        .flat_map(|t| t.iter())
    {
        if let Some(named) = value
            .get("source")
            .and_then(Item::as_str)
            .and_then(|name| names.get(name))
        {
            indexes.pinned.insert(normalise(package), named.clone());
        }
    }
    indexes
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
                included_by: None,
                registry: None,
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
                included_by: None,
                registry: None,
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
    // uv reads the nearest `uv.toml` at or above the project instead of
    // `[tool.uv]` when there is one.
    let uv_toml = files
        .iter()
        .filter(|(p, _)| {
            p.strip_suffix("uv.toml")
                .is_some_and(|dir| (dir.is_empty() || dir.ends_with('/')) && base.starts_with(dir))
        })
        .max_by_key(|(p, _)| p.len())
        .map(|(_, t)| Document::parse(t.as_str()).map_err(|e| e.to_string()))
        .transpose()?;
    let indexes = indexes(
        uv_toml
            .as_ref()
            .map(Document::as_item)
            .or_else(|| tool.and_then(|t| t.get("uv"))),
        tool.and_then(|t| t.get("poetry")),
    );
    let lock = scan.lock.as_ref();
    indexes.assign(&mut scan.out, |package| locked_index(lock, package));
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
    vec![Dependency {
        manager: Manager::Pyproject,
        file: path.into(),
        location: Location::new(text, start, start + token.len(), "/python".into()),
        name: "python".into(),
        package: "python".into(),
        kind: DependencyKind::Toolchain,
        current: token.into(),
        resolved: Some(token.into()),
        digest: None,
        source: Source::Mise,
        scheme: Scheme::Loose,
        extract: None,
        included_by: None,
        registry: None,
    }]
}
