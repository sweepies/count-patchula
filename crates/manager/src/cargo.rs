use crate::*;
use rustovate_versioning::{Ecosystem, Versioning};
use toml_edit::{Document, Item};

pub(super) fn documents(files: &Files) -> Result<BTreeMap<&str, Document<&str>>, String> {
    files
        .iter()
        .filter(|(p, _)| p.ends_with("Cargo.toml") || p.ends_with("Cargo.lock"))
        .map(|(p, t)| {
            Ok((
                p.as_str(),
                Document::parse(t.as_str()).map_err(|e| format!("{p}: {e}"))?,
            ))
        })
        .collect()
}
pub(super) fn extract(
    path: &str,
    text: &str,
    files: &BTreeMap<&str, Document<&str>>,
) -> Result<Vec<Dependency>, String> {
    if !path.ends_with("Cargo.toml") {
        return Ok(Vec::new());
    }
    let doc = files.get(path).expect("selected manifest parsed once");
    let base = path.strip_suffix("Cargo.toml").expect("manifest suffix");
    let mut workspace = BTreeMap::new();
    // The nearest ancestor workspace supplies inherited declarations. Their
    // editable version location belongs to the workspace root, scanned once.
    for (_, root) in files
        .iter()
        .filter(|(p, _)| {
            p.ends_with("Cargo.toml") && base.starts_with(p.strip_suffix("Cargo.toml").unwrap())
        })
        .rev()
    {
        if let Some(deps) = root
            .get("workspace")
            .and_then(|v| v.get("dependencies"))
            .and_then(Item::as_table_like)
        {
            workspace.extend(deps.iter().map(|(k, v)| (k.to_owned(), v.clone())));
            break;
        }
    }
    let mut out = Vec::new();
    walk(path, text, doc.as_table(), "", &workspace, &mut out);
    // A lockfile resolves direct manifest declarations only; never emit its
    // transitive packages as independent dependencies.
    let lock = files
        .iter()
        .filter(|(p, _)| {
            p.ends_with("Cargo.lock") && base.starts_with(p.strip_suffix("Cargo.lock").unwrap())
        })
        .max_by_key(|(p, _)| p.len());
    if let Some((_, lock)) = lock
        && let Some(packages) = lock.get("package").and_then(Item::as_array_of_tables)
    {
        for dep in &mut out {
            let mut candidates: Vec<_> = packages
                .iter()
                .filter(|p| p.get("name").and_then(Item::as_str) == Some(&dep.package))
                .filter(|p| {
                    p.get("source").and_then(Item::as_str).is_some_and(|s| {
                        s.starts_with("registry+https://github.com/rust-lang/crates.io-index")
                            || s.starts_with("sparse+https://index.crates.io/")
                    })
                })
                .filter_map(|p| p.get("version").and_then(Item::as_str))
                .filter_map(|v| Some((Ecosystem::Cargo.parse(v).ok()?, v)))
                .filter(|(v, _)| Ecosystem::Cargo.matches(&dep.current, v).unwrap_or(false))
                .collect();
            candidates.sort_by(|a, b| Ecosystem::Cargo.order(&a.0, &b.0));
            dep.resolved = candidates.last().map(|(_, v)| (*v).into());
        }
    }
    Ok(out)
}
fn walk(
    path: &str,
    text: &str,
    table: &dyn toml_edit::TableLike,
    pointer: &str,
    workspace: &BTreeMap<String, Item>,
    out: &mut Vec<Dependency>,
) {
    for (key, item) in table.iter() {
        let next = format!("{pointer}/{key}");
        if matches!(
            key,
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) {
            let Some(deps) = item.as_table_like() else {
                continue;
            };
            for (name, value) in deps.iter() {
                let inherited = value.get("workspace").and_then(Item::as_bool) == Some(true);
                let location = if inherited {
                    value.get("workspace").and_then(Item::span)
                } else {
                    None
                };
                let value = if inherited {
                    workspace.get(name).unwrap_or(value)
                } else {
                    value
                };
                if value.get("git").is_some() || value.get("registry").is_some() {
                    continue;
                }
                let version = if value.is_str() {
                    Some(value)
                } else {
                    value.get("version")
                };
                let Some(version) = version else {
                    continue;
                };
                let Some(current) = version.as_str() else {
                    continue;
                };
                let Some(span) = location.or_else(|| version.span()) else {
                    continue;
                };
                let package = value.get("package").and_then(Item::as_str).unwrap_or(name);
                out.push(Dependency {
                    manager: Manager::Cargo,
                    file: path.into(),
                    location: Location::new(text, span.start, span.end, format!("{next}/{name}")),
                    name: name.into(),
                    package: package.into(),
                    kind: match key {
                        "dev-dependencies" => DependencyKind::Development,
                        "build-dependencies" => DependencyKind::Build,
                        _ => DependencyKind::Production,
                    },
                    current: current.into(),
                    resolved: None,
                    digest: None,
                    source: Source::Crates,
                    scheme: Scheme::Semver,
                    extract: None,
                });
            }
        } else if let Some(table) = item.as_table_like() {
            walk(path, text, table, &next, workspace, out);
        }
    }
}
