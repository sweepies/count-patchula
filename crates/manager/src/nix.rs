//! `flake.lock` beside a `flake.nix`: each direct input of the root node
//! is a dependency, locked to the revision `nix flake update` moves. The
//! lock is only read; nix itself rewrites it.
use crate::*;
use jsonc_parser::common::Ranged;
use serde_json::{Map, Value};

/// Why the Count does not let nix fetch an input, if he refuses it. `dir` is
/// the flake's directory in the repository, against which a relative path
/// must stay inside the repository; without it (a transitive input, relative
/// to its parent's source) a relative path is allowed.
pub fn refusal(attrs: &Map<String, Value>, dir: Option<&str>) -> Option<String> {
    let text = |key: &str| attrs.get(key).and_then(Value::as_str).unwrap_or_default();
    let url = text("url");
    let local = |path: &str| match dir {
        _ if path.starts_with('/') => Some("it resolves outside the repository".to_owned()),
        Some(dir) if !inside(dir, path) => Some("it resolves outside the repository".to_owned()),
        _ => None,
    };
    match text("type") {
        "github" | "gitlab" | "sourcehut" | "indirect" => None,
        "path" => local(text("path")),
        "git" | "tarball" | "file" => {
            if let Some(path) = url.strip_prefix("file://") {
                local(path)
            } else if url.starts_with("ssh://")
                || url.split_once(':').is_some_and(|(user_host, _)| {
                    user_host.contains('@') && !user_host.contains('/')
                })
            {
                Some("ssh is not allowed; only https".into())
            } else if !url.starts_with("https://") {
                Some("only https URLs are fetched".into())
            } else {
                None
            }
        }
        "hg" | "mercurial" => Some("Mercurial inputs are not supported".into()),
        other => Some(format!("input type {other:?} is not supported")),
    }
}

/// [`refusal`] for a flake reference as `flake.nix` writes it, such as
/// `git+ssh://host/repo` or `path:../shared`.
pub fn reference_refusal(reference: &str, dir: Option<&str>) -> Option<String> {
    let (scheme, rest) = reference.split_once(':').unwrap_or(("", reference));
    let attrs = |kind: &str, key: &str, value: &str| {
        Map::from_iter([
            ("type".to_owned(), Value::from(kind)),
            (key.to_owned(), Value::from(value)),
        ])
    };
    let attrs = match scheme {
        "" if rest.starts_with(['.', '/']) => attrs("path", "path", rest),
        "" | "github" | "gitlab" | "sourcehut" | "flake" => return None,
        "path" => attrs(
            "path",
            "path",
            rest.split(['?', '#']).next().unwrap_or(rest),
        ),
        "https" | "http" | "file" => attrs("tarball", "url", reference),
        _ => match scheme.split_once('+') {
            Some((kind @ ("git" | "tarball" | "file" | "hg"), transport)) => {
                attrs(kind, "url", &format!("{transport}:{rest}"))
            }
            _ => {
                return Some(format!(
                    "flake reference scheme {scheme:?} is not supported"
                ));
            }
        },
    };
    refusal(&attrs, dir)
}

/// Whether a relative path from `dir` stays inside the repository.
fn inside(dir: &str, path: &str) -> bool {
    let mut depth = dir.split('/').filter(|s| !s.is_empty()).count();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." if depth == 0 => return false,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    true
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn escape(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '%' | '&' | '#' | '=' | '?' | ' ' => format!("%{:02X}", c as u32),
            c => c.into(),
        })
        .collect()
}

/// The flake reference an input was written with, as `nix` reads it on the
/// command line: `github:NixOS/nixpkgs/nixos-unstable`, `git+https://host/repo?ref=main`.
pub fn reference(original: &Map<String, Value>) -> Option<String> {
    let text = |key: &str| original.get(key).and_then(Value::as_str);
    let kind = text("type")?;
    let (mut base, mut used): (String, Vec<&str>) = match kind {
        "github" | "gitlab" | "sourcehut" => (
            format!("{kind}:{}/{}", text("owner")?, text("repo")?),
            vec!["type", "owner", "repo"],
        ),
        "indirect" => (format!("flake:{}", text("id")?), vec!["type", "id"]),
        "path" => (format!("path:{}", text("path")?), vec!["type", "path"]),
        "git" | "tarball" | "file" | "hg" => {
            (format!("{kind}+{}", text("url")?), vec!["type", "url"])
        }
        _ => return None,
    };
    let mut query = Vec::new();
    if matches!(kind, "github" | "gitlab" | "sourcehut" | "indirect") {
        match (text("ref"), text("rev")) {
            (Some(r), None) | (None, Some(r)) => {
                base.push('/');
                base.push_str(r);
                used.extend(["ref", "rev"]);
            }
            _ => {}
        }
    }
    for (key, value) in original {
        if used.contains(&key.as_str()) {
            continue;
        }
        query.push(format!("{}={}", escape(key), escape(&scalar(value)?)));
    }
    if !query.is_empty() {
        base.push(if base.contains('?') { '&' } else { '?' });
        base.push_str(&query.join("&"));
    }
    Some(base)
}

