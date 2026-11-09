use crate::*;
use patchula_versioning::{Ecosystem, Versioning};
use std::collections::BTreeSet;
use toml_edit::{Document, Item};

/// Cargo manifests and configurations, each parsed once per extraction:
/// a manifest the Count cannot parse is kept as its error, so the lookups
/// it shapes fail for that reason.
#[derive(Default)]
pub(crate) struct Parsed<'a> {
    /// Every `Cargo.toml` and `Cargo.lock` in the manifests, by path.
    pub manifests: BTreeMap<&'a str, Result<Document<&'a str>, String>>,
    /// Every Cargo configuration in the files, by path.
    pub configs: BTreeMap<&'a str, Result<toml_edit::DocumentMut, ()>>,
}

/// Parse the selected manifests and every configuration file once.
pub(crate) fn parsed<'a>(files: &'a Files, keep: impl Fn(&str) -> bool) -> Parsed<'a> {
    Parsed {
        manifests: files
            .iter()
            .filter(|(p, _)| keep(p) && (p.ends_with("Cargo.toml") || p.ends_with("Cargo.lock")))
            // One the Count could not read fails its crates' lookups
            // instead; see `replaced`.
            .filter_map(|(p, t)| Some((p, t.read().ok()?)))
            .map(|(p, t)| {
                (
                    p.as_str(),
                    Document::parse(t).map_err(|e| format!("{p}: {e}")),
                )
            })
            .collect(),
        configs: files
            .iter()
            .filter(|(p, _)| config_file(p))
            .map(|(p, t)| {
                (
                    p.as_str(),
                    t.read()
                        .ok()
                        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
                        .ok_or(()),
                )
            })
            .collect(),
    }
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
    let Some(doc) = files.get(path) else {
        return Ok(Vec::new());
    };
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
                            "from the alternate registry `{}`, which the Count does not look up",
                            crate::origins::shown(name)
                        ),
                        None => {
                            "from an alternate registry, which the Count does not look up".into()
                        }
                    }));
                    continue;
                }
                // An index named by its URL, which may hold a secret.
                if value.get("registry-index").is_some() {
                    skipped.push(skip(
                        "from an alternate registry index, which the Count does not look up".into(),
                    ));
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
                    included_by: None,
                    registry: None,
                });
            }
        } else if let Some(table) = item.as_table_like() {
            walk(path, text, table, &next, workspace, out, skipped);
        }
    }
}

/// Whether `path` is a Cargo configuration file in the repository.
pub(crate) fn config_file(path: &str) -> bool {
    [".cargo/config.toml", ".cargo/config"]
        .iter()
        .any(|name| path == *name || path.ends_with(&format!("/{name}")))
}

/// Whether a Cargo configuration could send a bare `cargo install` to a
/// registry besides crates.io: a `[registries]` entry naming a non-public
/// index or one with other settings the Count does not read, or a
/// `[registry]` table naming a default or credential provider; or a file
/// it cannot read.
pub(crate) fn nonpublic_registry(text: &str) -> bool {
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return true;
    };
    if let Some(registries) = doc.get("registries").and_then(Item::as_table_like) {
        for (_, entry) in registries.iter() {
            let Some(entry) = entry.as_table_like() else {
                return true;
            };
            // The sparse and the git index alike, by host and by exact
            // URL — never a substring, so a lookalike host counts.
            let public = entry.get("index").and_then(Item::as_str).is_some_and(|i| {
                crate::web_host(i.strip_prefix("sparse+").unwrap_or(i))
                    .is_some_and(|host| host == "index.crates.io")
                    || i.trim_end_matches('/') == "https://github.com/rust-lang/crates.io-index"
            });
            let benign = entry
                .iter()
                .all(|(key, _)| matches!(key, "index" | "token"));
            if !(public && benign) {
                return true;
            }
        }
    }
    doc.get("registry")
        .and_then(Item::as_table_like)
        .is_some_and(|r| {
            r.get("default").is_some()
                || r.get("index").is_some()
                || r.get("global-credential-providers").is_some()
        })
}

/// The folder of the workspace `dir` belongs to: the nearest folder at or
/// above it whose manifest has a `[workspace]` table; `dir` itself outside
/// one.
fn workspace_root<'a>(parsed: &Parsed<'a>, dir: &'a str) -> &'a str {
    let mut folder = dir;
    loop {
        let declares = parsed
            .manifests
            .get(format!("{folder}Cargo.toml").as_str())
            .is_some_and(|doc| doc.as_ref().is_ok_and(|doc| doc.get("workspace").is_some()));
        if declares {
            return folder;
        }
        if folder.is_empty() {
            return dir;
        }
        let parent = folder.trim_end_matches('/');
        folder = &parent[..parent.rfind('/').map_or(0, |i| i + 1)];
    }
}

