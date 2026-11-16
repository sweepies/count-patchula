//! Every endpoint whose cached answer the client returns without asking
//! GitHub. Such an answer is served to any credential with the same scope,
//! and the scope leaves permissions out (see `identity`), so each endpoint
//! here must need at most `contents: read` on its repository. A read that
//! skips revalidation on an endpoint missing from this list fails.
use reqwest::Url;

/// One endpoint: its path under the API root, with `{name}` matching any
/// one segment, its exact query, and the permission it needs.
#[derive(Debug)]
pub struct Endpoint {
    pub path: &'static str,
    pub query: Option<&'static str>,
    pub permission: &'static str,
}

/// Git objects named by their SHA, whose answers cannot change. A renamed
/// repository redirects to its `repositories/{id}` form.
pub const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        path: "repos/{owner}/{repo}/git/blobs/{sha}",
        query: None,
        permission: "contents: read",
    },
    Endpoint {
        path: "repos/{owner}/{repo}/git/trees/{sha}",
        query: Some("recursive=1"),
        permission: "contents: read",
    },
    Endpoint {
        path: "repos/{owner}/{repo}/git/commits/{sha}",
        query: None,
        permission: "contents: read",
    },
    Endpoint {
        path: "repositories/{id}/git/blobs/{sha}",
        query: None,
        permission: "contents: read",
    },
    Endpoint {
        path: "repositories/{id}/git/trees/{sha}",
        query: Some("recursive=1"),
        permission: "contents: read",
    },
    Endpoint {
        path: "repositories/{id}/git/commits/{sha}",
        query: None,
        permission: "contents: read",
    },
];

/// Whether `url`, under the API root `root`, is one of `ENDPOINTS`.
pub(crate) fn listed(root: &Url, url: &Url) -> bool {
    if url.origin() != root.origin() || url.fragment().is_some() {
        return false;
    }
    let Some(path) = url.path().strip_prefix(root.path()) else {
        return false;
    };
    let segments: Vec<&str> = path.split('/').collect();
    ENDPOINTS.iter().any(|endpoint| {
        let template: Vec<&str> = endpoint.path.split('/').collect();
        endpoint.query == url.query()
            && template.len() == segments.len()
            && template.iter().zip(&segments).all(|(want, got)| {
                if want.starts_with('{') {
                    !got.is_empty()
                } else {
                    want == got
                }
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn check(path: &str) -> bool {
        let root = Url::parse("https://ghe.example/api/v3/").unwrap();
        listed(&root, &root.join(path).unwrap())
    }

    #[test]
    fn only_listed_endpoints_skip_revalidation() {
        assert!(check("repos/o/r/git/blobs/abc"));
        assert!(check("repos/o/r/git/trees/abc?recursive=1"));
        assert!(check("repos/o/r/git/commits/abc"));
        assert!(check("repositories/12/git/blobs/abc"));
        assert!(!check("repos/o/r/git/trees/abc"));
        assert!(!check("repos/o/r/git/blobs/abc?x=1"));
        assert!(!check("repos/o/r/contents/abc"));
        assert!(!check("repos/o/r/git/blobs"));
        assert!(!check("repos/o/r/git/blobs/abc/more"));
        assert!(!check("repos/o//git/blobs/abc"));
        assert!(!check(
            "https://other.example/api/v3/repos/o/r/git/blobs/abc"
        ));
        for endpoint in ENDPOINTS {
            assert_eq!(endpoint.permission, "contents: read", "{endpoint:?}");
        }
    }

    /// The paths of every `get_immutable` call outside this crate, from the
    /// `format!` literal that follows the call.
    fn immutable_calls() -> Vec<(String, String)> {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found = Vec::new();
        let mut pending: Vec<_> = std::fs::read_dir(&crates)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name() != "github")
            .map(|entry| entry.path().join("src"))
            .filter(|path| path.is_dir())
            .collect();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (at, _) in text.match_indices(".get_immutable") {
                    let after = &text[at..text.len().min(at + 300)];
                    let literal = after
                        .split_once("format!(\"")
                        .and_then(|(_, rest)| rest.split_once('"'))
                        .map(|(literal, _)| literal.to_owned())
                        .unwrap_or_default();
                    found.push((path.display().to_string(), literal));
                }
            }
        }
        found
    }

    #[test]
    fn every_immutable_call_in_the_workspace_reads_a_listed_endpoint() {
        let calls = immutable_calls();
        assert!(!calls.is_empty());
        for (file, literal) in calls {
            // `{repo}` is `owner/name`; every other placeholder one segment.
            let mut path = String::new();
            let mut rest = literal.as_str();
            while let Some((before, after)) = rest.split_once('{') {
                let (name, after) = after.split_once('}').unwrap_or((after, ""));
                path.push_str(before);
                path.push_str(if name == "repo" { "owner/name" } else { "x" });
                rest = after;
            }
            path.push_str(rest);
            assert!(
                !literal.is_empty() && check(&path),
                "{file}: get_immutable reads {literal:?}, which is not in unrevalidated::ENDPOINTS"
            );
        }
    }
}
