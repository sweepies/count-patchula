//! Pure extraction and surgical manifest edits: no tools, network or filesystem access.
#![forbid(unsafe_code)]
mod actions;
mod apm;
mod bundler;
mod copier;
mod edit;
pub use actions::replacement as action_replacement;
pub use actions::runner_replacement;
pub use edit::{Edit, apply, edits};
pub use mise::included_files;
mod cargo;
mod composer;
mod credentials;
pub use credentials::{
    CredentialCheck, committed_credentials, credential_check, credential_reason, credential_scope,
};
mod custom;
mod devcontainer;
mod gitlabci;
mod go;
mod gradle;
mod gradle_vars;
mod helm;
mod images;
mod locked;
mod origins;
mod public;
mod pypi;
pub use locked::Locked;
mod maven;
mod mise;
mod nix;
pub use nix::{
    reference as flake_input_reference, reference_refusal as flake_reference_refusal,
    refusal as flake_input_refusal,
};
mod npm;
mod nuget;
mod precommit;
mod pyproject;
mod terraform;
mod toolchains;
mod woodpecker;
mod yaml;
mod yaml_locations;

use patchula_config::{Config, PackageId, Patterns};
use patchula_words::{DependencyKind, Manager, Scheme, Source};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// UTF-8 byte offsets, end exclusive. Range includes quotes for JSON/TOML.
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    /// Semantic path for formats whose scalars can be escaped or aliased.
    pub pointer: String,
}
impl Location {
    pub(crate) fn new(text: &str, start: usize, end: usize, pointer: String) -> Self {
        Self {
            start,
            end,
            line: text[..start].bytes().filter(|b| *b == b'\n').count() + 1,
            column: start - text[..start].rfind('\n').map_or(0, |n| n + 1) + 1,
            pointer,
        }
    }
}
/// Packages that ship reference data rather than code: CA root bundles, the
/// time zone database and the public suffix list. They are only useful while
/// current, so besides its own kind each is also `reference-data`, which the
/// schema's built-in rule fast-tracks.
const REFERENCE_DATA: &[(Source, &str)] = &[
    (Source::Pypi, "certifi"),
    (Source::Pypi, "tzdata"),
    (Source::Pypi, "pytz"),
    (Source::Pypi, "publicsuffixlist"),
    (Source::Crates, "webpki-roots"),
    (Source::Crates, "webpki-root-certs"),
    (Source::Crates, "chrono-tz"),
    (Source::Crates, "psl"),
    (Source::Go, "golang.org/x/crypto/x509roots/fallback"),
    (Source::Npm, "psl"),
    (Source::Npm, "tldts"),
    (Source::Rubygems, "tzinfo-data"),
    (Source::Rubygems, "public_suffix"),
    (Source::Packagist, "composer/ca-bundle"),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dependency {
    pub manager: Manager,
    pub file: String,
    pub location: Location,
    pub name: String,
    pub package: String,
    pub kind: DependencyKind,
    pub current: String,
    pub resolved: Option<String>,
    pub digest: Option<String>,
    pub source: Source,
    pub scheme: Scheme,
    pub extract: Option<String>,
    /// The mise config that includes this file as a TOML task file.
    pub included_by: Option<String>,
    /// The registry to look the package up in, as an https URL, when it is not
    /// the source's public one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}
/// A key as one token of a JSON Pointer, which escapes `~` and `/`.
pub(crate) fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}
/// A full commit hash: 40 hexadecimal digits.
pub(crate) fn commit_hash(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}
/// A dotted version that is all numbers: safe to compare by length.
pub(crate) fn numeric(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
/// One JSONC document's syntax tree under `options`.
fn jsonc_parse<'a>(
    text: &'a str,
    options: &jsonc_parser::ParseOptions,
) -> Result<jsonc_parser::ast::Value<'a>, String> {
    jsonc_parser::parse_to_ast(text, &Default::default(), options)
        .map_err(|e| e.to_string())?
        .value
        .ok_or_else(|| "empty JSON".into())
}
/// npm's manifests: comments and trailing commas only for JSONC files,
/// everything else strict JSON.
pub(crate) fn jsonc_ast(text: &str, jsonc: bool) -> Result<jsonc_parser::ast::Value<'_>, String> {
    jsonc_parse(
        text,
        &jsonc_parser::ParseOptions {
            allow_comments: jsonc,
            allow_trailing_commas: jsonc,
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
            allow_bare_decimal_point_numbers: false,
            allow_non_finite_numbers: false,
            allow_extended_string_escapes: false,
        },
    )
}
/// NuGet's and devcontainers' settings: every JSONC relaxation on, which
/// is what `ParseOptions::default()` has always meant here.
pub(crate) fn jsonc_ast_lenient(text: &str) -> Result<jsonc_parser::ast::Value<'_>, String> {
    jsonc_parse(text, &Default::default())
}

