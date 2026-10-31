//! `apm.yml`: the Git-pinned string entries under `dependencies.apm` and
//! `devDependencies.apm`, written `[host/]owner/repo[/path]#ref`. A tag is
//! looked up as GitHub tags or, on GitLab and Gitea/Forgejo, as forge tags. A
//! commit pin is tracked through the tag in its trailing comment
//! (`owner/repo#<sha> # v2.0.0`), the same way as a GitHub Actions pin.
use crate::*;
use serde_yaml_ng::Value;

const SECTIONS: &[(&str, DependencyKind)] = &[
    ("dependencies", DependencyKind::Production),
    ("devDependencies", DependencyKind::Development),
];

pub(super) fn extract(
    path: &str,
    text: &str,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Dependency>, String> {
    let locations = yaml_locations::parse(text)?;
    let Some(document) = yaml::docs(text)?.into_iter().next() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (section, kind) in SECTIONS {
        let Some(entries) = document
            .get(section)
            .and_then(|s| s.get("apm"))
            .and_then(Value::as_sequence)
        else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let pointer = format!("/0/{section}/apm/{index}");
            let location = locations.get(&pointer);
            let skip = |value: &str, reason: &str| Skipped {
                manager: Manager::Apm,
                file: path.into(),
                line: location.map(|l| l.line).or_else(|| {
                    locations
                        .iter()
                        .find(|(p, _)| p.starts_with(&format!("{pointer}/")))
                        .map(|(_, l)| l.line)
                }),
                value: value.into(),
                reason: reason.into(),
            };
            let Some(entry) = entry.as_str() else {
                if let Some((value, reason)) = object(entry) {
                    skipped.push(skip(value, reason));
                }
                continue;
            };
            let Some(location) = location else {
                skipped.push(skip(
                    entry,
                    "the entry is not a plain scalar the Count can edit in place",
                ));
                continue;
            };
            match dependency(path, text, entry, location, *kind) {
                Ok(Some(dep)) => out.push(dep),
                Ok(None) => {}
                Err(reason) => skipped.push(skip(entry, reason)),
            }
        }
    }
    Ok(out)
}

/// Why an object entry is not updated, or `None` for a local path.
fn object(entry: &Value) -> Option<(&str, &'static str)> {
    let field = |key: &str| entry.get(key).and_then(Value::as_str);
    if let Some(git) = field("git") {
        return Some((
            git,
            "object entries are not updated; write the entry as owner/repo#tag",
        ));
    }
    if let Some(id) = field("id").or_else(|| field("registry")) {
        return Some((id, "registry entries have no Count datasource"));
    }
    if let Some(name) = field("marketplace").and(field("name")) {
        return Some((name, "marketplace entries have no Count datasource"));
    }
    None
}

fn dependency(
    path: &str,
    text: &str,
    entry: &str,
    location: &Location,
    kind: DependencyKind,
) -> Result<Option<Dependency>, &'static str> {
    if ["./", "../", "/", "~/", ".\\", "..\\", "~\\"]
        .iter()
        .any(|p| entry.starts_with(p))
    {
        return Ok(None);
    }
    if entry.contains("://") || entry.starts_with("git@") {
        return Err("URL entries are not updated; write the entry as [host/]owner/repo#tag");
    }
    let Some((name, reference)) = entry.split_once('#') else {
        return Err("no #ref to update; the lockfile pins its commit");
    };
    let mut segments = name.split('/');
    let first = segments.next().unwrap_or_default();
    let host = first.contains('.').then(|| first.to_ascii_lowercase());
    let (owner, repo) = if host.is_some() {
        (segments.next(), segments.next())
    } else {
        (Some(first), segments.next())
    };
    let valid = |s: &&str| {
        !s.is_empty()
            && !matches!(*s, "." | "..")
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
    };
    let (Some(owner), Some(repo)) = (owner.filter(valid), repo.filter(valid)) else {
        return Err("not an owner/repo reference");
    };
    let (source, package) = match host.as_deref() {
        None | Some("github.com") => (Source::GithubTags, format!("{owner}/{repo}")),
        Some(host) if host.contains(':') => {
            return Err("hosts with a custom port have no Count datasource");
        }
        Some(host)
            if host == "gitlab.com"
                || host.split('.').any(|l| l == "gitlab")
                || gitea_host(host) =>
        {
            (Source::ForgeTags, format!("{host}/{owner}/{repo}"))
        }
        Some(_) => return Err("the host has no Count datasource for its tags"),
    };
    let comment = trailing_comment(text, location).map(|range| &text[range]);
    let hex = reference.bytes().all(|b| b.is_ascii_hexdigit());
    let (digest, version) = if reference.len() == 40 && hex {
        let tag = comment.and_then(|c| c[1..].split_whitespace().next());
        let Some(version) = tag.filter(|v| actions::version_comment(v)) else {
            return Err("commit pin has no tag comment to track");
        };
        (Some(reference.to_owned()), version)
    } else if (7..40).contains(&reference.len()) && hex {
        return Err("short commit pins are not updated; pin the full commit with a tag comment");
    } else if actions::version_comment(reference) {
        (None, reference)
    } else {
        return Err("the ref is a branch or a tag that is not a version");
    };
    // A pin adds a tag comment, which would swallow the rest of a flow list.
    if in_flow_list(text, location) {
        return Err("entries in a flow-style list are not edited in place");
    }
    // The located range takes in a trailing comment, which carries the tag of a
    // commit pin and is rewritten with it.
    let end = trailing_comment(text, location).map_or(location.end, |range| range.end);
    Ok(Some(Dependency {
        manager: Manager::Apm,
        file: path.into(),
        location: Location::new(text, location.start, end, location.pointer.clone()),
        name: name.into(),
        package,
        kind,
        current: version.into(),
        resolved: Some(version.into()),
        digest,
        source,
        scheme: Scheme::Loose,
        extract: None,
        included_by: None,
    }))
}

/// The comment after an entry on its line, from its `#`, when the whitespace
/// before it ends in a space: where the edit splits a scalar from its comment.
fn trailing_comment(text: &str, location: &Location) -> Option<std::ops::Range<usize>> {
    let rest = &text[location.end..];
    let rest = rest.lines().next().unwrap_or_default().trim_end();
    let hash = rest.find('#')?;
    (hash > 0 && rest[..hash].trim().is_empty() && rest[..hash].ends_with(' '))
        .then(|| location.end + hash..location.end + rest.len())
}

/// Whether an entry sits in a `[...]` list: the token before it, past any
/// anchor or tag, opens the list or separates its items.
fn in_flow_list(text: &str, location: &Location) -> bool {
    let mut before = text[..location.start].trim_end();
    if let Some((rest, token)) = before.rsplit_once(char::is_whitespace)
        && token.starts_with(['&', '!'])
    {
        before = rest.trim_end();
    }
    before.ends_with(['[', ','])
}
