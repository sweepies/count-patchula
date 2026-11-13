//! The facts lookup, the engine and update read from a declaration to
//! propose for it. Each ecosystem's descriptor states them through
//! `Ecosystem::proposing`; the named predicates that used to name a manager
//! in a caller are the oracle under `#[cfg(test)]`, proved equal to the
//! descriptors' answers over the whole manager × kind × source domain.
use crate::Dependency;

/// What lookup, the engine and update must know to propose for a
/// dependency: the proposal facts its ecosystem's descriptor states,
/// computed from the dependency rather than stored on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proposing {
    /// How the declared value reads as a version constraint.
    pub range: Range,
    /// A node or Terraform toolchain declaration: `engines.node`,
    /// `required_version`, or a version file such as `.nvmrc`.
    pub runtime_toolchain: bool,
    /// How an update writes the release into the declaration.
    pub written: Written,
    /// A provider constraint the release already satisfies is kept as
    /// written.
    pub keeps_satisfied: bool,
    /// How a ref that is not a version is looked up and classified.
    pub unversioned: Unversioned,
    /// Whether a short tag such as `v4` floats along its release line.
    pub short_tags: ShortTags,
    /// A selector resolved against the listing: mise's tools, and the setup
    /// tools GitHub Actions runs.
    pub selector_resolved: bool,
    /// mise entries and every `tool` kind resolve a selector against the
    /// listing.
    pub resolved_via_listing: bool,
    /// How a commit pin and digests apply.
    pub pin: Pin,
    /// How the committed lockfile applies.
    pub lock: Lock,
    /// What an update edits.
    pub edits: Edits,
    /// The advisory identity the source does not give.
    pub advisories: Advisories,
}

/// How a dependency's declared value reads as a version constraint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    /// An installed-version pin, proposed like any other.
    Installed,
    /// A supported-runtime range, not an installed pin: `engines.node` and
    /// Terraform's `required_version`.
    Support,
    /// A peer declaration: consumer compatibility, a range even where
    /// installed packages pin exact versions.
    Peer,
}

/// How an update's release is written into the declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    /// The declared constraint rewritten for the release.
    Range,
    /// The upstream tag, exactly as tagged: a git ref the tool checks out by
    /// name rather than a normalized version.
    Tag,
    /// The upstream tag at the declared precision: `v4` stays a `v4.y` tag.
    TagAtPrecision,
    /// The normalized version at the declared precision: a setup tool's
    /// selector tag stays a `v4.y` version.
    VersionAtPrecision,
}

/// How a ref that is not a version is looked up and classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unversioned {
    /// Looked up only when the ref is a version: a branch ref has no
    /// version to compare, so only tags are looked up.
    pub looked_up: bool,
    /// An unclassifiable current value is an opaque ref, not an error.
    pub opaque: bool,
}

/// Whether a short tag such as `v4` floats along its release line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortTags {
    /// A tag names one release.
    Fixed,
    /// A short tag floats: `v4` follows the `v4.y` line.
    Floating,
}

/// How a commit pin and digests apply to a dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    /// An action, reusable workflow or APM package looked up by its
    /// repository's tags, on GitHub or on a forge, whose ref can be pinned
    /// to a commit with the tag kept in a comment.
    pub commit: bool,
    /// A proposal for the dependency needs the target's digest.
    pub needs_digest: bool,
    /// The catalog's digest travels with the version.
    pub from_catalog: bool,
}

/// How a dependency's committed lockfile applies to a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lock {
    /// The ecosystem keeps its declared ranges behind a committed lockfile:
    /// `versions = "preserve"` is its built-in policy and a release the
    /// range already allows is a lockfile move, not a manifest edit.
    pub keeps_ranges: bool,
    /// The committed lockfile records the resolved version for the Count to
    /// move to a chosen version.
    pub movable: bool,
    /// A transitive lock entry can be moved to an exact version of the one
    /// package.
    pub transitive_movable: bool,
    /// The `resolved` version could only have come from a committed
    /// lockfile.
    pub reported: bool,
}