impl Dependency {
    /// The package as the Count looks it up, registry included.
    pub fn id(&self) -> PackageId {
        PackageId::new(self.source, self.registry.clone(), &self.package)
    }
    /// The version a committed lockfile holds for a direct dependency whose
    /// lock the Count can move to a chosen version: Cargo.lock, uv.lock,
    /// poetry.lock, Gemfile.lock and composer.lock. A release inside the
    /// declared range reaches such a dependency through its lockfile alone.
    pub fn locked(&self) -> Option<&str> {
        matches!(
            (self.manager, self.source),
            (Manager::Cargo, Source::Crates)
                | (Manager::Pyproject, Source::Pypi)
                | (Manager::Bundler, Source::Rubygems)
                | (Manager::Composer, Source::Packagist)
        )
        .then_some(self.resolved.as_deref())
        .flatten()
    }
    /// The kinds rules select this dependency by: its own, plus
    /// `reference-data` for a package on the reference data list.
    pub fn kinds(&self) -> Vec<DependencyKind> {
        let mut kinds = vec![self.kind];
        if REFERENCE_DATA.contains(&(self.source, self.package.as_str())) {
            kinds.push(DependencyKind::ReferenceData);
        }
        kinds
    }
    /// The declaration this dependency is, for identity keys and comparisons.
    pub fn declaration(&self) -> Declaration<'_> {
        Declaration {
            file: &self.file,
            manager: self.manager,
            name: &self.name,
            package: &self.package,
            source: Some(self.source),
            current: Some(self.current.as_str()),
            digest: self.digest.as_deref(),
            registry: self.registry.as_deref(),
        }
    }
}
/// A dependency declaration's identity: the fields that say which declared
/// package a report entry or a change belongs to, borrowed from whichever
/// type holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declaration<'a> {
    pub file: &'a str,
    pub manager: Manager,
    pub name: &'a str,
    pub package: &'a str,
    /// The datasource the package is looked up in; `None` when a report
    /// entry carries no metadata.
    pub source: Option<Source>,
    /// The declared value; `None` when a report entry has none.
    pub current: Option<&'a str>,
    pub digest: Option<&'a str>,
    /// The registry the package is looked up in, when it is not the public one.
    pub registry: Option<&'a str>,
}
impl Declaration<'_> {
    /// The declaration's key: the JSON of its fields, hashed. A missing
    /// source or registry serializes as `""`/absent exactly as the report
    /// and extracted recipes always wrote it.
    pub fn key(&self) -> String {
        let mut parts = serde_json::json!([
            self.file,
            self.manager,
            self.name,
            self.package,
            self.source.map_or("", Source::as_str),
            self.current,
            self.digest,
        ]);
        if let Some(registry) = self.registry
            && let Some(parts) = parts.as_array_mut()
        {
            parts.push(registry.into());
        }
        hash(parts.to_string().as_bytes())
    }
    /// Whether `other` names the same extracted dependency. The datasource is
    /// left out: a report entry's metadata may not state one, and the report
    /// join has never compared it.
    pub fn equivalent(&self, other: &Self) -> bool {
        self.file == other.file
            && self.manager == other.manager
            && self.name == other.name
            && self.package == other.package
            && self.current == other.current
            && self.digest == other.digest
            && self.registry == other.registry
    }
}
/// The lowercase hex SHA-256 of `bytes`: what declaration keys, plan
/// fingerprints and receipt hashes are made with.
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Extraction {
    pub dependencies: Vec<Dependency>,
    pub warnings: Vec<String>,
    /// Lockfile packages no manifest declares, checked only for advisories.
    #[serde(default)]
    pub locked: Vec<Locked>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
    /// For each file that can steer a lockfile tool's fetches, whether it
    /// can fetch only from the ecosystem's well-known public registry:
    /// every configured and locked origin is public and nothing is
    /// uncertain or unreadable. A run may share the `public` cache
    /// partition only when every file it reads decides so; a missing
    /// verdict is not public.
    #[serde(default)]
    pub public_only: BTreeMap<String, bool>,
}
/// A declaration the Count found but cannot read as a version, such as an
/// image built from a variable. Unlike a warning, it leaves branch cleanup on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub manager: Manager,
    pub file: String,
    pub line: Option<usize>,
    pub value: String,
    pub reason: String,
}
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
}
/// The tag source of the Git repository `path` on `host`: GitHub tags, or
/// forge tags on GitLab and Gitea/Forgejo hosts. `None` for other hosts and
/// for paths that are not a repository.
pub(crate) fn tag_source(host: &str, path: &str) -> Option<(Source, String)> {
    let valid = path.split('/').count() >= 2
        && path.split('/').all(|p| {
            !p.is_empty()
                && !matches!(p, "." | "..")
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        });
    if !valid {
        return None;
    }
    if host == "github.com" {
        let mut parts = path.split('/');
        let repo = format!("{}/{}", parts.next()?, parts.next()?);
        return Some((Source::GithubTags, repo));
    }
    let forge = host == "gitlab.com" || host.split('.').any(|l| l == "gitlab") || gitea_host(host);
    forge.then(|| (Source::ForgeTags, format!("{host}/{path}")))
}
/// Gitea and Forgejo hosts the `forge-tags` source recognises by name.
pub(crate) fn gitea_host(host: &str) -> bool {
    let labels = || host.split('.');
    !labels().any(|l| l == "gitlab")
        && (matches!(host, "codeberg.org" | "gitea.com")
            || labels().any(|l| matches!(l, "gitea" | "forgejo")))
}
/// The files extraction reads, by path, each with its text or the reason it
/// has none.
pub type Files = BTreeMap<String, Text>;

