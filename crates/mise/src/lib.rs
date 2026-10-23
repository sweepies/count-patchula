//! Pure resolution of published mise registry shorthands and backend identities.
#![forbid(unsafe_code)]
use patchula_config::Source;
use std::{collections::BTreeMap, sync::LazyLock};

pub const REGISTRY_JSON: &str = include_str!("../registry.json");
static REGISTRY: LazyLock<BTreeMap<String, Vec<String>>> =
    LazyLock::new(|| serde_json::from_str(REGISTRY_JSON).expect("published registry snapshot"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub source: Source,
    pub package: String,
    pub extract: Option<String>,
}

/// A shorthand follows the first backend whose release metadata we support.
/// Unsupported plugins remain explicit; plugin code is never executed.
pub fn resolve(name: &str) -> Option<Resolution> {
    if name.contains(':') {
        backend(name)
    } else {
        REGISTRY.get(name)?.iter().find_map(|b| backend(b))
    }
}

/// The versions host publishes the declared first backend of registry tools.
pub fn versions_tool(name: &str) -> Option<String> {
    if REGISTRY.contains_key(name) {
        backend(REGISTRY.get(name)?.first()?)?;
        return Some(name.into());
    }
    let short = name.strip_prefix("core:")?;
    REGISTRY
        .get(short)?
        .first()
        .filter(|backend| backend.as_str() == name)?;
    Some(short.into())
}

/// Route authenticated non-registry repositories directly, including private
/// upstreams. Never discover that exception by retrying a mirror 404.
pub fn listed_github_repository(package: &str) -> bool {
    REGISTRY
        .values()
        .flatten()
        .filter_map(|name| backend(name))
        .any(|r| {
            matches!(r.source, Source::GithubReleases | Source::GithubTags)
                && r.package.eq_ignore_ascii_case(package)
        })
}
fn backend(name: &str) -> Option<Resolution> {
    let (backend, package) = name.split_once(':')?;
    let mut extract = None;
    let (source, package) = match backend {
        "npm" => (Source::Npm, package),
        "cargo" if package.starts_with("https://github.com/") => (
            Source::GithubTags,
            package
                .strip_prefix("https://github.com/")?
                .trim_end_matches(".git"),
        ),
        "cargo" => (Source::Crates, package),
        "pypi" => (Source::Pypi, package),
        "github" | "ubi" => (Source::GithubReleases, package),
        "aqua" => {
            extract = Some("^v?(?<version>.+)$".into());
            (Source::GithubReleases, package)
        }
        // A packslip source is `owner/repo` on GitHub, or a host-qualified path.
        "packslip" => match package.strip_prefix("github.com/") {
            Some(package) => (Source::GithubReleases, package),
            None if package.split('/').next()?.contains('.') => return None,
            None => (Source::GithubReleases, package),
        },
        "core" => match package {
            "node" => (Source::Mise, "node"),
            "bun" => {
                extract = Some("^bun-v(?<version>.+)$".into());
                (Source::GithubReleases, "oven-sh/bun")
            }
            "go" => {
                extract = Some("^go(?<version>.+)$".into());
                (Source::GithubTags, "golang/go")
            }
            "rust" => (Source::GithubReleases, "rust-lang/rust"),
            "python" => (Source::GithubTags, "python/cpython"),
            "deno" => (Source::GithubReleases, "denoland/deno"),
            "zig" => (Source::GithubTags, "ziglang/zig"),
            "ruby" => {
                extract = Some("^v(?<version>.+)$".into());
                (Source::GithubReleases, "ruby/ruby")
            }
            _ => return None,
        },
        _ => return None,
    };
    // Backend selectors may include an executable subpath (e.g. aqua).
    let package = if matches!(source, Source::GithubReleases | Source::GithubTags) {
        let mut parts = package.split('/');
        let owner = parts.next()?;
        let repo = parts.next()?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        format!("{owner}/{repo}")
    } else {
        package.into()
    };
    // Product-specific release namespaces, observed in public release metadata.
    // A generic v-prefix cannot distinguish these from SDK/language releases.
    if matches!(source, Source::GithubReleases | Source::GithubTags) {
        extract = match package.as_str() {
            "jqlang/jq" => Some("^jq-(?<version>.+)$".into()),
            "bitwarden/sdk-sm" => Some("^bws-v(?<version>.+)$".into()),
            _ => extract,
        };
    }
    Some(Resolution {
        source,
        package,
        extract,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_and_explicit_backends_resolve_without_executing_plugins() {
        for (name, source, package) in [
            ("age", Source::GithubReleases, "FiloSottile/age"),
            ("fnox", Source::GithubReleases, "jdx/fnox"),
            ("node", Source::Mise, "node"),
            ("core:go", Source::GithubTags, "golang/go"),
            ("ubi:org/tool", Source::GithubReleases, "org/tool"),
            ("aqua:org/tool/cmd", Source::GithubReleases, "org/tool"),
            ("packslip:org/tool", Source::GithubReleases, "org/tool"),
            (
                "packslip:github.com/org/tool",
                Source::GithubReleases,
                "org/tool",
            ),
            ("npm:@scope/tool", Source::Npm, "@scope/tool"),
            ("cargo:spec-tool", Source::Crates, "spec-tool"),
            ("python", Source::GithubTags, "python/cpython"),
            ("rust", Source::GithubReleases, "rust-lang/rust"),
        ] {
            let r = resolve(name).unwrap();
            assert_eq!((r.source, r.package.as_str()), (source, package), "{name}");
        }
        assert!(resolve("unknown-clean-room-tool").is_none());
        assert!(resolve("asdf:org/plugin").is_none());
        assert!(resolve("packslip:gitlab.com/org/tool").is_none());
        assert!(resolve("packslip:packslip.dev").is_none());
        assert_eq!(
            resolve("bun").unwrap().extract.as_deref(),
            Some("^bun-v(?<version>.+)$")
        );
        assert_eq!(
            resolve("jq").unwrap().extract.as_deref(),
            Some("^jq-(?<version>.+)$")
        );
        assert_eq!(
            resolve("bitwarden-secrets-manager")
                .unwrap()
                .extract
                .as_deref(),
            Some("^bws-v(?<version>.+)$")
        );
    }
}
