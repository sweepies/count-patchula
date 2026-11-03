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

/// Where a lockfile says `package` came from. `uv.lock` records
/// `source.registry` for an index and other keys (`git`, `path`, `url`,
/// `editable`, `directory`, `virtual`) for anything else; `poetry.lock`
/// records `[package.source]` with a `type`, and nothing for PyPI. Entries
/// that disagree, as a forked uv resolution can, are ambiguous.
fn locked_origin(lock: Option<&Document<&str>>, package: &str) -> Option<crate::pypi::Origin> {
    use crate::pypi::{Named, Origin};
    let entries = lock?.get("package")?.as_array_of_tables()?;
    let origins = entries
        .iter()
        .filter(|p| {
            p.get("name")
                .and_then(Item::as_str)
                .map(normalise)
                .as_deref()
                == Some(package)
        })
        .map(|entry| {
            let Some(source) = entry.get("source") else {
                // Poetry records no source for PyPI.
                return Origin::Index(Named::Public);
            };
            if let Some(registry) = source.get("registry").and_then(Item::as_str) {
                return Origin::Index(Named::parse(registry));
            }
            match source.get("type").and_then(Item::as_str) {
                Some("legacy" | "pypi") => source
                    .get("url")
                    .and_then(Item::as_str)
                    .map_or(Origin::Index(Named::Public), |url| {
                        Origin::Index(Named::parse(url))
                    }),
                _ => Origin::Other,
            }
        });
    Origin::of(origins)
}

/// Where a uv source or Poetry dependency pins a package: to a named index,
/// to several (marker-split lists), or to something that is not an index.
fn pin(value: &Item, names: &BTreeMap<String, crate::pypi::Named>) -> Option<crate::pypi::Origin> {
    use crate::pypi::{Named, Origin};
    let one = |value: &dyn toml_edit::TableLike| -> Option<Origin> {
        if let Some(name) = value
            .get("index")
            .or_else(|| value.get("source"))
            .and_then(Item::as_str)
        {
            // A pin to an index this project does not define cannot be read.
            return Some(Origin::Index(
                names
                    .get(&name.to_ascii_lowercase())
                    .cloned()
                    .unwrap_or_else(|| Named::Unusable(format!("index {name}"))),
            ));
        }
        ["git", "path", "url", "workspace", "file", "directory"]
            .iter()
            .any(|key| value.get(key).is_some())
            .then_some(Origin::Other)
    };
    if let Some(table) = value.as_table_like() {
        return one(table);
    }
    let list = value.as_array()?;
    Origin::of(
        list.iter()
            .filter_map(|v| v.as_inline_table())
            .filter_map(|table| one(table)),
    )
}

