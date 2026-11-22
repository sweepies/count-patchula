//! Pkl packages as the `pkl` source names them: the package URI without
//! its scheme and version, which says which GitHub repository publishes the
//! package and how its release tags are spelled.
//!
//! - `pkg.pkl-lang.org/<project>/<name>`: `apple/<project>`, tags
//!   `<name>@<version>`
//! - `pkg.pkl-lang.org/github.com/<owner>/<repo>/<name>`: `<owner>/<repo>`,
//!   tags `<name>@<version>`
//! - `github.com/<owner>/<repo>/releases/download/<tag>/<name>`, the tag
//!   with its version written `<version>`: `<owner>/<repo>`, tags spelled
//!   like `<tag>`
//!
//! The `pkl` manager writes these names and the datasource and release notes
//! read them, so all three share this one reading.
use std::sync::LazyLock;

/// Where a GitHub release URI's package name writes its tag's version. `<`
/// and `>` cannot appear unencoded in a URI, so no tag holds it.
pub const VERSION_PLACEHOLDER: &str = "<version>";

/// A semantic version as Pkl requires a package's version to be.
pub fn pkl_semver(version: &str) -> bool {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)",
            r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
        ))
        .expect("semver pattern")
    });
    RE.is_match(version)
}

/// A GitHub owner or repository name.
pub fn github_segment(part: &str) -> bool {
    !part.is_empty()
        && !matches!(part, "." | "..")
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// The release stream a Pkl package is published as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PklPackage {
    /// The GitHub repository, `owner/repo`.
    pub repository: String,
    /// The text before the version in each release tag.
    pub prefix: String,
    /// The text after the version in each release tag.
    pub suffix: String,
}

impl PklPackage {
    /// The stream `package` names, or `None` for a name that is not one of
    /// the three forms.
    pub fn parse(package: &str) -> Option<Self> {
        let parts: Vec<&str> = package.split('/').collect();
        let (owner, repo, prefix, suffix) = match parts.as_slice() {
            ["pkg.pkl-lang.org", "github.com", owner, repo, name] => {
                (*owner, *repo, format!("{name}@"), String::new())
            }
            ["pkg.pkl-lang.org", project, name] => {
                ("apple", *project, format!("{name}@"), String::new())
            }
            ["github.com", owner, repo, "releases", "download", tag, name] => {
                let (prefix, suffix) = tag.split_once(VERSION_PLACEHOLDER)?;
                if suffix.contains(VERSION_PLACEHOLDER) || name.is_empty() {
                    return None;
                }
                (*owner, *repo, prefix.to_owned(), suffix.to_owned())
            }
            _ => return None,
        };
        let name_ok = parts
            .last()
            .is_some_and(|name| !name.is_empty() && !name.contains(['@', '<', '>']));
        (github_segment(owner) && github_segment(repo) && name_ok).then(|| Self {
            repository: format!("{owner}/{repo}"),
            prefix,
            suffix,
        })
    }
    /// The version `tag` releases, when it is one of this package's tags.
    pub fn version<'a>(&self, tag: &'a str) -> Option<&'a str> {
        tag.strip_prefix(self.prefix.as_str())?
            .strip_suffix(self.suffix.as_str())
            .filter(|version| pkl_semver(version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_form_names_its_repository_and_tags() {
        let pantry = PklPackage::parse("pkg.pkl-lang.org/pkl-pantry/pkl.toml").unwrap();
        assert_eq!(pantry.repository, "apple/pkl-pantry");
        assert_eq!(pantry.version("pkl.toml@1.0.3"), Some("1.0.3"));
        assert_eq!(pantry.version("pkl.toml.contrib@1.0.3"), None);
        assert_eq!(pantry.version("k8s@1.0.3"), None);
        let hosted = PklPackage::parse("pkg.pkl-lang.org/github.com/jdx/hk/hk").unwrap();
        assert_eq!(hosted.repository, "jdx/hk");
        assert_eq!(hosted.version("hk@2.0.0-rc.1"), Some("2.0.0-rc.1"));
        let release =
            PklPackage::parse("github.com/jdx/hk/releases/download/v<version>-final/hk").unwrap();
        assert_eq!(release.repository, "jdx/hk");
        assert_eq!(release.version("v2.11.1-final"), Some("2.11.1"));
        assert_eq!(release.version("v2.11.1"), None);
        assert_eq!(release.version("v2.11-final"), None);
    }

    #[test]
    fn other_names_are_not_packages() {
        for package in [
            "jdx/hk",
            "github.com/jdx/hk/releases/download/v2.9.0/hk",
            "github.com/jdx/hk/releases/download/<version><version>/hk",
            "github.com/jdx/hk/releases/download/v<version>/",
            "github.com/../hk/releases/download/v<version>/hk",
            "pkg.pkl-lang.org/pkl-pantry",
            "pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0",
            "example.com/a/b",
        ] {
            assert_eq!(PklPackage::parse(package), None, "{package}");
        }
    }
}
