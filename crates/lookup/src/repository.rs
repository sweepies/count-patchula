//! The built-in Pkl repository group: a Pkl package shares a pull request
//! with the other dependencies its GitHub repository ships, such as the `hk`
//! mise tool and hk's Pkl package, or the `github.com/apple/pkl-go` Go module
//! and the `pkl.golang` package.
//!
//! A dependency's repository comes only from what the repository itself
//! declares, read syntactically, and only for a public dependency. No
//! registry response names it, so no registry, public or private, can pull
//! a dependency into a group.
use patchula_config::{PklPackage, github_segment};
use patchula_manager::Dependency;
use patchula_words::{Source, Update};
use std::collections::{BTreeMap, BTreeSet};

/// The GitHub repository a public dependency's declaration names, as a
/// lowercased `owner/repo`, or `None` when the declaration alone does not
/// say.
pub fn declared_repository(dep: &Dependency) -> Option<String> {
    if !dep.id().is_public() {
        return None;
    }
    match dep.source {
        Source::Pkl => PklPackage::parse(&dep.package).map(|stream| stream.repository),
        Source::GithubReleases | Source::GithubTags => {
            match dep.package.split('/').collect::<Vec<_>>()[..] {
                [owner, repo] => owner_repo(owner, repo),
                _ => None,
            }
        }
        Source::Go => go_repository(&dep.package),
        Source::Mise => mise_repository(&dep.package),
        _ => None,
    }
    .map(|repository| repository.to_ascii_lowercase())
}

fn owner_repo(owner: &str, repo: &str) -> Option<String> {
    (github_segment(owner) && github_segment(repo)).then(|| format!("{owner}/{repo}"))
}

/// `github.com/<owner>/<repo>` and any path below it. A vanity path would
/// need its `go-get` page, which is a server's claim, so it names nothing.
fn go_repository(module: &str) -> Option<String> {
    let mut parts = module.strip_prefix("github.com/")?.split('/');
    owner_repo(parts.next()?, parts.next()?)
}

/// A tool without options whose backends, as the compiled-in registry
/// snapshot lists them, read exactly one GitHub repository, or an explicit
/// `go:github.com/<owner>/<repo>/...` tool.
fn mise_repository(tool: &str) -> Option<String> {
    if patchula_mise::split(tool).1.is_some() {
        return None;
    }
    if let Some(module) = tool.strip_prefix("go:") {
        return go_repository(module);
    }
    let sources = patchula_mise::github_sources(tool)?;
    match sources.len() {
        1 => sources.into_iter().next(),
        _ => None,
    }
}

/// What a dependency's own policy says about joining its repository's
/// group, for one update kind.
pub(crate) struct Candidate {
    /// Not ignored, active, and no group set by defaults or a rule, `""`
    /// included.
    pub ungrouped: bool,
    pub commit_prefix: String,
}

/// For each repository and update kind, the commit prefixes of the Pkl
/// packages (`[0]`) and of the other dependencies (`[1]`) that could join
/// its group.
#[derive(Default)]
pub(crate) struct Groups(BTreeMap<(String, Update), [BTreeSet<String>; 2]>);

