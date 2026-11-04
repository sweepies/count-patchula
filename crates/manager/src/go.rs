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
use std::collections::BTreeSet;

pub(super) fn extract(
    path: &str,
    text: &str,
    files: &Files,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let replacements = replacements(path, text, files);
    let require = Regex::new(
        r#"^\s*(?:require\s+)?(?P<name>[^\s"`]+|"[^"]+"|`[^`]+`)\s+(?P<version>v\d[^\s]*)"#,
    )
    .map_err(|e| e.to_string())?;
    let directive = Regex::new(r"^\s*(?P<kind>go|toolchain)\s+(?P<version>(?:go)?\d[^\s]*)")
        .map_err(|e| e.to_string())?;
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
                    .and_then(|reason| crate::origins::Named::Unusable(reason.clone()).recorded()),
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
    Ok(out)
}

/// Why a module a `replace` directive names is skipped.
const REPLACED: &str = "replaced by a `replace` directive, so its requirement is not what the build uses; updating it could move the build off the replacement";

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

/// The modules replaced for the `go.mod` at `path`, by itself or a
/// `go.work` in its folder or above, which Go uses for the modules it
/// lists; why, when one cannot be read.
fn replacements(path: &str, text: &str, files: &Files) -> Result<BTreeSet<String>, String> {
    let mut out = replaced(text)
        .ok_or_else(|| format!("a replace directive in {path} the Count cannot read"))?;
    let mut dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    loop {
        let work = format!("{dir}go.work");
        if let Some(text) = files.get(&work) {
            let read = (text.as_str() != crate::UNREADABLE_SETTINGS)
                .then(|| replaced(text))
                .flatten()
                .ok_or_else(|| format!("{work}, which the Count cannot read"))?;
            out.extend(read);
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
