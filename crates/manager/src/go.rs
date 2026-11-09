//! Go: `require` lines and the `go` and `toolchain` directives in `go.mod`.
//!
//! A module path names the place its code is served from, so the module
//! proxy cannot hand out someone else's code under a private path; which
//! paths are private is the environment's `GOPRIVATE`, which no committed
//! file sets. What a committed file can do is `replace` a module, in
//! `go.mod` or a `go.work` above it: such a module is skipped, since its
//! requirement is not what the build uses.
use crate::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn extract(
    path: &str,
    text: &str,
    modules: &Modules,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let replacements = replacements(path, modules);
    static REQUIRE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"^\s*(?:require\s+)?(?P<name>[^\s"`]+|"[^"]+"|`[^`]+`)\s+(?P<version>v\d[^\s]*)"#,
        )
        .expect("go.mod require pattern")
    });
    static DIRECTIVE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^\s*(?P<kind>go|toolchain)\s+(?P<version>(?:go)?\d[^\s]*)")
            .expect("go.mod directive pattern")
    });
    let require = &*REQUIRE;
    let directive = &*DIRECTIVE;
    let mut out = Vec::new();
    let mut block = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let code = line.split("//").next().unwrap_or("").trim();
        if code.starts_with("require") && code.ends_with('(') {
            block = true;
        } else if code == ")" {
            block = false;
        } else if (block || code.starts_with("require "))
            && let Some(c) = require.captures(code)
        {
            let version = c.name("version").unwrap();
            let n = line.find(code).unwrap() + version.start();
            let name = c["name"].trim_matches(['"', '`']);
            if replacements
                .as_ref()
                .is_ok_and(|replaced| replaced.contains(name))
            {
                skipped.push(Skipped {
                    manager: Manager::Go,
                    file: path.into(),
                    line: Some(Location::new(text, offset + n, offset + n, String::new()).line),
                    value: name.into(),
                    reason: REPLACED.into(),
                });
                offset += line.len();
                continue;
            }
            out.push(Dependency {
                manager: Manager::Go,
                file: path.into(),
                location: Location::new(
                    text,
                    offset + n,
                    offset + n + version.len(),
                    format!("/require/{name}"),
                ),
                form: EditForm::Scalar(Syntax::Bare),
                name: name.into(),
                package: name.into(),
                kind: DependencyKind::Production,
                current: version.as_str().into(),
                resolved: Some(version.as_str().into()),
                digest: version
                    .as_str()
                    .rsplit_once('-')
                    .and_then(|(prefix, hash)| {
                        (prefix.rsplit('-').next().is_some_and(|time| {
                            time.len() == 14 && time.bytes().all(|b| b.is_ascii_digit())
                        }) && hash.len() == 12
                            && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                        .then(|| hash.to_owned())
                    }),
                source: Source::Go,
                scheme: Scheme::Go,
                extract: None,
                included_by: None,
                registry: replacements
                    .as_ref()
                    .err()
                    .and_then(|reason| crate::origins::Named::Unusable(reason.clone()).registry()),
            });
        } else if let Some(c) = directive.captures(code) {
            let version = c.name("version").unwrap();
            let n = line.find(code).unwrap() + version.start();
            out.push(Dependency {
                manager: Manager::Go,
                file: path.into(),
                location: Location::new(
                    text,
                    offset + n,
                    offset + n + version.len(),
                    format!("/{}", &c["kind"]),
                ),
                form: EditForm::Scalar(Syntax::Bare),
                name: c["kind"].into(),
                package: "go".into(),
                kind: if &c["kind"] == "go" {
                    DependencyKind::GoDirective
                } else {
                    DependencyKind::Toolchain
                },
                current: version.as_str().trim_start_matches("go").into(),
                resolved: Some(version.as_str().trim_start_matches("go").into()),
                digest: None,
                source: Source::Mise,
                scheme: Scheme::Loose,
                extract: None,
                included_by: None,
                registry: None,
            });
        }
        offset += line.len();
    }
    // Go rejects a file whose block never closes, as `replaced` and `uses` do.
    if block {
        return Err("a `require (` block does not end".into());
    }
    Ok(out)
}