/// A file as the Count holds it: its text, or why the Count has none. An
/// unread file is not an absent one: it may say anything, so whatever it
/// could steer must fail rather than fall back to a default.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Text {
    Read(String),
    Unread(Unread),
}
impl Text {
    /// The file's text, or why there is none. The only way to the text, so
    /// every reader decides what an unread file means for it.
    pub fn read(&self) -> Result<&str, Unread> {
        match self {
            Self::Read(text) => Ok(text),
            Self::Unread(unread) => Err(*unread),
        }
    }
}
/// Files that were all read, as extraction takes them.
pub fn all_read(files: &BTreeMap<String, String>) -> Files {
    files
        .iter()
        .map(|(path, text)| (path.clone(), Text::Read(text.clone())))
        .collect()
}
impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::Read(text)
    }
}
impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self::Read(text.to_owned())
    }
}
/// A predicate for the paths among the tree a later round must read.
pub(crate) type PathPredicate<'a> = Box<dyn Fn(&str) -> bool + 'a>;
/// Extract one selected readable file into the extraction.
pub(crate) type FileExtractor<'a> =
    Box<dyn FnMut(&str, &str, &mut Extraction) -> Result<(), String> + 'a>;
/// A finishing step over the whole extraction.
pub(crate) type Finisher<'a> = Box<dyn FnMut(&mut Extraction) -> Result<(), String> + 'a>;

/// What the Scanner needs to know about one ecosystem: which files it
/// reads, the state it parses once per extraction, and how it extracts and
/// finishes. One type per ecosystem implements it; managers that share an
/// implementation share a type with a manager parameter.
pub(crate) trait Ecosystem {
    /// The default file rule, used unless `files` config overrides it.
    fn selected(&self, config: &Config, path: &str) -> bool;
    /// Settings files read whatever `files`/`exclude` select.
    fn registry_file(&self, _path: &str) -> bool {
        false
    }
    /// The settings a package backend of this ecosystem reads, which mise's
    /// own rule reuses rather than naming files itself.
    fn backend_settings(&self, _path: &str) -> bool {
        false
    }
    /// A predicate for the paths among the tree a later round must read,
    /// from the files read so far.
    fn more<'a>(&self, _files: &'a Files) -> Option<PathPredicate<'a>> {
        None
    }
    /// Shared parsed state and the per-file extractor and finishing step,
    /// built once per extraction.
    fn prepare<'a>(
        &self,
        scanner: &'a Scanner,
        files: &'a Files,
        shared: &'a Shared<'a>,
    ) -> Result<Prepared<'a>, String> {
        let _ = (scanner, files, shared);
        Ok(Prepared::default())
    }
}

