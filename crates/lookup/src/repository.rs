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

/// A tool the Count would look up, without options, whose backends, as the
/// compiled-in registry snapshot lists them, read exactly one GitHub
/// repository; an explicit `github:` or `ubi:` backend naming exactly
/// `<owner>/<repo>`; or an explicit `go:github.com/<owner>/<repo>/...` tool.
fn mise_repository(tool: &str) -> Option<String> {
    // A refused tool is never looked up, and its options may have been
    // dropped while reading it, so it says nothing about its repository.
    if patchula_mise::check(tool).is_err() || patchula_mise::split(tool).1.is_some() {
        return None;
    }
    if let Some(module) = tool.strip_prefix("go:") {
        return go_repository(module);
    }
    if let Some(("github" | "ubi", rest)) = tool.split_once(':')
        && rest.split('/').count() != 2
    {
        return None;
    }
    let sources = patchula_mise::github_sources(tool)?;
    let mut sources = sources.iter();
    let (Some(repository), None) = (sources.next(), sources.next()) else {
        return None;
    };
    match repository.split('/').collect::<Vec<_>>()[..] {
        [owner, repo] => owner_repo(owner, repo),
        _ => None,
    }
}

/// What a dependency's own policy says about joining its repository's
/// group, for one update kind.
pub(crate) struct Candidate {
    /// Not ignored, active, able to share a pull request, and no group set
    /// by defaults or a rule, `""` included.
    pub ungrouped: bool,
    pub commit_prefix: String,
}

/// The update kinds the planner puts on one branch: majors on their own,
/// minors, patches and digests together.
const TRACKS: [(bool, &[Update]); 2] = [
    (true, &[Update::Major]),
    (false, &[Update::Minor, Update::Patch, Update::Digest]),
];

/// The kinds on `kind`'s branch. Pins never group.
fn track(kind: Update) -> Option<&'static [Update]> {
    let major = match kind {
        Update::Pin => return None,
        kind => kind == Update::Major,
    };
    TRACKS
        .into_iter()
        .find_map(|(m, kinds)| (m == major).then_some(kinds))
}

/// The commit prefix every member on a track shares: `Some` when a
/// dependency is a candidate for every kind on the track, all with one
/// prefix.
fn member_prefix(
    kinds: &[Update],
    mut candidate: impl FnMut(Update) -> Option<Candidate>,
) -> Option<String> {
    let mut prefix = None::<String>;
    for kind in kinds {
        let c = candidate(*kind).filter(|c| c.ungrouped)?;
        match &prefix {
            Some(p) if *p != c.commit_prefix => return None,
            _ => prefix = Some(c.commit_prefix),
        }
    }
    prefix
}

/// The groups that form, by repository and track (`true` for majors), with
/// the one commit prefix their members share.
#[derive(Default)]
pub(crate) struct Groups(BTreeMap<(String, bool), String>);

impl Groups {
    /// Find the groups among `dependencies`; `candidate` gives a
    /// dependency's policy for a kind, or `None` when it cannot be
    /// evaluated, which keeps it out.
    ///
    /// A repository's track forms a group when Pkl packages and other
    /// dependencies of it are both members there, and every member writes
    /// the same commit prefix, so the group never holds a branch for
    /// conflicting prefixes.
    pub fn collect<'a>(
        dependencies: impl IntoIterator<Item = &'a Dependency>,
        mut candidate: impl FnMut(&Dependency, Update) -> Option<Candidate>,
    ) -> Self {
        // A group needs a Pkl package on one side, so a repository without
        // one forms none, and no dependency's policy is asked for.
        let dependencies: Vec<&Dependency> = dependencies.into_iter().collect();
        if !dependencies.iter().any(|dep| dep.source == Source::Pkl) {
            return Self::default();
        }
        let mut found = BTreeMap::<String, Vec<&Dependency>>::new();
        for dep in dependencies {
            if let Some(repository) = declared_repository(dep) {
                found.entry(repository).or_default().push(dep);
            }
        }
        let mut groups = BTreeMap::new();
        for (repository, deps) in found {
            for (major, kinds) in TRACKS {
                let mut sides = [false; 2];
                let mut prefixes = BTreeSet::new();
                for dep in &deps {
                    if let Some(prefix) = member_prefix(kinds, |kind| candidate(dep, kind)) {
                        sides[side(dep)] = true;
                        prefixes.insert(prefix);
                    }
                }
                // Pkl packages alone (a pantry) or other dependencies alone
                // (a Go module and its mise tool) form no group.
                if sides == [true, true] && prefixes.len() == 1 {
                    groups.insert(
                        (repository.clone(), major),
                        prefixes.pop_first().expect("one prefix"),
                    );
                }
            }
        }
        Self(groups)
    }

    /// The group `dep` joins for `kind`: its repository, when that track
    /// formed a group and `dep` is a member of it.
    pub fn group(
        &self,
        dep: &Dependency,
        kind: Update,
        candidate: impl FnMut(Update) -> Option<Candidate>,
    ) -> Option<String> {
        let kinds = track(kind)?;
        if self.0.is_empty() {
            return None;
        }
        let repository = declared_repository(dep)?;
        let prefix = self.0.get(&(repository.clone(), kind == Update::Major))?;
        (member_prefix(kinds, candidate).as_ref() == Some(prefix)).then_some(repository)
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
            // Refused tools, and backends naming no canonical owner/repo.
            (Source::Mise, "github:jdx/hk/sub@1", None),
            (
                Source::Mise,
                "go:github.com/apple/pkl-go/cmd/pkl-gen-go@1",
                None,
            ),
            (Source::Mise, "github:jdx/..", None),
            (Source::Mise, "github:jdx/hk?x=1", None),
            (Source::Mise, "github:jdx/hk/sub", None),
            (Source::Mise, "ubi:jdx/hk/sub", None),
            (Source::Mise, "-hk", None),
            (Source::Mise, "aqua:jdx/hk", Some("jdx/hk")),
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

    /// Without a Pkl package no group can form, so no dependency's policy
    /// is asked for; with one, the policy decides.
    #[test]
    fn a_repository_without_pkl_packages_asks_no_policy() {
        let candidate = || Candidate {
            ungrouped: true,
            commit_prefix: "chore(deps)".into(),
        };
        let others = [
            dep(Source::Go, "github.com/jdx/hk"),
            dep(Source::Mise, "hk"),
        ];
        let mut asked = 0;
        let groups = Groups::collect(&others, |_, _| {
            asked += 1;
            Some(candidate())
        });
        assert_eq!(asked, 0);
        assert!(groups.0.is_empty());
        let with_pkl = [
            dep(Source::Pkl, "pkg.pkl-lang.org/github.com/jdx/hk/hk"),
            dep(Source::Go, "github.com/jdx/hk"),
        ];
        let groups = Groups::collect(&with_pkl, |_, _| Some(candidate()));
        assert!(!groups.0.is_empty());
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