/// Why a module a `replace` directive names, or a workspace's own, is
/// skipped.
const REPLACED: &str = "replaced by a `replace` directive or taken from the workspace, so its requirement is not what the build uses; updating it could move the build off the replacement";

/// The modules `replace` directives in `text` (a `go.mod` or `go.work`)
/// name on their left, whatever version they replace; none when a
/// directive is not one the Count can read.
fn replaced(text: &str) -> Option<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let mut block = false;
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let directive = if block {
            if code == ")" {
                block = false;
                continue;
            }
            code
        } else if let Some(rest) = code.strip_prefix("replace") {
            let rest = rest.trim();
            if rest == "(" {
                block = true;
                continue;
            }
            if !code["replace".len()..].starts_with([' ', '\t']) {
                continue;
            }
            rest
        } else {
            continue;
        };
        if directive.is_empty() {
            continue;
        }
        let (left, right) = directive.split_once("=>")?;
        let mut words = left.split_whitespace();
        let module = words.next()?.trim_matches(['"', '`']);
        // At most a version after the module, and a replacement after `=>`.
        if module.is_empty() || words.count() > 1 || right.trim().is_empty() {
            return None;
        }
        out.insert(module.to_owned());
    }
    (!block).then_some(out)
}

/// The folders a `go.work` at `path` lists in `use`, as path prefixes;
/// none when one is not a folder in the repository the Count can resolve.
fn uses(path: &str, text: &str) -> Option<Vec<String>> {
    let base = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let mut out = Vec::new();
    let mut block = false;
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        let named = if block {
            if code == ")" {
                block = false;
                continue;
            }
            code
        } else if let Some(rest) = code.strip_prefix("use") {
            if !rest.starts_with([' ', '\t', '(']) {
                continue;
            }
            let rest = rest.trim();
            if rest == "(" {
                block = true;
                continue;
            }
            rest
        } else {
            continue;
        };
        if named.is_empty() {
            continue;
        }
        let named = named.trim_matches(['"', '`']);
        if named.starts_with('/') || named.contains(char::is_whitespace) {
            return None;
        }
        let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
        for part in named.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                part => parts.push(part),
            }
        }
        out.push(parts.iter().map(|p| format!("{p}/")).collect());
    }
    (!block).then_some(out)
}

/// The `go.mod` files the `go.work` files among `files` list in `use`.
pub(crate) fn workspace_modules(files: &Files) -> Vec<String> {
    files
        .iter()
        .filter(|(path, _)| work_file(path))
        .filter_map(|(path, text)| uses(path, text.read().ok()?))
        .flatten()
        .map(|dir| format!("{dir}go.mod"))
        .collect()
}

/// The module path a `go.mod` declares.
fn module_path(text: &str) -> Option<&str> {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or("").trim())
        .find_map(|code| {
            code.strip_prefix("module")
                .filter(|rest| rest.starts_with([' ', '\t']))
        })
        .map(|rest| rest.trim().trim_matches(['"', '`']))
}

/// What a parsed `go.work` says.
struct Workspace {
    replaced: BTreeSet<String>,
    uses: Vec<String>,
}

/// What a parsed `go.mod` says.
struct Module {
    replaced: BTreeSet<String>,
    /// The module path it declares.
    path: Option<String>,
}

/// The `go.work` and `go.mod` files of a repository, each parsed once per
/// extraction.
#[derive(Default)]
pub(crate) struct Modules {
    works: BTreeMap<String, Result<Workspace, String>>,
    mods: BTreeMap<String, Result<Module, String>>,
}