/// The indexes uv and Poetry configure, and the packages pinned by them.
/// `settings` is where uv reads its index settings: the nearest `uv.toml`, or
/// the nearest `[tool.uv]`. `projects` are the `[tool.uv]` tables of the
/// project and its workspace root, nearest first, for index names and
/// `[tool.uv.sources]`.
fn indexes(
    settings: Option<&Item>,
    projects: &[&Item],
    poetry: Option<&Item>,
) -> crate::pypi::Indexes {
    use crate::pypi::Named;
    let mut indexes = crate::pypi::Indexes::default();
    let mut names = BTreeMap::<String, Named>::new();
    let uv_indexes = |item: Option<&Item>| -> Vec<(Option<String>, Named, bool, bool)> {
        item.and_then(|u| u.get("index"))
            .and_then(Item::as_array_of_tables)
            .into_iter()
            .flatten()
            .filter_map(|index| {
                let url = index.get("url").and_then(Item::as_str)?;
                let flag = |key: &str| index.get(key).and_then(Item::as_bool) == Some(true);
                Some((
                    index
                        .get("name")
                        .and_then(Item::as_str)
                        .map(str::to_ascii_lowercase),
                    Named::parse(url),
                    flag("default"),
                    flag("explicit"),
                ))
            })
            .collect()
    };
    // Names from every table that can define them; the nearest wins.
    for item in projects.iter().rev().copied().map(Some).chain([settings]) {
        for (name, named, _, _) in uv_indexes(item) {
            if let Some(name) = name {
                names.insert(name, named);
            }
        }
    }
    for (_, named, default, explicit) in uv_indexes(settings) {
        if default {
            indexes.default = Some(named);
        } else if !explicit {
            indexes.extras.push(named);
        }
    }
    if let Some(url) = settings
        .and_then(|u| u.get("index-url"))
        .and_then(Item::as_str)
    {
        indexes.default = Some(Named::parse(url));
    }
    for url in settings
        .and_then(|u| u.get("extra-index-url"))
        .and_then(Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
    {
        indexes.extras.push(Named::parse(url));
    }
    // uv sources: the workspace root's, overridden by the project's.
    for project in projects.iter().rev() {
        for (package, value) in project
            .get("sources")
            .and_then(Item::as_table_like)
            .into_iter()
            .flat_map(|t| t.iter())
        {
            if let Some(origin) = pin(value, &names) {
                indexes.pinned.insert(normalise(package), origin);
            }
        }
    }
    // Poetry: sources by priority; names are case-insensitive, and a source
    // without a URL named PyPI is PyPI.
    let mut poetry_names = BTreeMap::<String, Named>::new();
    let mut primaries = Vec::new();
    for source in poetry
        .and_then(|p| p.get("source"))
        .and_then(Item::as_array_of_tables)
        .into_iter()
        .flatten()
    {
        let name = source
            .get("name")
            .and_then(Item::as_str)
            .map(str::to_ascii_lowercase);
        let named = match source.get("url").and_then(Item::as_str) {
            Some(url) => Named::parse(url),
            None if name.as_deref() == Some("pypi") => Named::Public,
            None => continue,
        };
        if let Some(name) = name {
            poetry_names.insert(name, named.clone());
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
        if value.is_str() {
            continue;
        }
        if let Some(origin) = pin(value, &poetry_names) {
            indexes.pinned.insert(normalise(package), origin);
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
    let base = path
        .strip_suffix("pyproject.toml")
        .expect("manifest suffix");
    // The project's directory and those above it, nearest first.
    let join = |dir: &str, leaf: &str| {
        if dir.is_empty() {
            leaf.to_owned()
        } else {
            format!("{dir}/{leaf}")
        }
    };
    let mut ancestors = vec![base.trim_end_matches('/')];
    while let Some(&dir) = ancestors.last()
        && !dir.is_empty()
    {
        ancestors.push(dir.rsplit_once('/').map_or("", |(parent, _)| parent));
    }
    let pyprojects: Vec<(&str, Document<&str>)> = ancestors
        .iter()
        .filter_map(|dir| {
            let text = files.get(&join(dir, "pyproject.toml"))?;
            Some((*dir, Document::parse(text.as_str()).ok()?))
        })
        .collect();
    // A uv workspace root shares its lock, sources and indexes with members.
    let root = pyprojects.iter().find(|(_, doc)| {
        doc.get("tool")
            .and_then(|t| t.get("uv"))
            .and_then(|u| u.get("workspace"))
            .is_some()
    });
    // Only the project's own lock, or its workspace root's, resolves it: a
    // lock above belongs to another project.
    let lock = std::iter::once(ancestors[0])
        .chain(root.map(|(dir, _)| *dir))
        .flat_map(|dir| ["uv.lock", "poetry.lock"].map(|leaf| join(dir, leaf)))
        .find_map(|path| files.get(&path))
        .map(|text| Document::parse(text.as_str()).map_err(|e| e.to_string()))
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
    // uv reads its index settings from the nearest `uv.toml`, or the nearest
    // `pyproject.toml` with `[tool.uv]`, at or above the project.
    let uv_tomls: Vec<(&str, Document<&str>)> = ancestors
        .iter()
        .filter_map(|dir| {
            let text = files.get(&join(dir, "uv.toml"))?;
            Some((*dir, Document::parse(text.as_str()).ok()?))
        })
        .collect();
    let uv_table = |doc: &Document<&str>| doc.get("tool").and_then(|t| t.get("uv")).cloned();
    let settings: Option<Item> = ancestors.iter().find_map(|dir| {
        if let Some((_, doc)) = uv_tomls.iter().find(|(d, _)| d == dir) {
            return Some(doc.as_item().clone());
        }
        pyprojects
            .iter()
            .find(|(d, _)| d == dir)
            .and_then(|(_, doc)| uv_table(doc))
    });
    let mut projects: Vec<Item> = tool
        .and_then(|t| t.get("uv"))
        .cloned()
        .into_iter()
        .collect();
    if let Some((dir, doc)) = root
        && *dir != ancestors[0]
    {
        projects.extend(uv_table(doc));
    }
    let projects: Vec<&Item> = projects.iter().collect();
    let indexes = indexes(
        settings.as_ref(),
        &projects,
        tool.and_then(|t| t.get("poetry")),
    );
    let lock = scan.lock.as_ref();
    indexes.assign(&mut scan.out, |package| locked_origin(lock, package));
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
