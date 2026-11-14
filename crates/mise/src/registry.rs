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