/// The Cargo configurations that can apply to the manifest or lockfile at
/// `path`, parsed: those in its folder or above, and, since Cargo reads them
/// from wherever it runs, those anywhere in its workspace.
fn configs<'a>(
    parsed: &'a Parsed<'a>,
    path: &str,
) -> Vec<(&'a str, &'a Result<toml_edit::DocumentMut, ()>)> {
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let root = workspace_root(parsed, dir);
    parsed
        .configs
        .iter()
        .filter(|(config, _)| {
            let folder = config
                .strip_suffix(".cargo/config.toml")
                .or_else(|| config.strip_suffix(".cargo/config"))
                .unwrap_or("");
            dir.starts_with(folder) || folder.starts_with(root)
        })
        .map(|(config, document)| (*config, document))
        .collect()
}

/// Why the crates.io lookups of the manifest or lockfile at `path` cannot be
/// taken to be crates.io's: a Cargo configuration in its folder or above
/// that replaces crates.io with another source, or one the Count cannot
/// read. `Cargo.lock` records crates.io for a replaced crate all the same,
/// so it cannot say. A replacement by a vendored `directory` source keeps
/// crates.io's crates, as `cargo vendor` copied them.
///
/// Every definition of a source in any configuration that applies counts,
/// so one a closer configuration overrides can only make lookups fail.
pub(crate) fn replaced(files: &Files, parsed: &Parsed, path: &str) -> Option<String> {
    // A patch from another registry, in any manifest at or above the crate's
    // folder (its workspace's included) or any applying configuration, may
    // stand for any crate, under any of the names Cargo takes for it.
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let mut manifests = Vec::new();
    let mut folder = dir;
    loop {
        if let Some((name, _)) = files.get_key_value(&format!("{folder}Cargo.toml")) {
            let document = parsed.manifests.get(name.as_str());
            let Some(Ok(document)) = document else {
                return Some(format!("{name}, which the Count cannot read"));
            };
            manifests.push((name.as_str(), document.as_table()));
        }
        if folder.is_empty() {
            break;
        }
        let parent = folder.trim_end_matches('/');
        folder = &parent[..parent.rfind('/').map_or(0, |i| i + 1)];
    }
    let mut sources: Vec<(String, toml_edit::Table)> = Vec::new();
    let configs = configs(parsed, path);
    let mut documents = Vec::new();
    for (config, document) in configs {
        let Ok(document) = document else {
            return Some(format!("{config}, which the Count cannot read"));
        };
        documents.push((config, document.as_table()));
    }
    for (file, document) in manifests.iter().chain(&documents) {
        let elsewhere = |entries: Option<&dyn toml_edit::TableLike>| {
            entries.is_some_and(|entries| {
                entries.iter().any(|(_, entry)| {
                    entry.get("registry").is_some() || entry.get("registry-index").is_some()
                })
            })
        };
        let patches = document.get("patch").and_then(Item::as_table_like);
        let patched = patches.is_some_and(|patches| {
            patches
                .iter()
                .any(|(_, source)| elsewhere(source.as_table_like()))
        });
        if patched || elsewhere(document.get("replace").and_then(Item::as_table_like)) {
            return Some(format!("a patch in {file} from another registry"));
        }
    }
    for (config, document) in &documents {
        let unread = || Some(format!("{config}, which the Count cannot read"));
        // A configuration that includes others says things the Count does
        // not read.
        if document.get("include").is_some() {
            return unread();
        }
        let Some(table) = document.get("source") else {
            continue;
        };
        let Some(table) = table.as_table_like() else {
            return unread();
        };
        for (name, source) in table.iter() {
            let Some(source) = source.as_table_like() else {
                return unread();
            };
            let mut owned = toml_edit::Table::new();
            for (key, value) in source.iter() {
                owned.insert(key, value.clone());
            }
            sources.push((name.to_owned(), owned));
        }
    }
    // Follow every replacement of crates.io to where it ends.
    let mut queue = vec!["crates-io".to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let definitions: Vec<&toml_edit::Table> = sources
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, table)| table)
            .collect();
        if definitions.is_empty() {
            // crates.io itself, unreplaced, needs no definition; any other
            // source is defined outside the repository.
            if name == "crates-io" {
                continue;
            }
            return Some(format!(
                "crates.io replaced by the source `{}`, which the repository does not define",
                crate::origins::shown(&name)
            ));
        }
        for table in definitions {
            let keys: Vec<&str> = table.iter().map(|(key, _)| key).collect();
            match (
                table.get("replace-with").and_then(Item::as_str),
                keys.as_slice(),
            ) {
                (Some(next), _) => queue.push(next.to_owned()),
                (None, ["directory"]) => {}
                // crates.io's own definition holds nothing else to follow.
                (None, _) if name == "crates-io" => {}
                (None, _) => {
                    return Some(format!(
                        "crates.io replaced by the source `{}`, which the Count does not read",
                        crate::origins::shown(&name)
                    ));
                }
            }
        }
    }
    None
}

