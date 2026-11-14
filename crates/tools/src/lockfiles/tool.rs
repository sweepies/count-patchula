//! What the driver needs to know about each lockfile ecosystem: the one
//! descriptor `descriptor()` maps every `Ecosystem` to, declared beside the
//! code that runs it.
use super::*;

/// How an ecosystem's pinned tools become mise install specs, for
/// `tools install` and `tools prune`.
pub(super) enum Install {
    /// Every `tools` entry installs as `name@version` from `defaults.toml`.
    Plain,
    /// The pinned mise itself, installed as `patchula_mise::PACKAGE` so
    /// `mise exec` can run it.
    Mise,
    /// Nothing installs through mise: Rust runs through rustup.
    None,
    /// PHP and Composer install through mise's http backend, not the
    /// registry's `php`/`composer` tools.
    Php,
    /// nix installs as a pinned static build, outside mise's registry.
    Nix,
}

/// How the pinned tools run the ecosystem's binary once `mise exec` has
/// them installed.
#[derive(Clone, Copy)]
pub(super) enum Run {
    /// The binary itself: `mise exec -- tools -- bin args`.
    Direct,
    /// Through rustup: the named tool's pin is the toolchain,
    /// `rustup run <version> bin`, with the run's own Cargo home.
    Rustup(&'static str),
    /// Through `/bin/sh -c`, for a tool whose exit status and streams the
    /// run must keep whole rather than letting the executor turn a nonzero
    /// exit into a bounded failure.
    Shell,
}

/// How far `validate_targets` proves this ecosystem moved a requested
/// target from the regenerated files.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Proof {
    /// The files are trusted as the tool wrote them.
    None,
    /// The manifest must still declare the requested value.
    Declared,
    /// The manifest declares it and the lock records it resolved.
    DeclaredAndResolved,
}

/// How much of a run's output the executor keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Output {
    /// The executor's default limit.
    Default,
    /// Everything, for a tool whose answer is JSON.
    Complete,
}

/// What the lockfile driver needs to know about one ecosystem.
pub(super) struct Tool {
    /// The managers whose files steer this ecosystem's fetches, for the
    /// public-only verdict.
    pub steers: &'static [Manager],
    /// The tool versions its runs resolve through `versions`: every
    /// `defaults.toml` key its invocations ask for. An ecosystem with none
    /// resolves no versions at all.
    pub tools: &'static [&'static str],
    /// Whether a run resolves the repository's tool versions through
    /// `versions`, which validates their pins even for an ecosystem whose
    /// tools ask for none.
    pub versions: bool,
    /// How its pinned tools become mise install specs.
    pub install: Install,
    /// How its binary runs under the pinned tools.
    pub run: Run,
    /// How far `validate_targets` proves its targets from regenerated files.
    pub proof: Proof,
    /// How much of the ecosystem's tool output a run keeps.
    pub output: Output,
}

impl Tool {
    /// The `(tool, version)` specs `tools install` and `tools prune` use
    /// for this ecosystem.
    pub(super) fn install_specs(
        &self,
        defaults: &toml_edit::Document<&'static str>,
    ) -> Vec<(String, String)> {
        match self.install {
            Install::Plain => self
                .tools
                .iter()
                .filter_map(|tool| {
                    Some((
                        (*tool).to_string(),
                        defaults["tools"][*tool].as_str()?.to_string(),
                    ))
                })
                .collect(),
            Install::None => Vec::new(),
            Install::Mise => match defaults["tools"]["mise"].as_str() {
                Some(version) => {
                    vec![(patchula_mise::PACKAGE.to_string(), version.to_string())]
                }
                None => Vec::new(),
            },
            // The static build is the default; compiled PHP is built on
            // first use.
            Install::Php => match (
                defaults["tools"]["php"].as_str(),
                defaults["tools"]["composer"].as_str(),
            ) {
                (Some(php), Some(composer)) => {
                    super::php::tools(patchula_config::PhpBuild::Static, php, composer)
                }
                _ => Vec::new(),
            },
            // Static nix exists for Linux only; elsewhere flake locks fail
            // clearly.
            Install::Nix => super::nix::tools().unwrap_or_default(),
        }
    }
}

/// The descriptor for `ecosystem`; the match is exhaustive, so a new
/// `Ecosystem` variant fails to compile until it has one.
pub(super) fn descriptor(ecosystem: Ecosystem) -> &'static Tool {
    match ecosystem {
        Ecosystem::Node => &super::node::DESCRIPTOR,
        Ecosystem::Cargo => &super::cargo::DESCRIPTOR,
        Ecosystem::Go => &super::go::DESCRIPTOR,
        Ecosystem::Terraform => &super::terraform::DESCRIPTOR,
        Ecosystem::Mise => &super::mise::DESCRIPTOR,
        Ecosystem::Python => &super::python::DESCRIPTOR,
        Ecosystem::GradleWrapper => &super::wrapper::DESCRIPTOR,
        Ecosystem::Dotnet => &super::dotnet::DESCRIPTOR,
        Ecosystem::Ruby => &super::ruby::DESCRIPTOR,
        Ecosystem::Php => &super::php::DESCRIPTOR,
        Ecosystem::Nix => &super::nix::DESCRIPTOR,
        Ecosystem::Apm => &super::apm::DESCRIPTOR,
        Ecosystem::Actions => &super::actions::DESCRIPTOR,
    }
}

/// Every descriptor, in `Ecosystem` order.
pub(super) fn descriptors() -> impl Iterator<Item = &'static Tool> {
    [
        Ecosystem::Node,
        Ecosystem::Cargo,
        Ecosystem::Go,
        Ecosystem::Terraform,
        Ecosystem::Mise,
        Ecosystem::Python,
        Ecosystem::GradleWrapper,
        Ecosystem::Dotnet,
        Ecosystem::Ruby,
        Ecosystem::Php,
        Ecosystem::Nix,
        Ecosystem::Apm,
        Ecosystem::Actions,
    ]
    .into_iter()
    .map(descriptor)
}