impl Lock {
    /// The version `resolved` is in the committed lockfile when the
    /// declaration keeps its range and the lock records it for the Count to
    /// move: a release inside the declared range reaches the dependency
    /// through its lockfile alone.
    pub fn locked<'a>(&self, resolved: Option<&'a str>) -> Option<&'a str> {
        (self.keeps_ranges && self.movable)
            .then_some(resolved)
            .flatten()
    }
}

/// What an update edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edits {
    /// A surgical edit of the declared value.
    Manifest,
    /// No manifest edit: the tool relocks, and each lock's inputs move on
    /// their own branch.
    Relock,
    /// Reported, never edited.
    ReportOnly,
}

/// The advisory identity the source does not give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advisories {
    /// The source names the advisory ecosystem.
    BySource,
    /// An action's advisories are GitHub Actions'.
    GithubAction,
    /// The `go` toolchain's advisories are Go's stdlib's.
    GoStdlib,
}

impl Dependency {
    /// The proposal facts this dependency's ecosystem states for it.
    pub fn proposing(&self) -> Proposing {
        crate::ecosystem(self.manager).proposing(self)
    }
}

/// The named predicates #257 moved here verbatim, kept under `#[cfg(test)]`
/// as the oracle the descriptor-stated [`Proposing`] fields are proved
/// equal to, plus the two defaulted `Ecosystem` hooks folded into [`Lock`]
/// (one match each over the descriptors that had them). A follow-up PR
/// deletes it.
#[cfg(test)]
pub(crate) mod oracle {
    use super::*;
    use patchula_versioning::Ecosystem;
    use patchula_words::{DependencyKind, Manager, Source};