/// crates.io, for `crate::origins` and cache partitioning.
pub(crate) const CRATES: crate::origins::Public = crate::origins::Public {
    source: Source::Crates,
    label: "crates.io",
    is_public: |url| {
        crate::web_host(url)
            .is_some_and(|host| matches!(host.as_str(), "index.crates.io" | "crates.io"))
    },
};

/// The parsed manifests, lock docs and configs extraction shares.
struct State<'a> {
    parsed: Parsed<'a>,
    docs: BTreeMap<&'a str, Document<&'a str>>,
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn selected(&self, _config: &Config, path: &str) -> bool {
        let leaf = path.rsplit('/').next().unwrap_or(path);
        matches!(
            leaf,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
        )
    }
    fn registry_file(&self, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Cargo, path).registry
    }
    fn preserves_ranges(&self) -> bool {
        true
    }
    fn movable_lockfile(&self, dep: &Dependency) -> bool {
        dep.source == Source::Crates
    }
    fn backend_settings(&self, path: &str) -> bool {
        config_file(path)
    }
    fn more<'a>(&self, files: &'a Files) -> Option<crate::PathPredicate<'a>> {
        // The manifests above a crate's, which can patch it as its
        // workspace, whatever `exclude` leaves out.
        let ancestors: std::collections::BTreeSet<String> = files
            .keys()
            .filter(|p| p.ends_with("Cargo.toml") || p.ends_with("Cargo.lock"))
            .flat_map(|p| {
                let mut out = Vec::new();
                let mut dir = &p[..p.rfind('/').map_or(0, |i| i + 1)];
                // The manifest beside a lockfile, then those above.
                out.push(format!("{dir}Cargo.toml"));
                while !dir.is_empty() {
                    let parent = dir.trim_end_matches('/');
                    dir = &parent[..parent.rfind('/').map_or(0, |i| i + 1)];
                    out.push(format!("{dir}Cargo.toml"));
                }
                out
            })
            .collect();
        Some(Box::new(move |path| ancestors.contains(path)))
    }
    fn prepare<'a>(
        &self,
        scanner: &'a Scanner,
        files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        // Only the manifests and lockfiles Cargo's selection takes: one read
        // only for its patches, above a selected crate, does not shape
        // extraction.
        let parsed = parsed(files, |path| scanner.selected(Manager::Cargo, path));
        // The manifests extraction reads; one the Count cannot parse fails
        // the whole extraction, as before.
        let docs: BTreeMap<&str, Document<&str>> = parsed
            .manifests
            .iter()
            .map(|(p, r)| r.clone().map(|d| (*p, d)))
            .collect::<Result<_, _>>()?;
        let state = std::rc::Rc::new(State { parsed, docs });
        let extract_state = state.clone();
        Ok(Prepared {
            extract: Some(Box::new(move |path, text, out| {
                let result = if path.ends_with("rust-toolchain")
                    || path.ends_with("rust-toolchain.toml")
                {
                    toolchains::rust(path, text)
                } else {
                    extract(path, text, &extract_state.docs, &mut out.skipped).map(|mut deps| {
                        if let Some(reason) = replaced(files, &extract_state.parsed, path) {
                            for dep in deps.iter_mut().filter(|d| d.source == Source::Crates) {
                                dep.registry = origins::Named::Unusable(reason.clone()).registry();
                            }
                        }
                        deps
                    })
                };
                file_result(out, path, result);
                Ok(())
            })),
            finish: Some(Box::new(move |out| {
                out.locked
                    .extend(locked::cargo(files, &state.docs, &out.dependencies));
                let mut memo = BTreeMap::new();
                for locked in out
                    .locked
                    .iter_mut()
                    .filter(|l| l.manager == Manager::Cargo)
                {
                    let reason = memo
                        .entry(locked.file.clone())
                        .or_insert_with(|| replaced(files, &state.parsed, &locked.file));
                    if let Some(reason) = reason {
                        locked.registry = Some(Registry::Unusable(reason.clone()));
                    }
                }
                Ok(())
            })),
        })
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
