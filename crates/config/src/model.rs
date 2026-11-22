//! Typed, strict representation of the bundled Pkl schema.
use patchula_words::{
    DependencyKind, Manager, ReleaseChannel, Source, Update, VersionPolicy, Window,
};
use serde::{Deserialize, Serialize};

use crate::{ConfigRegex, Filter, Pattern, Patterns};

patchula_words::words!(Rebase { Conflicts => "conflicts", Behind => "behind" });
patchula_words::words!(NpmEngine { Aube => "aube", Npm => "npm" });
patchula_words::words!(PhpBuild { Static => "static", Compiled => "compiled" });

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    pub managers: Vec<Manager>,
    pub defaults: Policy,
    pub rules: Vec<Rule>,
    pub files: Vec<FileSet>,
    pub extractors: Vec<Extractor>,
    pub ignore: Patterns,
    pub exclude: Patterns,
    pub prs: PullRequests,
    pub lockfiles: Lockfiles,
    pub security: Security,
    pub warnings: Warnings,
    pub gitlab: Gitlab,
    pub use_versions_host: bool,
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
    pub permitted: Option<Filter>,
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
    pub permitted: Option<Filter>,
    pub wait_hours: Option<u32>,
    pub windows: Option<Vec<Window>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub packages: Patterns,
    pub dependencies: Patterns,
    pub managers: Vec<Manager>,
    pub files: Patterns,
    pub updates: Vec<Update>,
    pub sources: Vec<Source>,
    pub kinds: Vec<DependencyKind>,
    pub current: Option<Filter>,
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
    pub paths: Patterns,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Extractor {
    pub note: String,
    pub paths: Patterns,
    pub patterns: Vec<ConfigRegex>,
    pub source: String,
    pub dependency: String,
    pub package: String,
    pub scheme: String,
    pub extract: Option<ConfigRegex>,
    pub registry: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gitlab {
    pub host: String,
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Security {
    pub advisories: bool,
    pub label: String,
    pub apply: Overrides,
    pub lowest_fix: bool,
    pub images: bool,
    pub image_platforms: Vec<String>,
    pub quiet_images: std::collections::BTreeMap<Pattern, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Warnings {
    pub quiet: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lockfiles {
    pub refresh: bool,
    pub wait_hours: u32,
    pub group: String,
    pub tidy_go: bool,
    pub npm_engine: NpmEngine,
    pub php: PhpBuild,
}