    /// An action, reusable workflow or APM package looked up by its
    /// repository's tags, on GitHub or on a forge, whose ref can be pinned
    /// to a commit with the tag kept in a comment.
    pub(crate) fn tagged_action(dep: &Dependency) -> bool {
        matches!(dep.manager, Manager::GithubActions | Manager::Apm)
            && matches!(dep.source, Source::GithubTags | Source::ForgeTags)
    }
    /// A git ref its tool checks out by name, so an update writes the
    /// upstream tag exactly as tagged rather than a normalized version.
    pub(crate) fn exact_tag_ref(dep: &Dependency) -> bool {
        match dep.manager {
            Manager::PreCommit | Manager::Apm => true,
            Manager::Gitlabci | Manager::GitlabciInclude => dep.source == Source::ForgeTags,
            _ => false,
        }
    }
    /// Looked up only when the ref is a version: a branch ref has no
    /// version to compare, so only tags are looked up.
    pub(crate) fn looked_up_only_as_version(dep: &Dependency) -> bool {
        tagged_action(dep)
            || dep.manager == Manager::PreCommit
            || matches!(dep.manager, Manager::Gitlabci | Manager::GitlabciInclude)
                && dep.source == Source::ForgeTags
    }
    /// A proposal for this dependency needs the target's digest.
    pub(crate) fn needs_digest(dep: &Dependency) -> bool {
        dep.manager == Manager::Apm
            || (dep.source == Source::Docker
                || dep.source == Source::Oci
                || dep.manager == Manager::GithubActions)
                && matches!(
                    dep.kind,
                    DependencyKind::Action
                        | DependencyKind::ReusableWorkflow
                        | DependencyKind::ContainerImage
                        | DependencyKind::Chart
                )
    }
    /// A peer declaration expresses consumer compatibility: a range, even
    /// where installed packages pin exact versions.
    pub(crate) fn peer_range(dep: &Dependency) -> bool {
        matches!(dep.manager, Manager::Npm | Manager::Bun) && dep.kind == DependencyKind::Peer
    }
    /// An unclassifiable current value is an opaque ref, not an error.
    pub(crate) fn opaque_when_unclassifiable(dep: &Dependency) -> bool {
        dep.manager == Manager::GithubActions || dep.manager == Manager::PreCommit
    }
    /// A short tag such as `v4` floats along its release line.
    pub(crate) fn short_tags_float(dep: &Dependency) -> bool {
        dep.manager == Manager::GithubActions && tagged_action(dep)
    }
    /// An update writes the tag at the declared precision: `v4` stays a
    /// `v4.y` tag.
    pub(crate) fn tag_at_declared_precision(dep: &Dependency) -> bool {
        dep.manager == Manager::GithubActions
    }
    /// A provider constraint the release already satisfies is kept as
    /// written.
    pub(crate) fn keeps_satisfied_constraint(dep: &Dependency) -> bool {
        dep.manager == Manager::Terraform && dep.kind == DependencyKind::Provider
    }
    /// The catalog's digest travels with the version.
    pub(crate) fn digest_from_catalog(dep: &Dependency) -> bool {
        dep.manager == Manager::GithubActions || tagged_action(dep)
    }
    /// `engines.node` and Terraform's `required_version` declare a
    /// supported runtime range. A version file such as `.nvmrc`,
    /// `.node-version` or `.terraform-version` records one bare version —
    /// its scheme cannot express a range — so it is an installed-version
    /// pin proposed like any other.
    pub(crate) fn supports_runtime_range(dep: &Dependency) -> bool {
        matches!(
            dep.manager,
            Manager::Npm | Manager::Bun | Manager::Terraform
        ) && dep.kind == DependencyKind::Toolchain
            && Ecosystem::of(dep.scheme, dep.source).supports_ranges()
    }
    /// A node or Terraform toolchain declaration: `engines.node`,
    /// `required_version`, or a version file such as `.nvmrc`.
    pub(crate) fn runtime_toolchain(dep: &Dependency) -> bool {
        matches!(
            dep.manager,
            Manager::Terraform | Manager::Npm | Manager::Bun
        ) && dep.kind == DependencyKind::Toolchain
    }
    /// A selector resolved against the listing: mise's tools, and the
    /// setup tools GitHub Actions runs.
    pub(crate) fn selector_resolved(dep: &Dependency) -> bool {
        dep.manager == Manager::Mise
            || (dep.manager == Manager::GithubActions && dep.kind == DependencyKind::Tool)
    }
    /// mise entries and every `tool` kind resolve a selector against the
    /// listing.
    pub(crate) fn resolved_via_listing(dep: &Dependency) -> bool {
        dep.manager == Manager::Mise || dep.kind == DependencyKind::Tool
    }
    /// A transitive Cargo lock entry can move to an exact version of the
    /// one package.
    pub(crate) fn transitive_lock_movable(dep: &Dependency) -> bool {
        dep.kind == DependencyKind::Transitive && dep.source == Source::Crates
    }
    /// copier applies the template itself: the dependency is reported,
    /// never edited.
    pub(crate) fn report_only(dep: &Dependency) -> bool {
        dep.manager == Manager::Copier
    }
    /// The descriptor `preserves_ranges` hooks, gathered: Cargo, uv/poetry,
    /// composer and Gemfile ecosystems keep ranges behind their locks.
    pub(crate) fn preserves_ranges(dep: &Dependency) -> bool {
        matches!(
            dep.manager,
            Manager::Cargo | Manager::Bundler | Manager::Composer | Manager::Pyproject
        )
    }
    /// The descriptor `movable_lockfile` hooks, gathered: Cargo.lock,
    /// go.mod, composer.lock, Gemfile.lock and uv/poetry locks record the
    /// resolved version.
    pub(crate) fn movable_lockfile(dep: &Dependency) -> bool {
        match dep.manager {
            Manager::Cargo => dep.source == Source::Crates,
            Manager::Go => dep.source == Source::Go,
            Manager::Composer => dep.source == Source::Packagist,
            Manager::Bundler => dep.source == Source::Rubygems,
            Manager::Pyproject => dep.source == Source::Pypi,
            _ => false,
        }
    }
    /// `Dependency::locked` over the two folded hooks, verbatim.
    pub(crate) fn locked(dep: &Dependency) -> Option<&str> {
        (preserves_ranges(dep) && movable_lockfile(dep))
            .then_some(dep.resolved.as_deref())
            .flatten()
    }
    /// The `resolved` version could only have come from a committed
    /// lockfile: `movable_lockfile` names each declaration its committed
    /// lockfile records a resolved version for — Cargo.lock, uv.lock,
    /// poetry.lock, Gemfile.lock, composer.lock and go.mod's `require`
    /// versions. npm, Bun and Terraform's descriptors name no movable lock,
    /// but their parsers fill `resolved` only from a committed one:
    /// package-lock.json, bun.lock, aube-lock.yaml or .terraform.lock.hcl.
    pub(crate) fn resolved_from_lockfile(dep: &Dependency) -> bool {
        !matches!(dep.kind, DependencyKind::Toolchain | DependencyKind::Tool)
            && dep.resolved.is_some()
            && (movable_lockfile(dep)
                || matches!(
                    dep.manager,
                    Manager::Npm | Manager::Bun | Manager::Terraform
                ))
    }
    /// A nix flake input moves by relocking its lock: no manifest edit, and
    /// each lock's inputs move on their own branch.
    pub(crate) fn relocks(dep: &Dependency) -> bool {
        dep.manager == Manager::Nix
    }
    /// The advisory identity the source does not give: an action's
    /// advisories are GitHub Actions'.
    pub(crate) fn github_actions_advisories(dep: &Dependency) -> bool {
        dep.manager == Manager::GithubActions
            && matches!(
                dep.kind,
                DependencyKind::Action | DependencyKind::ReusableWorkflow
            )
    }
    /// The advisory identity the source does not give: the `go` toolchain's
    /// advisories are Go's stdlib's.
    pub(crate) fn go_stdlib_advisories(dep: &Dependency) -> bool {
        dep.manager == Manager::Go && dep.kind == DependencyKind::Toolchain && dep.package == "go"
    }
}