/// The input's fully qualified root URL: `https://github.com/NixOS/nixpkgs`.
fn root_url(original: &Map<String, Value>) -> Option<String> {
    let text = |key: &str| original.get(key).and_then(Value::as_str);
    let hosted = |default: &str| {
        Some(format!(
            "https://{}/{}/{}",
            text("host").unwrap_or(default),
            text("owner")?,
            text("repo")?
        ))
    };
    match text("type")? {
        "github" => hosted("github.com"),
        "gitlab" => hosted("gitlab.com"),
        "sourcehut" => hosted("git.sr.ht"),
        "indirect" => Some(format!("flake:{}", text("id")?)),
        "path" => text("path").map(str::to_owned),
        _ => text("url").map(|u| u.split('?').next().unwrap_or(u).to_owned()),
    }
}

pub(super) fn extract(
    path: &str,
    text: &str,
    files: &Files,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let Some(dir) = path.strip_suffix("flake.lock") else {
        return Ok(Vec::new());
    };
    if !files.contains_key(&format!("{dir}flake.nix")) {
        return Ok(Vec::new());
    }
    let lock: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let ast = jsonc_parser::parse_to_ast(text, &Default::default(), &Default::default())
        .map_err(|e| e.to_string())?
        .value
        .ok_or("empty flake.lock")?;
    let nodes = lock["nodes"].as_object().ok_or("flake.lock has no nodes")?;
    let spans = ast
        .as_object()
        .and_then(|o| o.get_object("nodes"))
        .ok_or("flake.lock has no nodes")?;
    let root = lock["root"].as_str().unwrap_or("root");
    let Some(inputs) = nodes.get(root).and_then(|n| n["inputs"].as_object()) else {
        return Ok(Vec::new());
    };
    let line = |name: &str| {
        spans
            .get_object(root)
            .and_then(|n| n.get_object("inputs"))
            .and_then(|i| i.properties.iter().find(|p| p.name.as_str() == name))
            .map(|p| Location::new(text, p.start(), p.start(), String::new()).line)
    };
    let directory = dir.trim_end_matches('/');
    let mut out = Vec::new();
    for (name, target) in inputs {
        // A list follows another input's path and has no lock of its own.
        let Some(node) = target.as_str() else {
            continue;
        };
        let (Some(original), Some(locked)) = (
            nodes.get(node).and_then(|n| n["original"].as_object()),
            nodes.get(node).and_then(|n| n["locked"].as_object()),
        ) else {
            continue;
        };
        let Some(current) = reference(original) else {
            skipped.push(Skipped {
                manager: Manager::Nix,
                file: path.into(),
                line: line(name),
                value: name.clone(),
                reason: "input type is not supported".into(),
            });
            continue;
        };
        let refused =
            refusal(original, Some(directory)).or_else(|| refusal(locked, Some(directory)));
        let local = original.get("type").and_then(Value::as_str) == Some("path");
        if refused.is_some() || local {
            skipped.push(Skipped {
                manager: Manager::Nix,
                file: path.into(),
                line: line(name),
                value: current,
                reason: refused
                    .unwrap_or_else(|| "a path input changes with the repository".into()),
            });
            continue;
        }
        let field = if locked.contains_key("rev") {
            "rev"
        } else {
            "narHash"
        };
        let Some(span) = spans
            .get_object(node)
            .and_then(|n| n.get_object("locked"))
            .and_then(|l| l.get_string(field))
        else {
            continue;
        };
        out.push(Dependency {
            manager: Manager::Nix,
            file: path.into(),
            location: Location::new(
                text,
                span.start() + 1,
                span.end() - 1,
                format!(
                    "/nodes/{}/locked/{field}",
                    node.replace('~', "~0").replace('/', "~1")
                ),
            ),
            name: name.clone(),
            package: root_url(original).unwrap_or_else(|| current.clone()),
            kind: DependencyKind::FlakeInput,
            current,
            resolved: None,
            digest: Some(span.value.to_string()),
            source: Source::Nix,
            scheme: Scheme::Loose,
            extract: None,
        });
    }
    Ok(out)
}