/// What one ecosystem's `prepare` returns: the per-file extractor and the
/// finishing step, each capturing whatever parsed state it needs.
#[derive(Default)]
pub(crate) struct Prepared<'a> {
    /// Extract one selected readable file into `out`.
    pub extract: Option<FileExtractor<'a>>,
    /// A finishing step over the whole extraction.
    pub finish: Option<Finisher<'a>>,
}

/// State several ecosystems share: the YAML family parses each selected
/// file once for all five managers, npm's registry settings serve the node
/// extractor, custom extractors and locked packages, and NuGet's feeds serve
/// NuGet and mise's dotnet redirects.
pub(crate) struct Shared<'a> {
    pub yaml: Option<yaml::Docs<'a>>,
    pub npmrcs: BTreeMap<&'a str, npm::Npmrc>,
    pub feeds: Option<nuget::Feeds>,
}

/// One file's extraction: its dependencies on success, or a warning that
/// skips it.
pub(crate) fn file_result(
    out: &mut Extraction,
    path: &str,
    result: Result<Vec<Dependency>, String>,
) {
    match result {
        Ok(dependencies) => out.dependencies.extend(dependencies),
        Err(e) => out.warnings.push(format!("{path}: skipped: {e}")),
    }
}

/// The order ecosystems extract a file in: the manifest chain first, then
/// the additive YAML, CI and regex passes, reproducing the old dispatch.
const EXTRACT_ORDER: &[Manager] = &[
    Manager::Npm,
    Manager::Bun,
    Manager::GithubActions,
    Manager::Mise,
    Manager::Cargo,
    Manager::Dockerfile,
    Manager::Go,
    Manager::Terraform,
    Manager::Composer,
    Manager::Nix,
    Manager::Bundler,
    Manager::Nuget,
    Manager::Gradle,
    Manager::Maven,
    Manager::Devcontainer,
    Manager::Apm,
    Manager::Woodpecker,
    Manager::Copier,
    Manager::Helm,
    Manager::PreCommit,
    Manager::Pyproject,
    Manager::Compose,
    Manager::Kubernetes,
    Manager::Kustomize,
    Manager::Flux,
    Manager::HelmValues,
    Manager::Gitlabci,
    Manager::GitlabciInclude,
    Manager::Regex,
];

/// The order finishing steps run: Maven's repositories, then Gradle's
/// variables and repositories, mise's redirects, Cargo's and the node
/// ecosystem's locked packages.
const FINISH_ORDER: &[Manager] = &[
    Manager::Maven,
    Manager::Gradle,
    Manager::Mise,
    Manager::Cargo,
    Manager::Npm,
    Manager::Bun,
];

static NODE_NPM: npm::Descriptor = npm::Descriptor {
    manager: Manager::Npm,
};
static NODE_BUN: npm::Descriptor = npm::Descriptor {
    manager: Manager::Bun,
};
static YAML_COMPOSE: yaml::Descriptor = yaml::Descriptor {
    manager: Manager::Compose,
};
static YAML_KUBERNETES: yaml::Descriptor = yaml::Descriptor {
    manager: Manager::Kubernetes,
};
static YAML_KUSTOMIZE: yaml::Descriptor = yaml::Descriptor {
    manager: Manager::Kustomize,
};
static YAML_FLUX: yaml::Descriptor = yaml::Descriptor {
    manager: Manager::Flux,
};
static YAML_HELM_VALUES: yaml::Descriptor = yaml::Descriptor {
    manager: Manager::HelmValues,
};
static GITLABCI: gitlabci::Descriptor = gitlabci::Descriptor {
    manager: Manager::Gitlabci,
};
static GITLABCI_INCLUDE: gitlabci::Descriptor = gitlabci::Descriptor {
    manager: Manager::GitlabciInclude,
};

