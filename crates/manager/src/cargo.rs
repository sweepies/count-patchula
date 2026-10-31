use crate::*;
use patchula_versioning::{Ecosystem, Versioning};
use std::collections::BTreeSet;
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
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    if !path.ends_with("Cargo.toml") {
        return Ok(Vec::new());
    }
    let doc = files.get(path).expect("selected manifest parsed once");
    let base = path.strip_suffix("Cargo.toml").expect("manifest suffix");
    let mut workspace = BTreeMap::new();
    // The nearest ancestor workspace supplies inherited declarations. Their
    // editable version location belongs to the workspace root, scanned once.
    if let Some(deps) = files
        .iter()
        .filter(|(p, _)| {
            p.ends_with("Cargo.toml") && base.starts_with(p.strip_suffix("Cargo.toml").unwrap())
        })
        .filter_map(|(p, root)| {
            root.get("workspace")
                .and_then(|v| v.get("dependencies"))
                .and_then(Item::as_table_like)
                .map(|deps| (p.len(), deps))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, deps)| deps)
    {
        workspace.extend(deps.iter().map(|(k, v)| (k.to_owned(), v.clone())));
    }
    let mut out = Vec::new();
    walk(
        path,
        text,
        doc.as_table(),
        "",
        &workspace,
        &mut out,
        skipped,
    );
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
        // The lock records what a package depends on. Only the dependency
        // edges of the packages that declare this manifest's dependencies say
        // which locked version is the declaration's: the same crate may also be
        // locked for a transitive dependent, or a path dependency outside the
        // workspace, at another version a wide requirement would match too.
        let declaring = declaring_packages(files, doc, base);
        let members: Vec<_> = packages
            .iter()
            .filter(|p| p.get("source").is_none())
            .filter(|p| named(p).is_some_and(|name| declaring.contains(name)))
            .collect();
        for dep in &mut out {
            let mut candidates: Vec<_> = members
                .iter()
                .filter_map(|member| member.get("dependencies")?.as_array())
                .flatten()
                .filter_map(|edge| registry_edge(packages, edge.as_str()?, &dep.package))
                .filter_map(|v| Some((Ecosystem::Cargo.parse(v).ok()?, v)))
                .filter(|(v, _)| Ecosystem::Cargo.matches(&dep.current, v).unwrap_or(false))
                .collect();
            candidates.sort_by(|a, b| Ecosystem::Cargo.order(&a.0, &b.0));
            dep.resolved = candidates.last().map(|(_, v)| (*v).into());
        }
    }
    Ok(out)
}
/// The names of the lock packages whose dependency edges belong to this
/// manifest's declarations: its own package, and for a workspace root also
/// every member. A path dependency that is not a member is not one of them.
fn declaring_packages<'a>(
    files: &'a BTreeMap<&str, Document<&str>>,
    doc: &'a Document<&str>,
    base: &str,
) -> BTreeSet<&'a str> {
    let package = |doc: &'a Document<&str>| {
        doc.get("package")
            .and_then(|p| p.get("name"))
            .and_then(Item::as_str)
    };
    let mut names: BTreeSet<&str> = package(doc).into_iter().collect();
    let Some(workspace) = doc.get("workspace") else {
        return names;
    };
    let patterns = |key: &str| -> Vec<&str> {
        workspace
            .get(key)
            .and_then(Item::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect()
    };
    // Cargo's `**` also matches the directory it follows, so `crates/**` covers
    // `crates` itself; globset's needs one more component, so that directory is
    // matched on its own too.
    let compile = |key: &str| -> Vec<globset::GlobMatcher> {
        patterns(key)
            .into_iter()
            .flat_map(|pattern| {
                let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
                [Some(pattern), pattern.strip_suffix("/**")]
            })
            .flatten()
            .filter_map(glob)
            .collect()
    };
    let (members, excluded) = (compile("members"), compile("exclude"));
    for (path, member) in files {
        let Some(directory) = path
            .strip_prefix(base)
            .and_then(|rest| rest.strip_suffix("/Cargo.toml"))
        else {
            continue;
        };
        if members.iter().any(|pattern| pattern.is_match(directory))
            && !excluded.iter().any(|pattern| pattern.is_match(directory))
            && let Some(name) = package(member)
        {
            names.insert(name);
        }
    }
    names
}
/// Cargo's member globs, which it reads as paths: `*` and `?` stop at a `/`, `**`
/// crosses directories, and `[abc]` picks a character. globset compiles them to
/// an automaton, so a hostile pattern in `members` costs time linear in the
/// path, not exponential. A pattern that does not compile matches nothing.
fn glob(pattern: &str) -> Option<globset::GlobMatcher> {
    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher())
}
/// The locked version a dependency edge names, when it is a crates.io release
/// of `package`. An edge is `name`, `name version` or `name version (source)`;
/// the source is kept, because a git dependency can be the same crate at the
/// same version.
fn registry_edge<'a>(
    packages: &'a toml_edit::ArrayOfTables,
    edge: &str,
    package: &str,
) -> Option<&'a str> {
    let mut words = edge.split_whitespace();
    if words.next()? != package {
        return None;
    }
    let version = words.next();
    let source = words.next().map(|s| s.trim_matches(['(', ')']));
    packages
        .iter()
        .filter(|p| named(p) == Some(package) && registry(p))
        .filter(|p| version.is_none_or(|v| p.get("version").and_then(Item::as_str) == Some(v)))
        .filter(|p| source.is_none_or(|s| p.get("source").and_then(Item::as_str) == Some(s)))
        .find_map(|p| p.get("version")?.as_str())
}
/// A lock entry's crate name.
fn named(p: &toml_edit::Table) -> Option<&str> {
    p.get("name").and_then(Item::as_str)
}
/// Whether a lock entry comes from the crates.io registry.
fn registry(p: &toml_edit::Table) -> bool {
    p.get("source").and_then(Item::as_str).is_some_and(|s| {
        s.starts_with("registry+https://github.com/rust-lang/crates.io-index")
            || s.starts_with("sparse+https://index.crates.io/")
    })
}
fn walk(
    path: &str,
    text: &str,
    table: &dyn toml_edit::TableLike,
    pointer: &str,
    workspace: &BTreeMap<String, Item>,
    out: &mut Vec<Dependency>,
    skipped: &mut Vec<Skipped>,
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
                let declared = value;
                let value = if inherited {
                    workspace.get(name).unwrap_or(value)
                } else {
                    value
                };
                let skip = |reason: String| Skipped {
                    manager: Manager::Cargo,
                    file: path.into(),
                    line: declared
                        .span()
                        .map(|span| Location::new(text, span.start, span.end, String::new()).line),
                    value: name.into(),
                    reason,
                };
                // A path or git dependency is not on crates.io. One that also
                // names a version is worth a skip: the version is for publishing.
                let local = ["path", "git"].into_iter().find(|k| value.get(k).is_some());
                if let Some(kind) = local {
                    if value.get("version").is_some() {
                        skipped.push(skip(format!(
                            "a {kind} dependency, not looked up on crates.io"
                        )));
                    }
                    continue;
                }
                // `crates-io` is Cargo's reserved name for crates.io itself.
                if let Some(registry) = value
                    .get("registry")
                    .filter(|registry| registry.as_str() != Some("crates-io"))
                {
                    skipped.push(skip(match registry.as_str() {
                        Some(name) => format!(
                            "from the alternate registry `{name}`, which the Count does not look up"
                        ),
                        None => {
                            "from an alternate registry, which the Count does not look up".into()
                        }
                    }));
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
            walk(path, text, table, &next, workspace, out, skipped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::glob;

    /// Cargo reads `members` as path globs: `*` and `?` stop at `/`, `**`
    /// crosses directories.
    #[test]
    fn member_globs_follow_cargos_path_semantics() {
        for (pattern, path, expected) in [
            ("crates/*", "crates/app", true),
            ("crates/*", "crates/app/sub", false),
            ("crates/*", "crates", false),
            ("./crates/*/", "crates/app", true),
            ("crates/**", "crates/app/sub", true),
            ("crates/**/lib", "crates/lib", true),
            ("crates/**/lib", "crates/a/b/lib", true),
            ("crates/a?p", "crates/app", true),
            ("crates/a?p", "crates/ap", false),
            ("crates/[ab]pp", "crates/bpp", true),
            ("crates/[!ab]pp", "crates/bpp", false),
            ("*", "app", true),
            ("*", "crates/app", false),
            ("app", "app", true),
            ("app", "application", false),
        ] {
            let matched = glob(pattern).is_some_and(|glob| glob.is_match(path));
            assert_eq!(matched, expected, "{pattern} against {path}");
        }
        assert!(glob("crates/[").is_none());
    }

    /// The repository controls `members`, so matching must not take exponential
    /// time in the number of stars; this finishes at once, and would not
    /// backtracking over every split.
    #[test]
    fn a_hostile_member_glob_matches_in_linear_time() {
        let pattern = "*a".repeat(60) + "*b";
        let path = "a".repeat(200);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let matched = glob(&pattern).is_some_and(|glob| glob.is_match(&path));
            let _ = sender.send(matched);
        });
        let matched = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("matching a hostile glob timed out");
        assert!(!matched);
    }
}
