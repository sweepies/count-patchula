//! The published registry snapshot: which GitHub repositories the
//! mise-versions mirror lists, so that plain GitHub lookups read the mirror
//! only where it has data, and the backends it lists for a short tool name.
use std::{collections::BTreeMap, collections::BTreeSet, sync::LazyLock};

static REGISTRY: LazyLock<BTreeMap<String, Vec<String>>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../registry.json")).expect("published registry snapshot")
});

static REPOSITORIES: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    REGISTRY
        .values()
        .flatten()
        .filter_map(|backend| repository(backend))
        .collect()
});

/// `owner/repo`, lowercased, for a backend that names a GitHub repository.
fn repository(backend: &str) -> Option<String> {
    let (kind, rest) = backend.split_once(':')?;
    let rest = match kind {
        "github" | "ubi" | "aqua" => rest,
        "packslip" => rest.strip_prefix("github.com/").unwrap_or(rest),
        "cargo" => rest.strip_prefix("https://github.com/")?,
        _ => return None,
    };
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    // A host-qualified packslip source is not an owner.
    if owner.is_empty() || repo.is_empty() || owner.contains('.') {
        return None;
    }
    Some(format!("{owner}/{}", repo.trim_end_matches(".git")).to_ascii_lowercase())
}

/// The backends the registry snapshot lists for a short tool name, such as
/// `npm:prettier` for `prettier`; none for a name it does not list.
pub fn backends(tool: &str) -> &'static [String] {
    REGISTRY.get(tool).map_or(&[], Vec::as_slice)
}

/// Backends whose listing reads no GitHub repository when the name after
/// the backend is a plain registry name: no `/` (a path or GitHub
/// shorthand) and no `:` (a URL or a nested backend). npm's scoped names
/// are the exception, handled where they are read.
const REGISTRY_BACKENDS: &[&str] = &["cargo", "conda", "gem", "npm", "pipx", "pypi"];

/// The GitHub repositories, lowercased `owner/repo`, that mise may list
/// `tool`'s versions from. An empty set means none: a core tool or a
/// registry package, for which no GitHub token matters. `None` means the
/// Count cannot tell: a backend that reads arbitrary hosts or Go modules, a
/// shorthand the registry snapshot does not list, or one with a backend
/// like that. For a shorthand every backend the snapshot lists counts,
/// since mise picks the first one usable on its host.
pub fn github_sources(tool: &str) -> Option<BTreeSet<String>> {
    if tool.contains(':') {
        return backend_sources(tool);
    }
    let listed: Vec<&String> = backends(tool)
        .iter()
        .filter(|backend| {
            backend
                .split_once(':')
                .is_none_or(|(kind, _)| !crate::policy::PLUGIN_BACKENDS.contains(&kind))
        })
        .collect();
    if listed.is_empty() {
        return None;
    }
    let mut sources = BTreeSet::new();
    for backend in listed {
        sources.extend(backend_sources(backend)?);
    }
    Some(sources)
}

/// `github_sources` for one `backend:name`.
fn backend_sources(backend: &str) -> Option<BTreeSet<String>> {
    if let Some(repository) = repository(backend) {
        return Some(BTreeSet::from([repository]));
    }
    let (kind, name) = backend.split_once(':')?;
    let plain = !name.is_empty() && !name.contains(':');
    let registry_name = match kind {
        "core" => plain && !name.contains('/'),
        "npm" => {
            plain
                && name
                    .strip_prefix('@')
                    .map_or(!name.contains('/'), |scoped| {
                        scoped.matches('/').count() == 1
                    })
        }
        kind if REGISTRY_BACKENDS.contains(&kind) => plain && !name.contains('/'),
        _ => false,
    };
    registry_name.then(BTreeSet::new)
}

/// Whether the snapshot lists `owner/repo`, case-insensitively. A lookup
/// reads the mirror only for a listed repository rather than discovering
/// an unlisted one by a mirror 404.
pub fn listed_github_repository(package: &str) -> bool {
    REPOSITORIES.contains(&package.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_repositories_in_the_snapshot_are_listed() {
        for listed in ["FiloSottile/age", "jdx/fnox", "BurntSushi/ripgrep"] {
            assert!(listed_github_repository(listed), "{listed}");
        }
        for unlisted in ["private-org/internal", "age", "unknown/clean-room-tool"] {
            assert!(!listed_github_repository(unlisted), "{unlisted}");
        }
    }
    #[test]
    fn github_sources_name_the_repositories_a_listing_reads() {
        let set = |repos: &[&str]| Some(repos.iter().map(|r| (*r).to_owned()).collect());
        for (tool, sources) in [
            ("github:Org/Tool", set(&["org/tool"])),
            ("ubi:org/tool", set(&["org/tool"])),
            ("aqua:org/tool/cmd", set(&["org/tool"])),
            ("cargo:https://github.com/org/tool.git", set(&["org/tool"])),
            ("core:node", set(&[])),
            ("node", set(&[])),
            ("npm:prettier", set(&[])),
            ("npm:@types/node", set(&[])),
            ("cargo:ripgrep", set(&[])),
            ("pypi:black", set(&[])),
            ("gem:rails", set(&[])),
            // The snapshot lists aqua, asdf and cargo for ripgrep; the
            // plugin backend is never used.
            ("ripgrep", set(&["burntsushi/ripgrep"])),
            ("npm:github:org/tool", None),
            ("npm:@scope/a/b", None),
            ("pipx:org/tool", None),
            ("cargo:https://gitlab.com/org/tool", None),
            ("go:github.com/org/tool", None),
            ("http:tool", None),
            ("spm:org/tool", None),
            ("gitlab:org/tool", None),
            ("unknown-shorthand", None),
        ] {
            assert_eq!(github_sources(tool), sources, "{tool}");
        }
    }
    #[test]
    fn backends_that_name_a_repository() {
        for (backend, repo) in [
            ("github:org/tool", Some("org/tool")),
            ("ubi:Org/Tool", Some("org/tool")),
            ("aqua:org/tool/cmd", Some("org/tool")),
            ("packslip:org/tool", Some("org/tool")),
            ("packslip:github.com/org/tool", Some("org/tool")),
            ("packslip:tool.example.com", None),
            ("cargo:https://github.com/org/tool.git", Some("org/tool")),
            ("cargo:tool", None),
            ("npm:@scope/tool", None),
            ("asdf:org/plugin", None),
        ] {
            assert_eq!(repository(backend).as_deref(), repo, "{backend}");
        }
    }
}