/// The descriptor for `manager`; exhaustive, so a new `Manager` variant
/// fails to compile until it has one.
pub(crate) fn ecosystem(manager: Manager) -> &'static dyn Ecosystem {
    match manager {
        Manager::Npm => &NODE_NPM,
        Manager::Bun => &NODE_BUN,
        Manager::GithubActions => &actions::DESCRIPTOR,
        Manager::Mise => &mise::DESCRIPTOR,
        Manager::Cargo => &cargo::DESCRIPTOR,
        Manager::Dockerfile => &images::DESCRIPTOR,
        Manager::Compose => &YAML_COMPOSE,
        Manager::Go => &go::DESCRIPTOR,
        Manager::Terraform => &terraform::DESCRIPTOR,
        Manager::Kubernetes => &YAML_KUBERNETES,
        Manager::Kustomize => &YAML_KUSTOMIZE,
        Manager::Flux => &YAML_FLUX,
        Manager::HelmValues => &YAML_HELM_VALUES,
        Manager::Pyproject => &pyproject::DESCRIPTOR,
        Manager::PreCommit => &precommit::DESCRIPTOR,
        Manager::Helm => &helm::DESCRIPTOR,
        Manager::Devcontainer => &devcontainer::DESCRIPTOR,
        Manager::Woodpecker => &woodpecker::DESCRIPTOR,
        Manager::Copier => &copier::DESCRIPTOR,
        Manager::Maven => &maven::DESCRIPTOR,
        Manager::Gradle => &gradle::DESCRIPTOR,
        Manager::Nuget => &nuget::DESCRIPTOR,
        Manager::Bundler => &bundler::DESCRIPTOR,
        Manager::Composer => &composer::DESCRIPTOR,
        Manager::Gitlabci => &GITLABCI,
        Manager::GitlabciInclude => &GITLABCI_INCLUDE,
        Manager::Nix => &nix::DESCRIPTOR,
        Manager::Apm => &apm::DESCRIPTOR,
        Manager::Regex => &custom::DESCRIPTOR,
    }
}

