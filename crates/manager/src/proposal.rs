//! The facts lookup, the engine and update read from a declaration to
//! propose for it. Each ecosystem's descriptor states them through
//! `Ecosystem::proposing`, as the fields where its declarations differ from
//! [`Proposing::BASE`].
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

impl Proposing {
    /// A plain manifest declaration: an installed-version range rewritten
    /// in place, looked up as a version, with no pin, lockfile or advisory
    /// fact of its own. A descriptor states only what differs.
    pub const BASE: Proposing = Proposing {
        range: Range::Installed,
        runtime_toolchain: false,
        written: Written::Range,
        keeps_satisfied: false,
        unversioned: Unversioned {
            looked_up: false,
            opaque: false,
        },
        short_tags: ShortTags::Fixed,
        selector_resolved: false,
        resolved_via_listing: false,
        pin: Pin {
            commit: false,
            needs_digest: false,
            from_catalog: false,
        },
        lock: Lock {
            keeps_ranges: false,
            movable: false,
            transitive_movable: false,
            reported: false,
        },
        edits: Edits::Manifest,
        advisories: Advisories::BySource,
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dependency as dep;
    use patchula_words::{DependencyKind, Manager, Scheme, Source};

    fn facts(manager: Manager, kind: DependencyKind, source: Source) -> Proposing {
        dep(manager, kind, source).proposing()
    }

    /// Manifest declarations with no fact of their own propose as the base.
    #[test]
    fn plain_declarations_are_the_base() {
        use DependencyKind::*;
        for (manager, kind, source) in [
            (Manager::Maven, Production, Source::Maven),
            (Manager::Gradle, Build, Source::Maven),
            (Manager::Nuget, Production, Source::Nuget),
            (Manager::Helm, Chart, Source::Helm),
            (Manager::Regex, Production, Source::Npm),
            (Manager::Woodpecker, Production, Source::Docker),
        ] {
            assert_eq!(
                facts(manager, kind, source),
                Proposing::BASE,
                "{manager} {kind} {source}"
            );
        }
    }

    /// Copier is reported, never edited, and a nix flake input relocks.
    #[test]
    fn copier_reports_and_nix_relocks() {
        let copier = facts(
            Manager::Copier,
            DependencyKind::Template,
            Source::GithubTags,
        );
        assert_eq!(copier.edits, Edits::ReportOnly);
        assert_eq!(
            Proposing {
                edits: Edits::Manifest,
                ..copier
            },
            Proposing::BASE
        );
        let nix = facts(Manager::Nix, DependencyKind::FlakeInput, Source::Nix);
        assert_eq!(nix.edits, Edits::Relock);
    }

    /// An action tag pins to a commit, floats along its release line,
    /// writes at the declared precision and takes GitHub Actions'
    /// advisories; a setup tool resolves a selector against its listing.
    #[test]
    fn actions_pin_float_and_resolve_setup_tools() {
        let action = facts(
            Manager::GithubActions,
            DependencyKind::Action,
            Source::GithubTags,
        );
        assert!(action.pin.commit && action.pin.needs_digest && action.pin.from_catalog);
        assert_eq!(action.short_tags, ShortTags::Floating);
        assert_eq!(action.written, Written::TagAtPrecision);
        assert!(action.unversioned.looked_up && action.unversioned.opaque);
        assert_eq!(action.advisories, Advisories::GithubAction);
        let tool = facts(
            Manager::GithubActions,
            DependencyKind::Tool,
            Source::GithubReleases,
        );
        assert_eq!(tool.written, Written::VersionAtPrecision);
        assert!(tool.selector_resolved && tool.resolved_via_listing);
        assert!(!tool.pin.commit && !tool.pin.needs_digest);
        assert_eq!(tool.short_tags, ShortTags::Fixed);
        assert_eq!(tool.advisories, Advisories::BySource);
        let image = facts(
            Manager::GithubActions,
            DependencyKind::ContainerImage,
            Source::Docker,
        );
        assert!(image.pin.needs_digest && !image.pin.commit);
    }

    /// Refs a tool checks out by name are written exactly as tagged.
    #[test]
    fn checked_out_refs_write_the_exact_tag() {
        let apm = facts(Manager::Apm, DependencyKind::Production, Source::GithubTags);
        assert_eq!(apm.written, Written::Tag);
        assert!(apm.pin.commit && apm.pin.needs_digest && apm.pin.from_catalog);
        assert!(apm.unversioned.looked_up);
        let hook = facts(Manager::PreCommit, DependencyKind::Tool, Source::GithubTags);
        assert_eq!(hook.written, Written::Tag);
        assert!(hook.unversioned.looked_up && hook.unversioned.opaque);
        assert!(!hook.pin.commit);
        let include = facts(
            Manager::GitlabciInclude,
            DependencyKind::ReusableWorkflow,
            Source::ForgeTags,
        );
        assert_eq!(include.written, Written::Tag);
        assert!(include.unversioned.looked_up);
        let image = facts(
            Manager::Gitlabci,
            DependencyKind::ContainerImage,
            Source::Docker,
        );
        assert_eq!(image.written, Written::Range);
        assert!(image.pin.needs_digest && !image.unversioned.looked_up);
    }

    /// Image and OCI chart references need the target's digest; a chart
    /// from a Helm repository and a devcontainer feature do not.
    #[test]
    fn images_and_oci_charts_need_a_digest() {
        use DependencyKind::*;
        for (manager, kind, source, needs) in [
            (Manager::Dockerfile, ContainerImage, Source::Docker, true),
            (Manager::Compose, ContainerImage, Source::Docker, true),
            (Manager::Woodpecker, ContainerImage, Source::Docker, true),
            (Manager::Devcontainer, ContainerImage, Source::Docker, true),
            (Manager::Devcontainer, Tool, Source::Docker, false),
            (Manager::Flux, Chart, Source::Oci, true),
            (Manager::Helm, Chart, Source::Oci, true),
            (Manager::Helm, Chart, Source::Helm, false),
        ] {
            assert_eq!(
                facts(manager, kind, source).pin.needs_digest,
                needs,
                "{manager} {kind} {source}"
            );
        }
    }

    /// mise entries and every tool kind resolve a selector against the
    /// listing.
    #[test]
    fn tools_resolve_through_the_listing() {
        let mise = facts(Manager::Mise, DependencyKind::Tool, Source::Mise);
        assert!(mise.selector_resolved && mise.resolved_via_listing);
        for manager in [Manager::Npm, Manager::Nuget, Manager::Devcontainer] {
            let tool = facts(manager, DependencyKind::Tool, Source::GithubReleases);
            assert!(tool.resolved_via_listing, "{manager}");
            assert!(!tool.selector_resolved, "{manager}");
        }
    }

    /// Ecosystems with a committed lockfile keep their ranges behind it and
    /// move it; only Cargo moves a transitive entry.
    #[test]
    fn lockfile_ecosystems_keep_ranges_and_move_their_locks() {
        use DependencyKind::*;
        for (manager, source) in [
            (Manager::Cargo, Source::Crates),
            (Manager::Bundler, Source::Rubygems),
            (Manager::Composer, Source::Packagist),
            (Manager::Pyproject, Source::Pypi),
        ] {
            let lock = facts(manager, Production, source).lock;
            assert!(
                lock.keeps_ranges && lock.movable && lock.reported,
                "{manager}"
            );
            assert_eq!(lock.locked(Some("1.2.3")), Some("1.2.3"), "{manager}");
        }
        let python = facts(Manager::Pyproject, Toolchain, Source::Mise).lock;
        assert!(python.keeps_ranges && !python.movable && !python.reported);
        assert_eq!(python.locked(Some("3.12.0")), None);
        assert!(
            facts(Manager::Cargo, Transitive, Source::Crates)
                .lock
                .transitive_movable
        );
        assert!(
            !facts(Manager::Npm, Transitive, Source::Npm)
                .lock
                .transitive_movable
        );
        let go = facts(Manager::Go, Production, Source::Go).lock;
        assert!(go.movable && go.reported && !go.keeps_ranges);
    }

    /// npm, Bun and Terraform fill `resolved` only from a committed lock.
    #[test]
    fn node_and_terraform_report_only_what_a_lock_resolved() {
        for manager in [Manager::Npm, Manager::Bun, Manager::Terraform] {
            let mut declared = dep(manager, DependencyKind::Production, Source::Npm);
            assert!(declared.proposing().lock.reported, "{manager}");
            declared.resolved = None;
            assert!(!declared.proposing().lock.reported, "{manager}");
            let lock = declared.proposing().lock;
            assert!(!lock.keeps_ranges && !lock.movable, "{manager}");
        }
    }

    /// A peer declaration and a runtime toolchain range read as ranges; a
    /// version file's bare version is an installed pin.
    #[test]
    fn peers_and_runtime_toolchains_read_as_ranges() {
        assert_eq!(
            facts(Manager::Npm, DependencyKind::Peer, Source::Npm).range,
            Range::Peer
        );
        let engines = facts(Manager::Npm, DependencyKind::Toolchain, Source::Npm);
        assert_eq!(engines.range, Range::Support);
        assert!(engines.runtime_toolchain);
        let mut nvmrc = dep(Manager::Npm, DependencyKind::Toolchain, Source::Npm);
        nvmrc.scheme = Scheme::Loose;
        assert_eq!(nvmrc.proposing().range, Range::Installed);
        assert!(nvmrc.proposing().runtime_toolchain);
        let mut required = dep(
            Manager::Terraform,
            DependencyKind::Toolchain,
            Source::GithubReleases,
        );
        required.scheme = Scheme::Hashicorp;
        assert_eq!(required.proposing().range, Range::Support);
        let provider = facts(
            Manager::Terraform,
            DependencyKind::Provider,
            Source::Terraform,
        );
        assert!(provider.keeps_satisfied && !provider.runtime_toolchain);
    }

    /// The `go` toolchain's advisories are Go's stdlib's.
    #[test]
    fn the_go_toolchain_takes_stdlib_advisories() {
        let mut go = dep(Manager::Go, DependencyKind::Toolchain, Source::Mise);
        go.package = "go".into();
        assert_eq!(go.proposing().advisories, Advisories::GoStdlib);
        go.package = "toolchain".into();
        assert_eq!(go.proposing().advisories, Advisories::BySource);
    }
}