/// Parse every workspace and module file once; one the Count cannot read
/// is kept as its error, so the lookups it shapes fail for that reason.
pub(crate) fn modules(files: &Files) -> Modules {
    let works = files
        .iter()
        .filter(|(path, _)| work_file(path))
        .map(|(path, text)| {
            let read = |text: &str| Some((replaced(text)?, uses(path, text)?));
            let entry = text
                .read()
                .ok()
                .and_then(read)
                .map(|(replaced, uses)| Workspace { replaced, uses })
                .ok_or_else(|| format!("{path}, which the Count cannot read"));
            (path.clone(), entry)
        })
        .collect();
    let mods = files
        .iter()
        .filter(|(path, _)| path.ends_with("go.mod"))
        // An unread go.mod is not extracted, so it shapes no lookup here.
        .filter_map(|(path, text)| Some((path, text.read().ok()?)))
        .map(|(path, text)| {
            let entry = replaced(text)
                .map(|replaced| Module {
                    replaced,
                    path: module_path(text).map(str::to_owned),
                })
                .ok_or_else(|| format!("a replace directive in {path} the Count cannot read"));
            (path.clone(), entry)
        })
        .collect();
    Modules { works, mods }
}

/// The modules the build of the `go.mod` at `path` does not take from its
/// requirements: those it or a `go.work` in its folder or above replaces,
/// and in a workspace, those any module the workspace uses replaces and
/// the workspace's own modules, which Go takes from their folders; why,
/// when one of those files cannot be read.
fn replacements(path: &str, modules: &Modules) -> Result<BTreeSet<String>, String> {
    let mut out = modules
        .mods
        .get(path)
        .ok_or_else(|| format!("a replace directive in {path} the Count cannot read"))?
        .as_ref()
        .map_err(Clone::clone)?
        .replaced
        .clone();
    let mut dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    loop {
        let work = format!("{dir}go.work");
        if let Some(workspace) = modules.works.get(&work) {
            let workspace = workspace.as_ref().map_err(Clone::clone)?;
            out.extend(workspace.replaced.iter().cloned());
            for folder in &workspace.uses {
                let module = format!("{folder}go.mod");
                let module_ref = modules.mods.get(&module).ok_or_else(|| {
                    format!("{module}, which {work} uses and the Count cannot read")
                })?;
                let module_entry = module_ref.as_ref().map_err(Clone::clone)?;
                out.extend(module_entry.replaced.iter().cloned());
                if let Some(name) = &module_entry.path {
                    out.insert(name.clone());
                }
            }
        }
        if dir.is_empty() {
            break;
        }
        let parent = dir.trim_end_matches('/');
        dir = &parent[..parent.rfind('/').map_or(0, |i| i + 1)];
    }
    Ok(out)
}

/// Whether `path` is a Go workspace file.
pub(crate) fn work_file(path: &str) -> bool {
    path.rsplit('/').next() == Some("go.work")
}

pub(crate) static DESCRIPTOR: Descriptor = Descriptor;

pub(crate) struct Descriptor;

impl crate::Ecosystem for Descriptor {
    fn selected(&self, _config: &Config, path: &str) -> bool {
        path.rsplit('/').next() == Some("go.mod")
    }
    fn registry_file(&self, path: &str) -> bool {
        crate::roles::eco_roles(Manager::Go, path).registry
    }
    fn movable_lockfile(&self, dep: &Dependency) -> bool {
        dep.source == Source::Go
    }
    fn declared_form(
        &self,
        dep: &Dependency,
        current: &str,
        value: &str,
    ) -> Result<Option<String>, String> {
        if dep.location.pointer != "/toolchain" {
            return Ok(None);
        }
        if current != format!("go{}", dep.current) {
            return Err("Go toolchain location mismatch".into());
        }
        Ok(Some(format!("go{}", value.trim_start_matches("go"))))
    }
    fn more<'a>(&self, files: &'a Files) -> Option<crate::PathPredicate<'a>> {
        let wanted = workspace_modules(files);
        (!wanted.is_empty()).then(move || {
            Box::new(move |path: &str| wanted.iter().any(|module| module == path))
                as crate::PathPredicate<'a>
        })
    }
    fn prepare<'a>(
        &self,
        _scanner: &'a Scanner,
        files: &'a Files,
        _shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        let modules = modules(files);
        Ok(Prepared {
            extract: Some(Box::new(move |path, text, out| {
                let result = extract(path, text, &modules, &mut out.skipped);
                file_result(out, path, result);
                Ok(())
            })),
            finish: None,
        })
    }
}