pub struct Scanner {
    pub(crate) config: Config,
    exclude: Patterns,
    selections: Vec<(Manager, Patterns)>,
}
impl Scanner {
    pub fn new(config: &Config) -> Result<Self, String> {
        let selections = config
            .files
            .iter()
            .map(|f| Ok((f.manager, Patterns::compile(&f.paths)?)))
            .collect::<Result<_, String>>()?;
        Ok(Self {
            config: config.clone(),
            exclude: Patterns::compile(&config.exclude)?,
            selections,
        })
    }
    pub fn needs(&self, path: &str) -> bool {
        self.config.managers.iter().any(|m| self.selected(*m, path)) || self.registry_file(path)
    }
    /// Whether `path` sits under the repository's excluded paths.
    pub fn excluded(&self, path: &str) -> bool {
        !self.config.exclude.is_empty() && self.exclude.matches(path)
    }
    /// Whether `path` is one the committed-credentials scan checks: a
    /// settings file that can hold credentials, which this repository's
    /// scan reads. Paths `exclude` leaves out are never checked.
    pub fn credential_checked(&self, path: &str) -> bool {
        credential_scope(path) && !self.excluded(path) && self.needs(path)
    }
    /// The files among `paths` that what `files` says makes the Count read
    /// as well: the sources of the builds Gradle settings include, which can
    /// add repositories, and the other ecosystems' later-round files.
    pub fn more<'a>(&self, files: &Files, paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let wanted: Vec<PathPredicate<'_>> = self
            .config
            .managers
            .iter()
            .filter_map(|m| ecosystem(*m).more(files))
            .collect();
        paths
            .into_iter()
            .filter(|path| !files.contains_key(*path) && wanted.iter().any(|w| w(path)))
            .map(str::to_owned)
            .collect()
    }
    /// Whether npm dependencies, or an npm extractor, are maintained.
    fn npm_enabled(&self) -> bool {
        [Manager::Npm, Manager::Bun]
            .iter()
            .any(|m| self.config.managers.contains(m))
            || (self.config.managers.contains(&Manager::Regex)
                && self
                    .config
                    .extractors
                    .iter()
                    .any(|e| e.source == "npm" || e.source.contains('{')))
    }
    /// Registry settings are read whatever `files` and `exclude` select:
    /// without them, a scoped package would be looked up on the public
    /// registry, where its name may be someone else's. The node rule also
    /// applies to an npm extractor with npm disabled, which reads `.npmrc`
    /// for its registries.
    fn registry_file(&self, path: &str) -> bool {
        (self.npm_enabled() && ecosystem(Manager::Npm).registry_file(path))
            || self
                .config
                .managers
                .iter()
                .any(|m| ecosystem(*m).registry_file(path))
    }
    /// The pip requirements files among `paths`, which the Count does not
    /// maintain, as skipped entries saying why, when Python projects are
    /// enabled. Skipped, not warned about, so they leave branch cleanup and
    /// automerge on.
    pub fn unsupported<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Vec<Skipped> {
        if !self.config.managers.contains(&Manager::Pyproject) {
            return Vec::new();
        }
        paths
            .into_iter()
            .filter(|path| {
                requirements_name(path)
                    && (self.config.exclude.is_empty() || !self.exclude.matches(path))
            })
            .map(|path| Skipped {
                manager: Manager::Pyproject,
                file: path.to_owned(),
                line: None,
                value: "pip requirements".into(),
                reason: "pip requirements files are not maintained: pip takes each package from whatever index its options, included files and environment name, which the Count cannot settle for certain, and a guess could propose someone else's package of the same name. Move the project to uv (`uv init --bare`, then `uv add -r requirements.txt`) and the Count keeps `pyproject.toml` and `uv.lock` current".into(),
            })
            .collect()
    }
    pub(crate) fn selected(&self, manager: Manager, path: &str) -> bool {
        if !self.config.managers.contains(&manager) || self.excluded(path) {
            return false;
        }
        if let Some((_, p)) = self.selections.iter().find(|(m, _)| *m == manager) {
            return p.matches(path);
        }
        ecosystem(manager).selected(&self.config, path)
    }
    pub fn extract(&self, files: &Files) -> Result<Extraction, String> {
        let mut result = Extraction::default();
        let npmrcs = if self.npm_enabled() {
            result.warnings.extend(npm::env_registry_warnings(files));
            npm::npmrcs(files)
        } else {
            BTreeMap::new()
        };
        // A file that does not parse (a template such as ERB-rendered YAML)
        // is skipped with a warning. Warnings suppress branch cleanup, so the
        // skipped file's open updates are never closed as obsolete.
        let yaml = if [
            Manager::Compose,
            Manager::Kubernetes,
            Manager::Kustomize,
            Manager::Flux,
            Manager::HelmValues,
        ]
        .iter()
        .any(|m| self.config.managers.contains(m))
        {
            Some(yaml::prepare(self, files, &mut result.warnings))
        } else {
            None
        };
        let feeds = (self.config.managers.contains(&Manager::Nuget)
            || self.config.managers.contains(&Manager::Mise))
        .then(|| nuget::Feeds::new(files));
        let shared = Shared {
            yaml,
            npmrcs,
            feeds,
        };
        let mut prepared = Vec::new();
        for &manager in EXTRACT_ORDER {
            if self.config.managers.contains(&manager) {
                prepared.push((manager, ecosystem(manager).prepare(self, files, &shared)?));
            }
        }
        for (path, text) in files {
            // An unread file says nothing a manager can extract; whatever it
            // could steer is failed by the readers that consult it.
            let text = match text.read() {
                Ok(text) => text,
                Err(unread) => {
                    if self.config.managers.iter().any(|m| self.selected(*m, path)) {
                        result
                            .warnings
                            .push(format!("{path}: skipped: {}", unread.why()));
                    }
                    continue;
                }
            };
            for (manager, eco) in &mut prepared {
                let Some(extract) = eco.extract.as_mut() else {
                    continue;
                };
                if self.selected(*manager, path) {
                    extract(path, text, &mut result)?;
                }
            }
        }
        // The ordered finishing steps first, then any other ecosystem's in
        // extraction order, so a finisher is never skipped for want of a
        // place in `FINISH_ORDER`.
        prepared.sort_by_key(|(manager, _)| {
            FINISH_ORDER
                .iter()
                .position(|m| m == manager)
                .unwrap_or(FINISH_ORDER.len())
        });
        for (_, eco) in &mut prepared {
            if let Some(finish) = eco.finish.as_mut() {
                finish(&mut result)?;
            }
        }
        for (path, text) in files {
            if let Some(flag) = public::verdict(path, text, files) {
                result.public_only.insert(path.clone(), flag);
            }
        }
        Ok(result)
    }
}
/// A registry value as recorded when the Count cannot use it: any user,
/// password or query removed, so a secret in it never reaches a report.
pub(crate) fn redacted(value: &str) -> String {
    const UNREADABLE: &str = "<unreadable URL>";
    if !value.contains("://") {
        // A path or name; one with an `@` may be a URL with a user in it, and
        // a query or fragment may hold a token.
        return match value.split_once(['?', '#']) {
            _ if value.contains('@') => UNREADABLE.into(),
            Some((kept, _)) => format!("{kept} (credentials removed)"),
            None => value.to_owned(),
        };
    }
    let Ok(mut url) = url::Url::parse(value) else {
        return UNREADABLE.into();
    };
    let user = !url.username().is_empty() || url.password().is_some();
    // An `@` that did not end a user means a password holding `/`, `?` or `#`
    // ended the host early, leaving part of itself where the host should be.
    if value.contains('@') && !user {
        return UNREADABLE.into();
    }
    // A user ends at the last `@`; a parser that ended it sooner took part
    // of the password for the host.
    if let Some((_, after)) = value.rsplit_once('@')
        && !after
            .to_ascii_lowercase()
            .starts_with(url.host_str().unwrap_or("\0"))
    {
        return UNREADABLE.into();
    }
    let secret = user || url.query().is_some() || url.fragment().is_some();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let kept = url.as_str().trim_end_matches('/');
    // Or it ended the host early and left the rest of itself in the path.
    if kept.contains('@') {
        return UNREADABLE.into();
    }
    if secret {
        format!("{kept} (credentials removed)")
    } else {
        kept.to_owned()
    }
}
/// A registry URL a repository names: https, without a user, query or
/// fragment, and nothing to expand from the environment. Normalised without a
/// trailing slash or surrounding quotes.
pub(crate) fn registry_url(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '\'']).trim_end_matches('/');
    // A URL's scheme is case-insensitive.
    let rest = value
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &value[8..])?;
    let host = rest.split('/').next().unwrap_or_default();
    let valid = !host.is_empty() && !value.contains(['@', '?', '#', ' ', '$']);
    valid.then(|| format!("https://{rest}"))
}