impl Groups {
    /// Collect the candidates among `dependencies`; `candidate` gives a
    /// dependency's policy for a kind, or `None` when it cannot be
    /// evaluated, which keeps it out.
    pub fn collect<'a>(
        dependencies: impl IntoIterator<Item = &'a Dependency>,
        mut candidate: impl FnMut(&Dependency, Update) -> Option<Candidate>,
    ) -> Self {
        let mut found = BTreeMap::<String, Vec<&Dependency>>::new();
        for dep in dependencies {
            if let Some(repository) = declared_repository(dep) {
                found.entry(repository).or_default().push(dep);
            }
        }
        let mut groups = BTreeMap::new();
        for (repository, deps) in found {
            // Pkl packages alone (a pantry) or other dependencies alone (a
            // Go module and its mise tool) form no group.
            if !deps.iter().any(|d| d.source == Source::Pkl)
                || deps.iter().all(|d| d.source == Source::Pkl)
            {
                continue;
            }
            for kind in Update::ALL.iter().copied() {
                let mut sides: [BTreeSet<String>; 2] = Default::default();
                for dep in &deps {
                    if let Some(c) = candidate(dep, kind)
                        && c.ungrouped
                    {
                        sides[side(dep)].insert(c.commit_prefix);
                    }
                }
                groups.insert((repository.clone(), kind), sides);
            }
        }
        Self(groups)
    }

    /// The group `dep` joins for `kind`: its repository, when it is a
    /// candidate itself and a candidate on the other side shares its commit
    /// prefix, so the group never holds for conflicting prefixes.
    pub fn group(&self, dep: &Dependency, kind: Update, own: &Candidate) -> Option<String> {
        if !own.ungrouped || self.0.is_empty() {
            return None;
        }
        let repository = declared_repository(dep)?;
        let sides = self.0.get(&(repository.clone(), kind))?;
        sides[1 - side(dep)]
            .contains(&own.commit_prefix)
            .then_some(repository)
    }
}

fn side(dep: &Dependency) -> usize {
    usize::from(dep.source != Source::Pkl)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(source: Source, package: &str) -> Dependency {
        Dependency {
            package: package.into(),
            ..patchula_manager::testing::dependency(
                patchula_words::Manager::Regex,
                patchula_words::DependencyKind::Production,
                source,
            )
        }
    }

    #[test]
    fn declared_repository_reads_declarations_only() {
        for (source, package, expected) in [
            (
                Source::Pkl,
                "pkg.pkl-lang.org/github.com/jdx/hk/hk",
                Some("jdx/hk"),
            ),
            (
                Source::Pkl,
                "pkg.pkl-lang.org/pkl-go/pkl.golang",
                Some("apple/pkl-go"),
            ),
            (
                Source::Pkl,
                "github.com/Org/Repo/releases/download/v%version%/pkg",
                Some("org/repo"),
            ),
            (Source::Pkl, "example.com/org/pkg", None),
            (Source::Go, "github.com/apple/pkl-go", Some("apple/pkl-go")),
            (
                Source::Go,
                "github.com/apple/pkl-go/v2",
                Some("apple/pkl-go"),
            ),
            (Source::Go, "golang.org/x/net", None),
            (Source::Go, "go.example.com/vanity", None),
            (Source::Mise, "hk", Some("jdx/hk")),
            (Source::Mise, "pkl", Some("apple/pkl")),
            (Source::Mise, "github:jdx/hk", Some("jdx/hk")),
            (Source::Mise, "hk[api_url=https://example.com]", None),
            (Source::Mise, "github:jdx/hk[bin=hk]", None),
            (
                Source::Mise,
                "go:github.com/apple/pkl-go/cmd/pkl-gen-go",
                Some("apple/pkl-go"),
            ),
            (Source::Mise, "asdf:jdx/hk", None),
            (Source::Mise, "node", None),
            (Source::Mise, "no-such-tool-in-the-snapshot", None),
            (Source::GithubReleases, "jdx/hk", Some("jdx/hk")),
            (
                Source::GithubTags,
                "actions/checkout",
                Some("actions/checkout"),
            ),
            (Source::GithubTags, "org/repo/path", None),
            (Source::Npm, "hk", None),
            (Source::Crates, "hk", None),
            (Source::Docker, "jdx/hk", None),
        ] {
            assert_eq!(
                declared_repository(&dep(source, package)).as_deref(),
                expected,
                "{source:?} {package}"
            );
        }
    }

    #[test]
    fn a_registry_qualified_dependency_names_no_repository() {
        for (source, package) in [
            (Source::Pkl, "pkg.pkl-lang.org/github.com/jdx/hk/hk"),
            (Source::Go, "github.com/jdx/hk"),
            (Source::Mise, "hk"),
            (Source::GithubReleases, "jdx/hk"),
        ] {
            let mut d = dep(source, package);
            d.registry = Some("https://private.example".into());
            assert_eq!(declared_repository(&d), None, "{source:?}");
        }
    }
}
