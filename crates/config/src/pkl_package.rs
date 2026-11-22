//! Pkl packages as the `pkl` source names them: the package URI without
//! its scheme and version, which says which GitHub repository publishes the
//! package and how its release tags are spelled.
//!
//! - `pkg.pkl-lang.org/<project>/<name>`: `apple/<project>`, tags
//!   `<name>@%version%`
//! - `pkg.pkl-lang.org/github.com/<owner>/<repo>/<name>`: `<owner>/<repo>`,
//!   tags `<name>@%version%`
//! - `github.com/<owner>/<repo>/releases/download/<tag>/<name>`, the tag
//!   with its version written `%version%`: `<owner>/<repo>`, tags spelled
//!   like `<tag>`
//!
//! The `pkl` manager writes these names and the datasource and release notes
//! read them, so all three share this one reading.
/// Where a GitHub release URI's package name writes its tag's version. A
/// tag may not hold `%` (a percent-encoded tag never matches GitHub's own),
/// so no tag holds it, and it means nothing to Markdown, HTML, globs or
/// regular expressions, so a package name renders and selects as written.
pub const VERSION_PLACEHOLDER: &str = "%version%";

/// A semantic version as Pkl requires a package's version to be, read
/// with the parser the `semver` scheme compares versions with.
pub fn pkl_semver(version: &str) -> bool {
    semver::Version::parse(version).is_ok()
}

/// A Pkl package name or release tag: letters, digits and `._+@-`, never `.`
/// or `..`. Anything else, escapes and percent-encoding included, is not
/// read, so names reach branch names, reports and PR bodies as plain text.
pub fn pkl_segment(part: &str) -> bool {
    !part.is_empty()
        && !matches!(part, "." | "..")
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+@-".contains(&b))
}

/// A GitHub owner or repository name.
fn github_segment(part: &str) -> bool {
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
            // `github.com` names the hosted form, which needs an owner and a
            // repository; it is no Apple project.
            ["pkg.pkl-lang.org", project, name] if *project != "github.com" => {
                ("apple", *project, format!("{name}@"), String::new())
            }
            ["github.com", owner, repo, "releases", "download", tag, _] => {
                let (prefix, suffix) = tag.split_once(VERSION_PLACEHOLDER)?;
                let affix = |part: &str| part.is_empty() || pkl_segment(part);
                if !affix(prefix) || !affix(suffix) {
                    return None;
                }
                (*owner, *repo, prefix.to_owned(), suffix.to_owned())
            }
            _ => return None,
        };
        let name_ok = parts
            .last()
            .is_some_and(|name| pkl_segment(name) && !name.contains('@'));
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
            PklPackage::parse("github.com/jdx/hk/releases/download/v%version%-final/hk").unwrap();
        assert_eq!(release.repository, "jdx/hk");
        assert_eq!(release.version("v2.11.1-final"), Some("2.11.1"));
        assert_eq!(release.version("v2.11.1"), None);
        assert_eq!(release.version("v2.11-final"), None);
        // Versions are read as the semver scheme reads them.
        assert_eq!(release.version("v1.0.0-01-final"), None);
        assert_eq!(release.version("v99999999999999999999.0.0-final"), None);
    }

    #[test]
    fn other_names_are_not_packages() {
        for package in [
            "jdx/hk",
            "github.com/jdx/hk/releases/download/v2.9.0/hk",
            "github.com/jdx/hk/releases/download/%version%%version%/hk",
            "github.com/jdx/hk/releases/download/v%version%%2B/hk",
            "pkg.pkl-lang.org/pkl-pantry/..",
            "pkg.pkl-lang.org/pkl-pantry/a|b",
            "pkg.pkl-lang.org/pkl-pantry/a\nb",
            "pkg.pkl-lang.org/pkl-pantry/a\u{1b}[31m",
            "github.com/jdx/hk/releases/download/v%version%/",
            "github.com/../hk/releases/download/v%version%/hk",
            "pkg.pkl-lang.org/pkl-pantry",
            "pkg.pkl-lang.org/github.com/foo",
            "pkg.pkl-lang.org/pkl-pantry/pkl.toml@1.0.0",
            "example.com/a/b",
        ] {
            assert_eq!(PklPackage::parse(package), None, "{package}");
        }
    }
}