#[cfg(test)]
mod tests {
    use super::oracle;
    use super::*;
    use crate::{EditForm, Location, Syntax};
    use patchula_words::{DependencyKind, Manager, Scheme, Source};

    /// A minimal declaration for the oracle's axes: only `manager`, `kind`,
    /// `source`, `scheme`, `package` and `resolved` reach a predicate.
    fn dep(
        manager: Manager,
        kind: DependencyKind,
        source: Source,
        scheme: Scheme,
        package: &str,
        resolved: Option<&str>,
    ) -> Dependency {
        Dependency {
            manager,
            file: "manifest".into(),
            location: Location {
                start: 0,
                end: 1,
                line: 1,
                column: 1,
                pointer: String::new(),
            },
            form: EditForm::Scalar(Syntax::Bare),
            name: package.into(),
            package: package.into(),
            kind,
            current: "1.0.0".into(),
            resolved: resolved.map(str::to_owned),
            digest: None,
            source,
            scheme,
            extract: None,
            included_by: None,
            registry: None,
            cooldown: None,
        }
    }

    /// Every field `proposing` answers equals the named predicate the
    /// descriptor replaced, over `Manager::ALL × DependencyKind::ALL ×
    /// Source::ALL`, varying `resolved` (`resolved_from_lockfile` and
    /// `locked` read it), the package name (`go_stdlib_advisories` reads
    /// it), and the scheme where a predicate reads it
    /// (`supports_runtime_range` calls `supports_ranges` on it).
    #[test]
    fn proposing_matches_the_predicates_it_replaced() {
        let mut rows = 0usize;
        for &manager in Manager::ALL {
            for &kind in DependencyKind::ALL {
                for &source in Source::ALL {
                    // The only predicate that reads `scheme` is reachable
                    // only under these managers and this kind; one scheme
                    // stands in for all elsewhere.
                    let schemes: &[Scheme] =
                        if matches!(manager, Manager::Npm | Manager::Bun | Manager::Terraform)
                            && kind == DependencyKind::Toolchain
                        {
                            Scheme::ALL
                        } else {
                            &[Scheme::Semver]
                        };
                    for resolved in [None, Some("1.2.3")] {
                        for package in ["go", "example/package"] {
                            for &scheme in schemes {
                                let dep = dep(manager, kind, source, scheme, package, resolved);
                                let expected = Proposing {
                                    range: if oracle::peer_range(&dep) {
                                        Range::Peer
                                    } else if oracle::supports_runtime_range(&dep) {
                                        Range::Support
                                    } else {
                                        Range::Installed
                                    },
                                    runtime_toolchain: oracle::runtime_toolchain(&dep),
                                    written: if oracle::tag_at_declared_precision(&dep) {
                                        if dep.kind == DependencyKind::Tool {
                                            Written::VersionAtPrecision
                                        } else {
                                            Written::TagAtPrecision
                                        }
                                    } else if oracle::exact_tag_ref(&dep) {
                                        Written::Tag
                                    } else {
                                        Written::Range
                                    },
                                    keeps_satisfied: oracle::keeps_satisfied_constraint(&dep),
                                    unversioned: Unversioned {
                                        looked_up: oracle::looked_up_only_as_version(&dep),
                                        opaque: oracle::opaque_when_unclassifiable(&dep),
                                    },
                                    short_tags: if oracle::short_tags_float(&dep) {
                                        ShortTags::Floating
                                    } else {
                                        ShortTags::Fixed
                                    },
                                    selector_resolved: oracle::selector_resolved(&dep),
                                    resolved_via_listing: oracle::resolved_via_listing(&dep),
                                    pin: Pin {
                                        commit: oracle::tagged_action(&dep),
                                        needs_digest: oracle::needs_digest(&dep),
                                        from_catalog: oracle::digest_from_catalog(&dep),
                                    },
                                    lock: Lock {
                                        keeps_ranges: oracle::preserves_ranges(&dep),
                                        movable: oracle::movable_lockfile(&dep),
                                        transitive_movable: oracle::transitive_lock_movable(&dep),
                                        reported: oracle::resolved_from_lockfile(&dep),
                                    },
                                    edits: if oracle::report_only(&dep) {
                                        Edits::ReportOnly
                                    } else if oracle::relocks(&dep) {
                                        Edits::Relock
                                    } else {
                                        Edits::Manifest
                                    },
                                    advisories: if oracle::github_actions_advisories(&dep) {
                                        Advisories::GithubAction
                                    } else if oracle::go_stdlib_advisories(&dep) {
                                        Advisories::GoStdlib
                                    } else {
                                        Advisories::BySource
                                    },
                                };
                                assert_eq!(
                                    dep.proposing(),
                                    expected,
                                    "{manager} {kind} {source} {scheme} {package} resolved={resolved:?}"
                                );
                                assert_eq!(
                                    dep.proposing().lock.locked(dep.resolved.as_deref()),
                                    oracle::locked(&dep),
                                    "locked: {manager} {kind} {source} {scheme} {package} resolved={resolved:?}"
                                );
                                rows += 1;
                            }
                        }
                    }
                }
            }
        }
        // 9,918 = 29 × 19 × 18 (manager, kind, source) rows, each at two
        // `resolved` and two `package` values; the 54 rows the scheme read
        // is reachable on (3 managers × 1 kind × 18 sources) iterate every
        // scheme instead of one.
        assert_eq!(rows, (9_918 - 54) * 4 + 54 * 4 * Scheme::ALL.len());
    }
}
