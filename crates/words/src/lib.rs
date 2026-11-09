//! The vocabulary every crate speaks.
//!
//! One leaf crate owns the kebab-case words the workspace shares: which
//! manager reads a declaration, which update kind a release is, which rule a
//! tooling warning names. [`words!`] generates an enum whose serde, `Display`,
//! `FromStr` and `as_str` all speak the same word, so a word written in one
//! place can never drift from the word read in another.
#![forbid(unsafe_code)]

/// A word no variant of the enum answers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    /// The enum's name, for the message.
    pub kind: &'static str,
    /// The word that failed to parse.
    pub word: String,
}

impl std::fmt::Display for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown {} \"{}\"", self.kind, self.word)
    }
}

impl std::error::Error for Unknown {}

/// Declare a closed vocabulary: a `Copy` enum whose variants serialize,
/// display and parse as kebab-case words.
///
/// Every variant gets `#[serde(rename = word)]`, an exhaustive
/// `as_str`/`Display`/`FromStr` triple, `ALL` in declaration order (which is
/// also `Ord`), and the usual derives. `FromStr` matches exactly and fails
/// with [`Unknown`].
#[macro_export]
macro_rules! words {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($(#[$vmeta:meta])* $variant:ident => $word:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize,
            serde::Deserialize,
        )]
        pub enum $name {
            $(
                $(#[$vmeta])*
                #[serde(rename = $word)]
                $variant
            ),+
        }

        impl $name {
            /// The word this variant serializes, displays and parses as.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $word),+
                }
            }

            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.pad(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::Unknown;

            fn from_str(word: &str) -> Result<Self, Self::Err> {
                $(if word == $word {
                    return Ok(Self::$variant);
                })+
                Err($crate::Unknown {
                    kind: stringify!($name),
                    word: word.to_owned(),
                })
            }
        }
    };
}

words!(
    /// The tool that reads a declaration.
    Manager {
    Npm => "npm",
    Bun => "bun",
    GithubActions => "github-actions",
    Mise => "mise",
    Cargo => "cargo",
    Dockerfile => "dockerfile",
    Compose => "compose",
    Go => "go",
    Terraform => "terraform",
    Kubernetes => "kubernetes",
    Kustomize => "kustomize",
    Flux => "flux",
    HelmValues => "helm-values",
    Pyproject => "pyproject",
    PreCommit => "pre-commit",
    Helm => "helm",
    Devcontainer => "devcontainer",
    Woodpecker => "woodpecker",
    Copier => "copier",
    Maven => "maven",
    Gradle => "gradle",
    Nuget => "nuget",
    Bundler => "bundler",
    Composer => "composer",
    Gitlabci => "gitlabci",
    GitlabciInclude => "gitlabci-include",
    Nix => "nix",
    Apm => "apm",
    Regex => "regex",
});

words!(
    /// Which package or file format a dependency comes from.
    Source {
    Npm => "npm",
    GithubReleases => "github-releases",
    GithubTags => "github-tags",
    GithubRunners => "github-runners",
    Docker => "docker",
    Crates => "crates",
    Go => "go",
    Terraform => "terraform",
    Pypi => "pypi",
    Helm => "helm",
    Oci => "oci",
    Mise => "mise",
    ForgeTags => "forge-tags",
    Maven => "maven",
    Nuget => "nuget",
    Rubygems => "rubygems",
    Packagist => "packagist",
    Nix => "nix",
});

words!(
    /// The kind of update a release proposes.
    Update {
    Major => "major",
    Minor => "minor",
    Patch => "patch",
    Digest => "digest",
    Pin => "pin",
});

words!(
    /// How strictly a dependency's declared range is kept.
    VersionPolicy {
    Exact => "exact",
    Preserve => "preserve",
    Widen => "widen",
});

words!(
    /// Which release channel a dependency tracks.
    ReleaseChannel {
    Stable => "stable",
    Lts => "lts",
});

words!(
    /// Which versioning scheme a dependency follows.
    Scheme {
    Semver => "semver",
    Docker => "docker",
    Pep440 => "pep440",
    Poetry => "poetry",
    Maven => "maven",
    Nuget => "nuget",
    Rubygems => "rubygems",
    Composer => "composer",
    Go => "go",
    Hashicorp => "hashicorp",
    Loose => "loose",
});

words!(
    /// What kind of dependency a declaration is.
    DependencyKind {
    Production => "production",
    Development => "development",
    Peer => "peer",
    Optional => "optional",
    Build => "build",
    Action => "action",
    ReusableWorkflow => "reusable-workflow",
    Runner => "runner",
    ContainerImage => "container-image",
    Tool => "tool",
    GoDirective => "go-directive",
    Toolchain => "toolchain",
    Provider => "provider",
    Module => "module",
    Chart => "chart",
    Template => "template",
    ReferenceData => "reference-data",
    Transitive => "transitive",
    FlakeInput => "flake-input",
});

