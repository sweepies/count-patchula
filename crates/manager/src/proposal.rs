//! The facts lookup, the engine and update read from a declaration to
//! propose for it. Each method is one predicate that used to name a manager
//! in a caller; the predicates live here so the callers hold policy, not
//! per-manager rules.
use crate::Dependency;
use patchula_versioning::Ecosystem;
use patchula_words::{DependencyKind, Manager, Source};

impl Dependency {
    /// An action, reusable workflow or APM package looked up by its
    /// repository's tags, on GitHub or on a forge, whose ref can be pinned to
    /// a commit with the tag kept in a comment.
    pub fn tagged_action(&self) -> bool {
        matches!(self.manager, Manager::GithubActions | Manager::Apm)
            && matches!(self.source, Source::GithubTags | Source::ForgeTags)
    }
    /// A git ref its tool checks out by name, so an update writes the upstream
    /// tag exactly as tagged rather than a normalized version.
    pub fn exact_tag_ref(&self) -> bool {
        match self.manager {
            Manager::PreCommit | Manager::Apm => true,
            Manager::Gitlabci | Manager::GitlabciInclude => self.source == Source::ForgeTags,
            _ => false,
        }
    }
    /// Looked up only when the ref is a version: a branch ref has no version
    /// to compare, so only tags are looked up.
    pub fn looked_up_only_as_version(&self) -> bool {
        self.tagged_action()
            || self.manager == Manager::PreCommit
            || matches!(self.manager, Manager::Gitlabci | Manager::GitlabciInclude)
                && self.source == Source::ForgeTags
    }
    /// A proposal for this dependency needs the target's digest.
    pub fn needs_digest(&self) -> bool {
        self.manager == Manager::Apm
            || (self.source == Source::Docker
                || self.source == Source::Oci
                || self.manager == Manager::GithubActions)
                && matches!(
                    self.kind,
                    DependencyKind::Action
                        | DependencyKind::ReusableWorkflow
                        | DependencyKind::ContainerImage
                        | DependencyKind::Chart
                )
    }
    /// A peer declaration expresses consumer compatibility: a range, even
    /// where installed packages pin exact versions.
    pub fn peer_range(&self) -> bool {
        matches!(self.manager, Manager::Npm | Manager::Bun) && self.kind == DependencyKind::Peer
    }
    /// An unclassifiable current value is an opaque ref, not an error.
    pub fn opaque_when_unclassifiable(&self) -> bool {
        self.manager == Manager::GithubActions || self.manager == Manager::PreCommit
    }
    /// A short tag such as `v4` floats along its release line.
    pub fn short_tags_float(&self) -> bool {
        self.manager == Manager::GithubActions && self.tagged_action()
    }
    /// An update writes the tag at the declared precision: `v4` stays a
    /// `v4.y` tag.
    pub fn tag_at_declared_precision(&self) -> bool {
        self.manager == Manager::GithubActions
    }
    /// A provider constraint the release already satisfies is kept as written.
    pub fn keeps_satisfied_constraint(&self) -> bool {
        self.manager == Manager::Terraform && self.kind == DependencyKind::Provider
    }
    /// The catalog's digest travels with the version.
    pub fn digest_from_catalog(&self) -> bool {
        self.manager == Manager::GithubActions || self.tagged_action()
    }
    /// `engines.node` and Terraform's `required_version` declare a supported
    /// runtime range. A version file such as `.nvmrc`, `.node-version` or
    /// `.terraform-version` records one bare version — its scheme cannot express
    /// a range — so it is an installed-version pin proposed like any other.
    pub fn supports_runtime_range(&self) -> bool {
        matches!(
            self.manager,
            Manager::Npm | Manager::Bun | Manager::Terraform
        ) && self.kind == DependencyKind::Toolchain
            && Ecosystem::of(self.scheme, self.source).supports_ranges()
    }
    /// A node or Terraform toolchain declaration: `engines.node`,
    /// `required_version`, or a version file such as `.nvmrc`.
    pub fn runtime_toolchain(&self) -> bool {
        matches!(
            self.manager,
            Manager::Terraform | Manager::Npm | Manager::Bun
        ) && self.kind == DependencyKind::Toolchain
    }
    /// A selector resolved against the listing: mise's tools, and the setup
    /// tools GitHub Actions runs.
    pub fn selector_resolved(&self) -> bool {
        self.manager == Manager::Mise
            || (self.manager == Manager::GithubActions && self.kind == DependencyKind::Tool)
    }
    /// mise entries and every `tool` kind resolve a selector against the
    /// listing.
    pub fn resolved_via_listing(&self) -> bool {
        self.manager == Manager::Mise || self.kind == DependencyKind::Tool
    }
    /// A transitive Cargo lock entry can move to an exact version of the one
    /// package.
    pub fn transitive_lock_movable(&self) -> bool {
        self.kind == DependencyKind::Transitive && self.source == Source::Crates
    }
    /// copier applies the template itself: the dependency is reported, never
    /// edited.
    pub fn report_only(&self) -> bool {
        self.manager == Manager::Copier
    }
    /// The `resolved` version could only have come from a committed lockfile:
    /// `movable_lockfile` names each declaration its committed lockfile
    /// records a resolved version for — Cargo.lock, uv.lock, poetry.lock,
    /// Gemfile.lock, composer.lock and go.mod's `require` versions. npm, Bun
    /// and Terraform's descriptors name no movable lock, but their parsers
    /// fill `resolved` only from a committed one: package-lock.json, bun.lock,
    /// aube-lock.yaml or .terraform.lock.hcl.
    pub fn resolved_from_lockfile(&self) -> bool {
        !matches!(self.kind, DependencyKind::Toolchain | DependencyKind::Tool)
            && self.resolved.is_some()
            && (self.movable_lockfile()
                || matches!(
                    self.manager,
                    Manager::Npm | Manager::Bun | Manager::Terraform
                ))
    }
    /// A nix flake input moves by relocking its lock: no manifest edit, and
    /// each lock's inputs move on their own branch.
    pub fn relocks(&self) -> bool {
        self.manager == Manager::Nix
    }
    /// The advisory identity the source does not give: an action's
    /// advisories are GitHub Actions'.
    pub fn github_actions_advisories(&self) -> bool {
        self.manager == Manager::GithubActions
            && matches!(
                self.kind,
                DependencyKind::Action | DependencyKind::ReusableWorkflow
            )
    }
    /// The advisory identity the source does not give: the `go` toolchain's
    /// advisories are Go's stdlib's.
    pub fn go_stdlib_advisories(&self) -> bool {
        self.manager == Manager::Go
            && self.kind == DependencyKind::Toolchain
            && self.package == "go"
    }
}
