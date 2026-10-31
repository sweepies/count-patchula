//! `pom.xml`: parent, dependencies, managed dependencies, plugins and build
//! extensions. `${property}` versions are edited where the property is
//! defined in `<properties>`; only the first artifact referencing a property
//! is emitted, so artifacts sharing one never receive conflicting edits.
use crate::*;
use std::collections::BTreeSet;

/// A leaf element's trimmed text span, with its element path. Parent
/// elements carry an instance number so siblings stay distinct.
struct Leaf {
    path: Vec<String>,
    start: usize,
    end: usize,
}

/// A small XML scanner: element paths and leaf text spans, skipping comments,
/// CDATA, processing instructions and declarations.
fn leaves(text: &str) -> Result<Vec<Leaf>, String> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, usize, bool)> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut i = 0;
    while let Some(offset) = text[i..].find('<') {
        let open = i + offset;
        let rest = &text[open..];
        let skip = |end: &str| rest.find(end).map(|n| open + n + end.len());
        if rest.starts_with("<!--") {
            i = skip("-->").ok_or("unterminated XML comment")?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = skip("]]>").ok_or("unterminated CDATA")?;
            continue;
        }
        if rest.starts_with("<?") {
            i = skip("?>").ok_or("unterminated processing instruction")?;
            continue;
        }
        if rest.starts_with("<!") {
            i = skip(">").ok_or("unterminated declaration")?;
            continue;
        }
        let close = open + rest.find('>').ok_or("unterminated XML tag")?;
        let tag = &text[open + 1..close];
        i = close + 1;
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            let (top, body, nested) = stack.pop().ok_or("unbalanced XML")?;
            if top.split('#').next() != Some(name) {
                return Err(format!("mismatched </{name}>"));
            }
            if !nested {
                let raw = &text[body..open];
                let trimmed = raw.trim();
                let start = body + (raw.len() - raw.trim_start().len());
                let mut path: Vec<String> = stack.iter().map(|(n, _, _)| n.clone()).collect();
                path.push(top);
                out.push(Leaf {
                    path,
                    start,
                    end: start + trimmed.len(),
                });
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let name = tag
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .ok_or("empty XML tag")?
            .to_string();
        if let Some(parent) = stack.last_mut() {
            parent.2 = true;
        }
        if self_closing {
            continue;
        }
        // Number each element instance under its parent path.
        let prefix: Vec<_> = stack.iter().map(|(n, _, _)| n.as_str()).collect();
        let key = format!("{}/{name}", prefix.join("/"));
        let count = counts.entry(key).or_default();
        *count += 1;
        stack.push((format!("{name}#{count}"), close + 1, false));
    }
    if !stack.is_empty() {
        return Err("unclosed XML element".into());
    }
    Ok(out)
}

fn bare(segment: &str) -> &str {
    segment.split('#').next().unwrap_or(segment)
}

pub(super) fn extract(path: &str, text: &str) -> Result<Vec<Dependency>, String> {
    let leaves = leaves(text)?;
    let value = |l: &Leaf| &text[l.start..l.end];
    // project/properties/<name>
    let properties: BTreeMap<&str, &Leaf> = leaves
        .iter()
        .filter(|l| {
            l.path.len() == 3 && bare(&l.path[0]) == "project" && bare(&l.path[1]) == "properties"
        })
        .map(|l| (bare(&l.path[2]), l))
        .collect();
    // Group the direct children of each coordinate element instance.
    let mut groups: BTreeMap<String, (DependencyKind, BTreeMap<&str, &Leaf>)> = BTreeMap::new();
    for leaf in &leaves {
        let Some((field, owner)) = leaf.path.split_last() else {
            continue;
        };
        let names: Vec<_> = owner.iter().map(|s| bare(s)).collect();
        let kind = match names.as_slice() {
            ["project", "parent"] => DependencyKind::Build,
            ["project", "dependencies", "dependency"]
            | [
                "project",
                "dependencyManagement",
                "dependencies",
                "dependency",
            ] => DependencyKind::Production,
            ["project", "build", "plugins", "plugin"]
            | ["project", "build", "pluginManagement", "plugins", "plugin"]
            | ["project", "build", "extensions", "extension"] => DependencyKind::Build,
            _ => continue,
        };
        groups
            .entry(owner.join("/"))
            .or_insert_with(|| (kind, BTreeMap::new()))
            .1
            .insert(bare(field), leaf);
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    // Document order, so the first artifact using a property owns it.
    groups.sort_by_key(|(_, fields)| fields.values().map(|l| l.start).min());
    let mut claimed = BTreeSet::new();
    let mut out = Vec::new();
    for (kind, fields) in groups {
        let (Some(artifact), Some(version)) = (fields.get("artifactId"), fields.get("version"))
        else {
            continue;
        };
        let group = match fields.get("groupId") {
            Some(group) => value(group),
            None if kind == DependencyKind::Build => "org.apache.maven.plugins",
            None => continue,
        };
        let (group, artifact) = (group, value(artifact));
        if group.contains("${") || artifact.contains("${") {
            continue;
        }
        let raw = value(version);
        let (location, current) =
            if let Some(name) = raw.strip_prefix("${").and_then(|r| r.strip_suffix('}')) {
                let Some(property) = properties.get(name) else {
                    continue;
                };
                if !claimed.insert(name) {
                    continue;
                }
                (
                    Location::new(
                        text,
                        property.start,
                        property.end,
                        format!("/properties/{name}"),
                    ),
                    value(property),
                )
            } else {
                (
                    Location::new(
                        text,
                        version.start,
                        version.end,
                        format!("/{group}:{artifact}/version"),
                    ),
                    raw,
                )
            };
        if current.is_empty() || current.contains("${") {
            continue;
        }
        out.push(Dependency {
            manager: Manager::Maven,
            file: path.into(),
            location,
            name: format!("{group}:{artifact}"),
            package: format!("{group}:{artifact}"),
            kind,
            current: current.into(),
            resolved: (!current.starts_with(['[', '('])).then(|| current.into()),
            digest: None,
            source: Source::Maven,
            scheme: Scheme::Maven,
            extract: None,
            included_by: None,
        });
    }
    Ok(out)
}