/// A pip requirements file by its name: `requirements.txt`,
/// `dev-requirements.in`, `requirements-test.txt`, or any `.txt` or `.in`
/// under a `requirements/` directory.
fn requirements_name(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = leaf
        .strip_suffix(".txt")
        .or_else(|| leaf.strip_suffix(".in"))
    else {
        return false;
    };
    stem.contains("requirements")
        || path
            .rsplit_once('/')
            .is_some_and(|(dir, _)| dir.split('/').any(|part| part == "requirements"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn extraction_names_every_manager_once() {
        let mut order = super::EXTRACT_ORDER.to_vec();
        order.sort();
        order.dedup();
        assert_eq!(
            order.len(),
            super::EXTRACT_ORDER.len(),
            "a manager is listed twice"
        );
        let mut all = super::Manager::ALL.to_vec();
        all.sort();
        assert_eq!(order, all, "every manager has a place in EXTRACT_ORDER");
    }

    /// No user, password or token survives redaction, whatever characters
    /// it holds. The secrets use letters no recorded value contains, so any
    /// leaked part of one shows.
    #[test]
    fn redaction_never_keeps_a_secret() {
        // Letters, digits and a non-ASCII letter no recorded value holds,
        // among separators, escapes and whitespace.
        const SECRET: &[char] = &[
            'Q', 'X', 'Z', 'J', 'K', 'W', 'G', '5', '1', '\u{e9}', '/', '?', '#', '@', ':', '%',
            '$', '&', '=', '+', '!', ';', ',', '[', ']', '\\', ' ', '\t', '\n',
        ];
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % bound as u64).unwrap_or_default()
        };
        for _ in 0..20_000 {
            let secret: String = (0..=next(12)).map(|_| SECRET[next(SECRET.len())]).collect();
            for value in [
                format!("https://ci:{secret}@pypi.acme.dev/simple"), // betterleaks:allow
                format!("https://{secret}@pypi.acme.dev/simple"),    // betterleaks:allow
                format!("https://{secret}:0@pypi.acme.dev/simple"),  // betterleaks:allow
                format!("https://pypi.acme.dev/simple?token={secret}"),
                format!("https://pypi.acme.dev?{secret}"),
                format!("https://pypi.acme.dev/simple#{secret}"),
                format!("ci:{secret}@pypi.acme.dev/simple"),
                format!("pypi.acme.dev/simple?token={secret}"),
                format!("http://ci:{secret}@pypi.acme.dev/simple"), // betterleaks:allow
                format!("HTTPS://ci:{secret}@pypi.acme.dev/simple"), // betterleaks:allow
                format!("https://ci:{secret}\\0@pypi.acme.dev/simple"), // betterleaks:allow
                format!("https://ci:{secret} 0@pypi.acme.dev/simple"), // betterleaks:allow
            ] {
                // As redacted, and as the Python reader records an index.
                let parsed = crate::pypi::named(&value).recorded();
                for recorded in [Some(super::redacted(&value)), parsed]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        !recorded.to_ascii_uppercase().contains([
                            'Q', 'X', 'Z', 'J', 'K', 'W', 'G', '5', '1', '\u{e9}', '\u{c9}'
                        ]),
                        "{value:?} -> {recorded:?}"
                    );
                }
            }
        }
    }
}

