//! Typed, strict representation of the bundled Pkl schema.
use serde::{Deserialize, Serialize};

macro_rules! words {
    ($name:ident { $($variant:ident => $word:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
        pub enum $name { $(#[serde(rename = $word)] $variant),+ }
    };
}
words!(Manager { Npm => "npm", Bun => "bun", GithubActions => "github-actions", Mise => "mise", Cargo => "cargo", Dockerfile => "dockerfile", Compose => "compose", Go => "go", Terraform => "terraform", Kubernetes => "kubernetes", Kustomize => "kustomize", Flux => "flux", HelmValues => "helm-values", Requirements => "requirements", Pyproject => "pyproject", PreCommit => "pre-commit", Helm => "helm", Devcontainer => "devcontainer", Maven => "maven", Gradle => "gradle", Nuget => "nuget", Bundler => "bundler", Regex => "regex" });
words!(Source { Npm => "npm", GithubReleases => "github-releases", GithubTags => "github-tags", GithubRunners => "github-runners", Docker => "docker", Crates => "crates", Go => "go", Terraform => "terraform", Pypi => "pypi", Helm => "helm", Oci => "oci", Mise => "mise", ForgeTags => "forge-tags", Maven => "maven", Nuget => "nuget", Rubygems => "rubygems" });
words!(Update { Major => "major", Minor => "minor", Patch => "patch", Digest => "digest", Pin => "pin" });
words!(VersionPolicy { Exact => "exact", Preserve => "preserve", Widen => "widen" });
words!(ReleaseChannel { Stable => "stable", Lts => "lts" });
words!(Scheme { Semver => "semver", Docker => "docker", Pep440 => "pep440", Poetry => "poetry", Maven => "maven", Nuget => "nuget", Rubygems => "rubygems", Go => "go", Hashicorp => "hashicorp", Loose => "loose" });
words!(Rebase { Conflicts => "conflicts", Behind => "behind" });
words!(NpmEngine { Aube => "aube", Npm => "npm" });
words!(DependencyKind { Production => "production", Development => "development", Peer => "peer", Optional => "optional", Build => "build", Action => "action", ReusableWorkflow => "reusable-workflow", Runner => "runner", ContainerImage => "container-image", Tool => "tool", GoDirective => "go-directive", Toolchain => "toolchain", Provider => "provider", Module => "module", Chart => "chart" });

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    pub managers: Vec<Manager>,
    pub defaults: Policy,
    pub rules: Vec<Rule>,
    pub files: Vec<FileSet>,
    pub extractors: Vec<Extractor>,
    pub ignore: Vec<String>,
    pub exclude: Vec<String>,
    pub prs: PullRequests,
    pub lockfiles: Lockfiles,
    pub use_versions_host: bool,
    pub submodules: bool,
    pub ignore_authors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Policy {
    pub active: bool,
    pub versions: VersionPolicy,
    pub channel: ReleaseChannel,
    pub action_digests: bool,
    pub image_digests: bool,
    pub merge: bool,
    pub group: Option<String>,
    pub labels: Vec<String>,
    pub commit_prefix: String,
    pub release_notes: bool,
    pub body_notes: Vec<String>,
    pub permitted: Option<String>,
    pub wait_hours: u32,
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Overrides {
    pub active: Option<bool>,
    pub versions: Option<VersionPolicy>,
    pub channel: Option<ReleaseChannel>,
    pub digest: Option<bool>,
    pub merge: Option<bool>,
    pub group: Option<String>,
    pub labels: Option<Vec<String>>,
    pub commit_prefix: Option<String>,
    pub release_notes: Option<bool>,
    pub body_notes: Option<Vec<String>>,
    pub permitted: Option<String>,
    pub wait_hours: Option<u32>,
    pub windows: Option<Vec<Window>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub packages: Vec<String>,
    pub dependencies: Vec<String>,
    pub managers: Vec<Manager>,
    pub files: Vec<String>,
    pub updates: Vec<Update>,
    pub sources: Vec<Source>,
    pub kinds: Vec<DependencyKind>,
    pub current: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub note: String,
    pub select: Selector,
    pub apply: Overrides,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileSet {
    pub manager: Manager,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Extractor {
    pub note: String,
    pub paths: Vec<String>,
    pub patterns: Vec<String>,
    pub source: String,
    pub dependency: String,
    pub package: String,
    pub scheme: String,
    pub extract: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PullRequests {
    pub concurrent: Option<u32>,
    pub per_hour: Option<u32>,
    pub dashboard: bool,
    pub branch: String,
    pub rebase: Rebase,
    pub batch_merge: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub cron: Vec<String>,
    pub zone: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lockfiles {
    pub refresh: bool,
    pub wait_hours: u32,
    pub group: String,
    pub tidy_go: bool,
    pub npm_engine: NpmEngine,
}