words!(
    /// Where a dependency's current value was resolved from.
    ResolutionKind {
    /// The manifest's declared range.
    Declared => "declared",
    /// The lockfile's resolved entry.
    Lockfile => "lockfile",
    /// A pinned commit.
    PinnedCommit => "pinned-commit",
    /// A floating ref such as a branch.
    FloatingRef => "floating-ref",
    /// A release line such as `3.x`.
    ReleaseLine => "release-line",
    /// A supported range.
    SupportedRange => "supported-range",
});

words!(
    /// Why a lookup failed, in the report's words.
    LookupErrorKind {
    /// The registry could not be reached, or answered with a server error.
    Network => "network",
    /// The registry refused the credentials, or asked for some.
    Auth => "auth",
    NotFound => "not-found",
    RateLimit => "rate-limit",
    /// The registry answered with something the Count could not read.
    Parse => "parse",
    Other => "other",
});

words!(
    /// A tooling warning: something about the repository makes an update
    /// that merges itself riskier than it looks. `warnings.quiet` names these.
    ToolingRule {
    /// Nothing tests the PR before it merges.
    MergeWithoutCi => "merge-without-ci",
    /// Native auto-merge is allowed and no status check is required.
    MergeWithoutRequiredChecks => "merge-without-required-checks",
    /// A security fix merges itself and nothing delivers the default branch.
    SecurityMergeWithoutDelivery => "security-merge-without-delivery",
});

words!(
    /// The case of a tooling warning that has its own wording.
    ToolingDetail {
    /// The default branch requires a review but no checks.
    ReviewsOnly => "reviews-only",
    /// Only tags or releases trigger delivery.
    TaggedRelease => "tagged-release",
    /// GitHub's auto-merge, not the Count, merges the PR.
    Native => "native",
});

words!(
    /// An advisory database's severity rating, worst first.
    Severity {
    Critical => "critical",
    High => "high",
    Moderate => "moderate",
    Low => "low",
});

impl Severity {
    /// The rating an advisory database's word names — any case, `medium`
    /// for Moderate — or `None` for a word outside the vocabulary.
    pub fn advisory(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "moderate" | "medium" => Some(Self::Moderate),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// Deserialize an advisory severity leniently: [`Severity::advisory`]'s
/// reading, so a word outside the vocabulary — or none at all — is `None`.
pub fn severity<'de, D>(deserializer: D) -> Result<Option<Severity>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        <Option<String> as serde::Deserialize>::deserialize(deserializer)?
            .as_deref()
            .and_then(Severity::advisory),
    )
}

words!(
    /// When a ticked checkbox says it takes effect: wording only.
    #[derive(Default)]
    Recheck {
    Now => "now",
    #[default]
    NextRun => "next-run",
});

words!(
    /// Where an image's SBOM came from.
    #[derive(Default)]
    SbomSource {
    #[default]
    Attestation => "attestation",
    CosignAtt => "cosign-att",
    CosignSbom => "cosign-sbom",
});

impl Update {
    /// The order competing proposals apply in when several could write the
    /// same declaration: the widest change first, a pin ahead of a bare
    /// digest move. Declaration order (`Ord`) does not give this.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Major => 0,
            Self::Minor => 1,
            Self::Patch => 2,
            Self::Pin => 3,
            Self::Digest => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// Every enum the crate declares round-trips through `as_str`, `FromStr`
    /// and serde, for every member of `ALL`.
    macro_rules! round_trip {
        ($($name:ident),+ $(,)?) => {
            $(
                #[test]
                #[allow(non_snake_case)]
                fn $name() {
                    for &variant in $name::ALL {
                        assert_eq!(variant.as_str().parse::<$name>().unwrap(), variant);
                        assert_eq!(variant.to_string(), variant.as_str());
                        let json = serde_json::to_string(&variant).unwrap();
                        assert_eq!(json, format!("\"{}\"", variant.as_str()));
                        assert_eq!(serde_json::from_str::<$name>(&json).unwrap(), variant);
                    }
                    assert!("".parse::<$name>().is_err());
                    assert!("NOT-A-WORD".parse::<$name>().is_err());
                }
            )+
        };
    }

    round_trip!(
        Manager,
        Source,
        Update,
        VersionPolicy,
        ReleaseChannel,
        Scheme,
        DependencyKind,
        ResolutionKind,
        LookupErrorKind,
        ToolingRule,
        ToolingDetail,
        Severity,
        Recheck,
        SbomSource,
    );

    #[test]
    fn unknown_names_the_enum_and_the_word() {
        let error = Manager::from_str("cargo2").unwrap_err();
        assert_eq!(error.to_string(), "unknown Manager \"cargo2\"");
        assert_eq!(error.kind, "Manager");
        assert_eq!(error.word, "cargo2");
    }

    #[test]
    fn declaration_order_is_ord_and_all() {
        assert!(Update::Major < Update::Minor);
        assert!(Update::Digest < Update::Pin);
        assert_eq!(Update::ALL[0], Update::Major);
        assert_eq!(Update::ALL.len(), 5);
    }

    #[test]
    fn recheck_defaults_to_next_run() {
        assert_eq!(Recheck::default(), Recheck::NextRun);
        assert_eq!("now".parse::<Recheck>().unwrap(), Recheck::Now);
        assert_eq!("next-run".parse::<Recheck>().unwrap(), Recheck::NextRun);
    }
}