/// The keys the Count's Python readers know, for the tests that check them
/// against the real uv and Poetry.
pub mod known {
    pub use crate::pypi::settings::{
        POETRY_DEPENDENCY_KEYS, POETRY_SOURCE_KEYS, UV_IRRELEVANT, UV_PIP_IRRELEVANT, UV_PIP_READ,
        UV_READ, UV_SOURCE_KEYS,
    };
}

/// A file that can say where packages come from: Python settings,
/// Bundler's settings, a NuGet config, an MSBuild file that can set
/// restore sources, or Maven or Gradle build files that can add
/// repositories. One the Count cannot read, such as a symlink or a file
/// that is not text, must reach the scanner as unreadable, since leaving it
/// out would make its packages the public registry's.
pub fn registry_settings(path: &str) -> bool {
    python_settings(path)
        || bundle_config(path)
        || nuget::config_file(path)
        || nuget::msbuild_file(path)
        || maven::repository_file(path)
        || gradle::build_logic(path)
        || cargo::config_file(path)
        || path.rsplit('/').next() == Some("Cargo.toml")
        || go::work_file(path)
        || mise::config_file(path)
        || matches!(
            path.rsplit('/').next().unwrap_or(path),
            ".npmrc" | ".yarnrc.yml" | "pip.conf" | "pip.ini"
        )
}

/// A file that can name a Python package index.
pub(crate) fn python_settings(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    matches!(
        leaf,
        "pyproject.toml" | "uv.toml" | "pdm.toml" | "pixi.toml" | "hatch.toml"
    )
}

/// Bundler's settings for a project.
pub(crate) fn bundle_config(path: &str) -> bool {
    path == ".bundle/config" || path.ends_with("/.bundle/config")
}

/// The settings file a symlink at `path` could stand for: the path itself
/// when it is a settings file, or one inside a linked folder the tools
/// follow: `.bundle` for Bundler, `.mvn` for Maven, and Gradle's build-logic
/// folders, whose contents the Count then cannot read.
pub fn linked_settings(path: &str) -> Option<String> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if registry_settings(path) {
        return Some(path.to_owned());
    }
    match leaf {
        ".bundle" => Some(format!("{path}/config")),
        ".mvn" => Some(format!("{path}/maven.config")),
        ".cargo" => Some(format!("{path}/config.toml")),
        _ if gradle::logic_folder(path) => Some(format!("{path}/build.gradle.kts")),
        _ => None,
    }
}

/// The text of a file that was read, as one a document was parsed from must
/// be; empty for any other.
pub(crate) fn read_text<'a>(files: &'a Files, path: &str) -> &'a str {
    files
        .get(path)
        .and_then(|text| text.read().ok())
        .unwrap_or("")
}

/// Why a file in [`Files`] holds no text the Count read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Unread {
    /// The read failed.
    Failed,
    /// The include chain reached it after the read rounds ran out.
    TooDeep,
}
impl Unread {
    /// Why, in a few words, for a warning that skips the file.
    pub fn why(self) -> &'static str {
        match self {
            Self::Failed => "the Count could not read it",
            Self::TooDeep => "the include chain was too deep to reach it",
        }
    }
    /// Why lookups that depend on the file at `path` fail.
    pub(crate) fn reason(self, path: &str) -> String {
        match self {
            Self::Failed => format!("{path}, which the Count cannot read"),
            Self::TooDeep => format!(
                "include chain deeper than {MORE_ROUNDS} rounds starting at {path}; the remaining files were not read"
            ),
        }
    }
}

/// How many times the files read so far may name more to read: Gradle
/// builds include builds and apply scripts, and Maven POMs name modules, a
/// few levels deep at most.
pub const MORE_ROUNDS: usize = 16;

/// The host of an http or https `url`, lowercased and without a default
/// port, whatever the case of its scheme.
pub(crate) fn web_host(url: &str) -> Option<String> {
    let rest = ["https://", "http://"].iter().find_map(|scheme| {
        url.get(..scheme.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(scheme))
            .map(|_| &url[scheme.len()..])
    })?;
    let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    Some(
        host.strip_suffix(":443")
            .or_else(|| host.strip_suffix(":80"))
            .unwrap_or(&host)
            .to_owned(),
    )
}
