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
pub use cargo::crates_io;
pub use credentials::{CredentialCheck, committed_credentials, credential_reason};
pub use roles::{credential_check, credential_scope, linked_settings, registry_settings};
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
mod proposal;
pub use proposal::{
    Advisories, Edits, Lock, Pin, Proposing, Range, ShortTags, Unversioned, Written,
};
mod pyproject;
pub mod roles;
mod terraform;
mod toolchains;
mod woodpecker;
mod yaml;
mod yaml_locations;

use patchula_config::{Config, PackageId, Patterns, Registry};
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
    pub(crate) fn new(text: &Lines<'_>, start: usize, end: usize, pointer: String) -> Self {
        let (line, column) = text.line_column(start);
        Self {
            start,
            end,
            line,
            column,
            pointer,
        }
    }
}
/// A file's text with the byte offset of each line's start, built once per
/// file so a `Location` does not rescan the text for every dependency.
/// Derefs to the text itself.
pub(crate) struct Lines<'a> {
    text: &'a str,
    /// Byte offset of each line's first byte: 0, then one past each '\n'.
    starts: Vec<usize>,
}
impl<'a> Lines<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self {
            text,
            starts: std::iter::once(0)
                .chain(text.match_indices('\n').map(|(n, _)| n + 1))
                .collect(),
        }
    }
    /// The byte offset the 1-indexed `line` starts at.
    pub(crate) fn line_start(&self, line: usize) -> Option<usize> {
        self.starts.get(line.checked_sub(1)?).copied()
    }
    /// The 1-indexed line and column of byte offset `at`.
    pub(crate) fn line_column(&self, at: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|start| *start <= at);
        (line, at - self.starts[line - 1] + 1)
    }
}
impl std::ops::Deref for Lines<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        self.text
    }
}
impl AsRef<str> for Lines<'_> {
    fn as_ref(&self) -> &str {
        self.text
    }
}
impl From<Location> for patchula_report::Location {
    fn from(location: Location) -> Self {
        Self {
            start: location.start,
            end: location.end,
            line: location.line,
            column: location.column,
            pointer: location.pointer,
        }
    }
}
/// How the value at `Dependency::location` is spelled, which the parser that
/// extracted it knows: `edits` decodes and re-encodes it with this rather
/// than inferring a syntax from the manager, file or pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditForm {
    /// A scalar in `syntax`.
    Scalar(Syntax),
    /// A container image reference. `digest_pointer` is the pointer of the
    /// sibling field holding the image digest when the tag is edited on its
    /// own; `comment` keeps a trailing ` #` comment inside the located
    /// range.
    Image {
        syntax: Syntax,
        digest_pointer: Option<String>,
        comment: bool,
    },
}
impl EditForm {
    /// The syntax the located value is spelled in.
    pub fn syntax(&self) -> Syntax {
        match self {
            Self::Scalar(syntax) => *syntax,
            Self::Image { syntax, .. } => *syntax,
        }
    }
}
/// The syntax a located value is spelled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Syntax {
    /// Raw text; a replacement holding whitespace is refused.
    Bare,
    /// Raw text a custom extractor captured; a replacement may hold
    /// whitespace.
    Text,
    /// A span inside a quoted string; a replacement holding a quote,
    /// backslash or whitespace is refused.
    InnerToken,
    /// A span inside a quoted string where spaces are allowed; a
    /// replacement holding a quote or backslash is refused.
    Inner,
    /// A Gradle version: `[A-Za-z0-9._-+]` only.
    Gradle,
    /// A JSON string literal, quotes included in the location.
    Json,
    /// A YAML scalar, quotes included when the file quotes it.
    Yaml,
    /// A YAML scalar holding a tag name or ref: a plain number stays text
    /// rather than decoding as a float.
    YamlName,
    /// A TOML string literal, quotes included in the location.
    Toml,
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
    /// How the value at `location` is spelled, set by the parser that
    /// extracted it.
    pub form: EditForm,
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
    /// The registry to look the package up in, when it is not the source's
    /// public one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<Registry>,
    /// The committed Cargo publish-age cooldown that applies to the package:
    /// a floor on its release-age wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<Cooldown>,
}
/// A repository's committed Cargo publish-age cooldown: the strictest value
/// the Cargo configurations applying to a manifest or lockfile set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cooldown {
    /// The cooldown in whole hours, rounded up from the committed value.
    pub hours: u32,
    /// The committed value, verbatim: what the lockfile run's Cargo gets.
    pub value: String,
    /// The configuration file that commits it.
    pub file: String,
}
/// A Cargo directory's committed cooldown, as the lockfile contract carries
/// it: the value the run's Cargo gets, or the reason its jobs must fail
/// rather than run without the repository's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CargoCooldown {
    /// A committed cooldown.
    Set(Cooldown),
    /// A committed cooldown the Count cannot read.
    Unread(String),
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
            registry: self.registry.as_ref().map(Registry::recorded),
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
    /// Each Cargo manifest or lockfile directory's committed publish-age
    /// cooldown, kept for the lockfile run's Cargo environment.
    #[serde(default)]
    pub cargo_cooldowns: BTreeMap<String, CargoCooldown>,
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
    Box<dyn FnMut(&str, &Lines<'_>, &mut Extraction) -> Result<(), String> + 'a>;
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
    /// What lookup, the engine and update must know to propose for `dep`:
    /// the proposal facts, required so a new manager states every one.
    fn proposing(&self, dep: &Dependency) -> Proposing;
    /// The source a package only a lockfile records is looked up in, when
    /// this ecosystem's lockfile readers emit `Locked` entries.
    fn locked_source(&self) -> Option<Source> {
        None
    }
    /// Why `dep` refuses edits outright, when it does.
    fn edit_refusal(&self, _dep: &Dependency) -> Option<String> {
        None
    }
    /// Whether `dep` is inventory the Count records but never edits.
    fn inventory_only(&self, _dep: &Dependency, _raw: &str) -> bool {
        false
    }
    /// The replacement `value` takes in `dep`'s declared form, when the
    /// ecosystem wraps versions, or `None` for the plain scalar.
    fn declared_form(
        &self,
        _dep: &Dependency,
        _current: &str,
        _value: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
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
        Manager::Compose => yaml::descriptor(manager),
        Manager::Go => &go::DESCRIPTOR,
        Manager::Terraform => &terraform::DESCRIPTOR,
        Manager::Kubernetes | Manager::Kustomize | Manager::Flux | Manager::HelmValues => {
            yaml::descriptor(manager)
        }
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
        let yaml = if yaml::MANAGERS
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
            let text = Lines::new(text);
            for (manager, eco) in &mut prepared {
                let Some(extract) = eco.extract.as_mut() else {
                    continue;
                };
                if self.selected(*manager, path) {
                    extract(path, &text, &mut result)?;
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
    patchula_config::acceptable_registry_url(value).then(|| format!("https://{}", &value[8..]))
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
    /// The schema's built-in `versions = "preserve"` rule names the managers
    /// whose descriptors declare `lock.keeps_ranges`; the two must agree.
    #[test]
    fn pkl_preserve_rule_matches_descriptors() {
        use super::Manager;
        let schema = patchula_config::SCHEMA;
        let apply = schema
            .find("versions = \"preserve\"")
            .expect("the built-in preserve rule");
        let rule_start = schema[..apply].rfind("new Rule").expect("a rule");
        let rule = &schema[rule_start..apply];
        let listing = rule
            .split("managers = new Listing {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("the rule's manager listing");
        let declared: std::collections::BTreeSet<Manager> = listing
            .split('"')
            .skip(1)
            .step_by(2)
            .map(|word| word.parse().unwrap())
            .collect();
        let descriptors: std::collections::BTreeSet<Manager> = Manager::ALL
            .iter()
            .copied()
            .filter(|manager| {
                let dep = super::Dependency {
                    manager: *manager,
                    file: "manifest".into(),
                    location: super::Location {
                        start: 0,
                        end: 1,
                        line: 1,
                        column: 1,
                        pointer: String::new(),
                    },
                    form: super::EditForm::Scalar(super::Syntax::Bare),
                    name: "example".into(),
                    package: "example".into(),
                    kind: super::DependencyKind::Production,
                    current: "1.0.0".into(),
                    resolved: None,
                    digest: None,
                    source: super::Source::Npm,
                    scheme: super::Scheme::Semver,
                    extract: None,
                    included_by: None,
                    registry: None,
                    cooldown: None,
                };
                super::ecosystem(*manager).proposing(&dep).lock.keeps_ranges
            })
            .collect();
        assert_eq!(declared, descriptors);
    }

    /// Today's per-path classification, pinned before the file-role taxonomy:
    /// `needs`, `registry_settings`, `linked_settings`, `credential_scope`,
    /// `credential_check`, public-verdict coverage and each manager's
    /// `registry_file`, for root and nested forms of representative paths.
    /// The taxonomy must reproduce every answer except the spec's permitted
    /// fixes.
    #[test]
    fn file_classification_is_pinned() {
        use super::CredentialCheck;
        type Row = (
            &'static str,
            bool,
            bool,
            Option<&'static str>,
            bool,
            CredentialCheck,
            bool,
            &'static str,
        );
        /// (path, needs, registry_settings, linked_settings, credential_scope,
        /// credential_check, public verdict, registry_file managers)
        #[rustfmt::skip]
        const TABLE: &[Row] = &[
            ("package.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/package.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("package-lock.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/package-lock.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("npm-shrinkwrap.json", false, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/npm-shrinkwrap.json", false, false, None, false, CredentialCheck::Scan, true, ""),
            ("aube-lock.yaml", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/aube-lock.yaml", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("pnpm-lock.yaml", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/pnpm-lock.yaml", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("yarn.lock", false, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/yarn.lock", false, false, None, false, CredentialCheck::Scan, true, ""),
            ("bun.lock", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/bun.lock", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("bun.lockb", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/bun.lockb", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("pnpm-workspace.yaml", true, false, None, true, CredentialCheck::Scan, true, ""),
            ("sub/dir/pnpm-workspace.yaml", true, false, None, true, CredentialCheck::Scan, true, ""),
            (".npmrc", true, true, Some(".npmrc"), true, CredentialCheck::Scan, true, "npm,bun,mise"),
            ("sub/dir/.npmrc", true, true, Some("sub/dir/.npmrc"), true, CredentialCheck::Scan, true, "npm,bun,mise"),
            (".yarnrc.yml", true, true, Some(".yarnrc.yml"), true, CredentialCheck::Scan, true, "npm,bun,mise"),
            ("sub/dir/.yarnrc.yml", true, true, Some("sub/dir/.yarnrc.yml"), true, CredentialCheck::Scan, true, "npm,bun,mise"),
            (".nvmrc", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.nvmrc", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".node-version", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.node-version", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("pyproject.toml", true, true, Some("pyproject.toml"), true, CredentialCheck::Scan, true, "mise,pyproject"),
            ("sub/dir/pyproject.toml", true, true, Some("sub/dir/pyproject.toml"), true, CredentialCheck::Scan, true, "mise,pyproject"),
            ("uv.toml", true, true, Some("uv.toml"), true, CredentialCheck::Scan, true, "mise,pyproject"),
            ("sub/dir/uv.toml", true, true, Some("sub/dir/uv.toml"), true, CredentialCheck::Scan, true, "mise,pyproject"),
            ("poetry.toml", true, true, Some("poetry.toml"), true, CredentialCheck::Scan, true, "pyproject"),
            ("sub/dir/poetry.toml", true, true, Some("sub/dir/poetry.toml"), true, CredentialCheck::Scan, true, "pyproject"),
            ("pdm.toml", true, true, Some("pdm.toml"), false, CredentialCheck::Undecided, true, "pyproject"),
            ("sub/dir/pdm.toml", true, true, Some("sub/dir/pdm.toml"), false, CredentialCheck::Undecided, true, "pyproject"),
            ("pixi.toml", true, true, Some("pixi.toml"), false, CredentialCheck::Undecided, true, "pyproject"),
            ("sub/dir/pixi.toml", true, true, Some("sub/dir/pixi.toml"), false, CredentialCheck::Undecided, true, "pyproject"),
            ("hatch.toml", true, true, Some("hatch.toml"), false, CredentialCheck::Undecided, true, "pyproject"),
            ("sub/dir/hatch.toml", true, true, Some("sub/dir/hatch.toml"), false, CredentialCheck::Undecided, true, "pyproject"),
            ("uv.lock", true, false, None, false, CredentialCheck::Scan, true, "pyproject"),
            ("sub/dir/uv.lock", true, false, None, false, CredentialCheck::Scan, true, "pyproject"),
            ("poetry.lock", true, false, None, false, CredentialCheck::Scan, true, "pyproject"),
            ("sub/dir/poetry.lock", true, false, None, false, CredentialCheck::Scan, true, "pyproject"),
            (".python-version", true, false, None, false, CredentialCheck::Skip, false, ""),
            ("sub/dir/.python-version", true, false, None, false, CredentialCheck::Skip, false, ""),
            ("pip.conf", true, true, Some("pip.conf"), true, CredentialCheck::Undecided, false, "mise"),
            ("sub/dir/pip.conf", true, true, Some("sub/dir/pip.conf"), true, CredentialCheck::Undecided, false, "mise"),
            ("pip.ini", true, true, Some("pip.ini"), true, CredentialCheck::Undecided, false, "mise"),
            ("sub/dir/pip.ini", true, true, Some("sub/dir/pip.ini"), true, CredentialCheck::Undecided, false, "mise"),
            ("requirements.txt", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/requirements.txt", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("requirements.in", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/requirements.in", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("Pipfile", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/Pipfile", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("Pipfile.lock", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/Pipfile.lock", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("pdm.lock", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/pdm.lock", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("pylock.toml", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/pylock.toml", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("Gemfile", true, false, None, true, CredentialCheck::Scan, true, ""),
            ("sub/dir/Gemfile", true, false, None, true, CredentialCheck::Scan, true, ""),
            ("Gemfile.lock", true, false, None, false, CredentialCheck::Scan, true, "bundler"),
            ("sub/dir/Gemfile.lock", true, false, None, false, CredentialCheck::Scan, true, "bundler"),
            (".ruby-version", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.ruby-version", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("example.gemspec", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/example.gemspec", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("composer.json", true, false, None, true, CredentialCheck::Scan, true, ""),
            ("sub/dir/composer.json", true, false, None, true, CredentialCheck::Scan, true, ""),
            ("composer.lock", true, false, None, false, CredentialCheck::Scan, true, "composer"),
            ("sub/dir/composer.lock", true, false, None, false, CredentialCheck::Scan, true, "composer"),
            ("Cargo.toml", true, true, Some("Cargo.toml"), false, CredentialCheck::Scan, true, ""),
            ("sub/dir/Cargo.toml", true, true, Some("sub/dir/Cargo.toml"), false, CredentialCheck::Scan, true, ""),
            ("Cargo.lock", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/Cargo.lock", true, false, None, false, CredentialCheck::Scan, true, ""),
            (".cargo/config.toml", true, true, Some(".cargo/config.toml"), true, CredentialCheck::Scan, false, "mise,cargo"),
            ("sub/dir/.cargo/config.toml", true, true, Some("sub/dir/.cargo/config.toml"), true, CredentialCheck::Scan, false, "mise,cargo"),
            (".cargo/config", true, true, Some(".cargo/config"), true, CredentialCheck::Scan, false, "mise,cargo"),
            ("sub/dir/.cargo/config", true, true, Some("sub/dir/.cargo/config"), true, CredentialCheck::Scan, false, "mise,cargo"),
            ("rust-toolchain", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/rust-toolchain", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("rust-toolchain.toml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/rust-toolchain.toml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("lib.rs", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/lib.rs", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("go.mod", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/go.mod", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("go.work", true, true, Some("go.work"), false, CredentialCheck::Scan, false, "go"),
            ("sub/dir/go.work", true, true, Some("sub/dir/go.work"), false, CredentialCheck::Scan, false, "go"),
            ("go.sum", false, false, None, false, CredentialCheck::Skip, false, ""),
            ("sub/dir/go.sum", false, false, None, false, CredentialCheck::Skip, false, ""),
            ("go.work.sum", false, false, None, false, CredentialCheck::Skip, false, ""),
            ("sub/dir/go.work.sum", false, false, None, false, CredentialCheck::Skip, false, ""),
            ("main.go", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/main.go", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("nuget.config", true, true, Some("nuget.config"), true, CredentialCheck::Scan, true, "mise,nuget"),
            ("sub/dir/nuget.config", true, true, Some("sub/dir/nuget.config"), true, CredentialCheck::Scan, true, "mise,nuget"),
            ("NuGet.Config", true, true, Some("NuGet.Config"), true, CredentialCheck::Scan, true, "mise,nuget"),
            ("sub/dir/NuGet.Config", true, true, Some("sub/dir/NuGet.Config"), true, CredentialCheck::Scan, true, "mise,nuget"),
            ("NUGET.CONFIG", true, true, Some("NUGET.CONFIG"), true, CredentialCheck::Scan, true, "mise,nuget"),
            ("sub/dir/NUGET.CONFIG", true, true, Some("sub/dir/NUGET.CONFIG"), true, CredentialCheck::Scan, true, "mise,nuget"),
            ("packages.lock.json", true, false, None, false, CredentialCheck::Skip, true, ""),
            ("sub/dir/packages.lock.json", true, false, None, false, CredentialCheck::Skip, true, ""),
            ("global.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/global.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("dotnet-tools.json", false, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/dotnet-tools.json", false, false, None, false, CredentialCheck::Scan, true, ""),
            (".config/dotnet-tools.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/.config/dotnet-tools.json", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("Directory.Packages.props", true, true, Some("Directory.Packages.props"), false, CredentialCheck::Scan, true, "nuget"),
            ("sub/dir/Directory.Packages.props", true, true, Some("sub/dir/Directory.Packages.props"), false, CredentialCheck::Scan, true, "nuget"),
            ("Directory.Build.props", true, true, Some("Directory.Build.props"), false, CredentialCheck::Scan, true, "nuget"),
            ("sub/dir/Directory.Build.props", true, true, Some("sub/dir/Directory.Build.props"), false, CredentialCheck::Scan, true, "nuget"),
            ("Directory.Build.targets", true, true, Some("Directory.Build.targets"), false, CredentialCheck::Scan, true, "nuget"),
            ("sub/dir/Directory.Build.targets", true, true, Some("sub/dir/Directory.Build.targets"), false, CredentialCheck::Scan, true, "nuget"),
            ("app.csproj", true, true, Some("app.csproj"), false, CredentialCheck::Scan, true, "nuget"),
            ("sub/dir/app.csproj", true, true, Some("sub/dir/app.csproj"), false, CredentialCheck::Scan, true, "nuget"),
            ("app.fsproj", true, true, Some("app.fsproj"), false, CredentialCheck::Scan, true, "nuget"),
            ("sub/dir/app.fsproj", true, true, Some("sub/dir/app.fsproj"), false, CredentialCheck::Scan, true, "nuget"),
            ("app.vbproj", true, true, Some("app.vbproj"), false, CredentialCheck::Scan, true, "nuget"),
            ("sub/dir/app.vbproj", true, true, Some("sub/dir/app.vbproj"), false, CredentialCheck::Scan, true, "nuget"),
            ("app.sln", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/app.sln", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("app.slnx", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("sub/dir/app.slnx", true, false, None, false, CredentialCheck::Scan, true, ""),
            ("app.slnf", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/app.slnf", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("x.props", true, true, Some("x.props"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.props", true, true, Some("sub/dir/x.props"), false, CredentialCheck::Undecided, true, "nuget"),
            ("x.targets", true, true, Some("x.targets"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.targets", true, true, Some("sub/dir/x.targets"), false, CredentialCheck::Undecided, true, "nuget"),
            ("x.rsp", true, true, Some("x.rsp"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.rsp", true, true, Some("sub/dir/x.rsp"), false, CredentialCheck::Undecided, true, "nuget"),
            ("x.projitems", true, true, Some("x.projitems"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.projitems", true, true, Some("sub/dir/x.projitems"), false, CredentialCheck::Undecided, true, "nuget"),
            ("x.user", true, true, Some("x.user"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.user", true, true, Some("sub/dir/x.user"), false, CredentialCheck::Undecided, true, "nuget"),
            ("x.msbuild", true, true, Some("x.msbuild"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.msbuild", true, true, Some("sub/dir/x.msbuild"), false, CredentialCheck::Undecided, true, "nuget"),
            ("x.pubxml", true, true, Some("x.pubxml"), false, CredentialCheck::Undecided, true, "nuget"),
            ("sub/dir/x.pubxml", true, true, Some("sub/dir/x.pubxml"), false, CredentialCheck::Undecided, true, "nuget"),
            ("main.tf", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/main.tf", true, false, None, false, CredentialCheck::Scan, false, ""),
            (".terraform.lock.hcl", true, false, None, false, CredentialCheck::Skip, false, ""),
            ("sub/dir/.terraform.lock.hcl", true, false, None, false, CredentialCheck::Skip, false, ""),
            (".terraform-version", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.terraform-version", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("mise.toml", true, true, Some("mise.toml"), false, CredentialCheck::Scan, false, "mise"),
            ("sub/dir/mise.toml", true, true, Some("sub/dir/mise.toml"), false, CredentialCheck::Scan, false, "mise"),
            (".mise.toml", true, true, Some(".mise.toml"), false, CredentialCheck::Scan, false, "mise"),
            ("sub/dir/.mise.toml", true, true, Some("sub/dir/.mise.toml"), false, CredentialCheck::Scan, false, "mise"),
            ("mise.lock", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/mise.lock", false, false, None, false, CredentialCheck::Scan, false, ""),
            (".tool-versions", true, false, None, false, CredentialCheck::Skip, false, ""),
            ("sub/dir/.tool-versions", true, false, None, false, CredentialCheck::Skip, false, ""),
            ("mise.local.toml", true, true, Some("mise.local.toml"), false, CredentialCheck::Undecided, false, "mise"),
            ("sub/dir/mise.local.toml", true, true, Some("sub/dir/mise.local.toml"), false, CredentialCheck::Undecided, false, "mise"),
            (".miserc", true, true, Some(".miserc"), false, CredentialCheck::Undecided, false, "mise"),
            ("sub/dir/.miserc", true, true, Some("sub/dir/.miserc"), false, CredentialCheck::Undecided, false, "mise"),
            (".config/mise.toml", true, true, Some(".config/mise.toml"), false, CredentialCheck::Scan, false, "mise"),
            ("sub/dir/.config/mise.toml", true, true, Some("sub/dir/.config/mise.toml"), false, CredentialCheck::Scan, false, "mise"),
            ("mise/tools.toml", true, true, Some("mise/tools.toml"), false, CredentialCheck::Undecided, false, "mise"),
            ("sub/dir/mise/tools.toml", true, true, Some("sub/dir/mise/tools.toml"), false, CredentialCheck::Undecided, false, "mise"),
            (".mise/tools.toml", true, true, Some(".mise/tools.toml"), false, CredentialCheck::Undecided, false, "mise"),
            ("sub/dir/.mise/tools.toml", true, true, Some("sub/dir/.mise/tools.toml"), false, CredentialCheck::Undecided, false, "mise"),
            ("pom.xml", true, true, Some("pom.xml"), false, CredentialCheck::Undecided, false, "maven"),
            ("sub/dir/pom.xml", true, true, Some("sub/dir/pom.xml"), false, CredentialCheck::Undecided, false, "maven"),
            ("settings.xml", true, true, Some("settings.xml"), false, CredentialCheck::Undecided, false, "maven"),
            ("sub/dir/settings.xml", true, true, Some("sub/dir/settings.xml"), false, CredentialCheck::Undecided, false, "maven"),
            ("custom-settings.xml", true, true, Some("custom-settings.xml"), false, CredentialCheck::Undecided, false, "maven"),
            ("sub/dir/custom-settings.xml", true, true, Some("sub/dir/custom-settings.xml"), false, CredentialCheck::Undecided, false, "maven"),
            (".mvn/maven.config", true, true, Some(".mvn/maven.config"), false, CredentialCheck::Undecided, false, "maven"),
            ("sub/dir/.mvn/maven.config", true, true, Some("sub/dir/.mvn/maven.config"), false, CredentialCheck::Undecided, false, "maven"),
            ("build.gradle.kts", true, true, Some("build.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("sub/dir/build.gradle.kts", true, true, Some("sub/dir/build.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("settings.gradle.kts", true, true, Some("settings.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("sub/dir/settings.gradle.kts", true, true, Some("sub/dir/settings.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("build.gradle", true, true, Some("build.gradle"), false, CredentialCheck::Undecided, false, "gradle"),
            ("sub/dir/build.gradle", true, true, Some("sub/dir/build.gradle"), false, CredentialCheck::Undecided, false, "gradle"),
            ("gradle.lockfile", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/gradle.lockfile", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("gradle-wrapper.properties", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/gradle-wrapper.properties", false, false, None, false, CredentialCheck::Scan, false, ""),
            ("gradle/wrapper/gradle-wrapper.properties", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/gradle/wrapper/gradle-wrapper.properties", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("build-logic/settings.gradle.kts", true, true, Some("build-logic/settings.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("sub/dir/build-logic/settings.gradle.kts", true, true, Some("sub/dir/build-logic/settings.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("buildSrc/build.gradle.kts", true, true, Some("buildSrc/build.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("sub/dir/buildSrc/build.gradle.kts", true, true, Some("sub/dir/buildSrc/build.gradle.kts"), false, CredentialCheck::Undecided, false, "gradle"),
            ("src/main/kotlin/App.kt", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/src/main/kotlin/App.kt", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("flake.nix", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/flake.nix", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("flake.lock", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/flake.lock", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("apm.yml", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/apm.yml", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("apm.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/apm.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("apm.lock.yaml", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("sub/dir/apm.lock.yaml", true, false, None, false, CredentialCheck::Scan, false, ""),
            ("Chart.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/Chart.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("Chart.lock", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/Chart.lock", false, false, None, false, CredentialCheck::Undecided, false, ""),
            (".github/workflows/ci.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.github/workflows/ci.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".gitea/workflows/ci.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.gitea/workflows/ci.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".gitlab-ci.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.gitlab-ci.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".pre-commit-config.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.pre-commit-config.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("devcontainer.json", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/devcontainer.json", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".devcontainer.json", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.devcontainer.json", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("Dockerfile", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/Dockerfile", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("Dockerfile.dev", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/Dockerfile.dev", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("docker-compose.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/docker-compose.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("compose.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/compose.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".copier-answers.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.copier-answers.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            (".woodpecker/ci.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.woodpecker/ci.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("kustomization.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/kustomization.yaml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("action.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/action.yml", true, false, None, false, CredentialCheck::Undecided, false, ""),
            ("README.md", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/README.md", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("foo.toml", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/foo.toml", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("x/settings.json", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/x/settings.json", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("random.txt", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/random.txt", false, false, None, false, CredentialCheck::Undecided, false, ""),
            (".env", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.env", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("WORKSPACE", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/WORKSPACE", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("deno.json", false, false, None, false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/deno.json", false, false, None, false, CredentialCheck::Undecided, false, ""),
            (".bundle", false, false, Some(".bundle/config"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.bundle", false, false, Some("sub/dir/.bundle/config"), false, CredentialCheck::Undecided, false, ""),
            (".bundle/config", true, true, Some(".bundle/config"), false, CredentialCheck::Undecided, false, "bundler"),
            ("sub/dir/.bundle/config", true, true, Some("sub/dir/.bundle/config"), false, CredentialCheck::Undecided, false, "bundler"),
            (".mvn", false, false, Some(".mvn/maven.config"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.mvn", false, false, Some("sub/dir/.mvn/maven.config"), false, CredentialCheck::Undecided, false, ""),
            (".cargo", false, false, Some(".cargo/config.toml"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/.cargo", false, false, Some("sub/dir/.cargo/config.toml"), false, CredentialCheck::Undecided, false, ""),
            ("build-logic", false, false, Some("build-logic/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/build-logic", false, false, Some("sub/dir/build-logic/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("buildSrc", false, false, Some("buildSrc/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/buildSrc", false, false, Some("sub/dir/buildSrc/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("build-conventions", false, false, Some("build-conventions/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/build-conventions", false, false, Some("sub/dir/build-conventions/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("gradle/plugins", false, false, Some("gradle/plugins/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
            ("sub/dir/gradle/plugins", false, false, Some("sub/dir/gradle/plugins/build.gradle.kts"), false, CredentialCheck::Undecided, false, ""),
        ];
        let config = patchula_config::defaults().unwrap();
        let scanner = super::Scanner::new(&config).unwrap();
        let files = super::Files::new();
        for &(path, needs, settings, linked, scope, check, verdict, registry) in TABLE {
            assert_eq!(scanner.needs(path), needs, "{path}: needs");
            assert_eq!(
                super::registry_settings(path),
                settings,
                "{path}: registry_settings"
            );
            assert_eq!(
                super::linked_settings(path).as_deref(),
                linked,
                "{path}: linked_settings"
            );
            assert_eq!(
                super::credential_scope(path),
                scope,
                "{path}: credential_scope"
            );
            assert_eq!(
                super::credential_check(path),
                check,
                "{path}: credential_check"
            );
            assert_eq!(
                super::public::verdict(path, &super::Text::Read(String::new()), &files).is_some(),
                verdict,
                "{path}: public verdict"
            );
            let managers: Vec<&str> = super::Manager::ALL
                .iter()
                .filter(|m| super::ecosystem(**m).registry_file(path))
                .map(|m| m.as_str())
                .collect();
            assert_eq!(managers.join(","), registry, "{path}: registry_file");
        }
    }

    #[test]
    fn every_manager_has_a_descriptor() {
        for manager in super::Manager::ALL {
            // The call is the check: ecosystem() routes YAML-family managers
            // through yaml::descriptor, which panics for a manager missing
            // from yaml::MANAGERS.
            super::ecosystem(*manager);
        }
    }

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

/// Bundler's settings for a project.
pub(crate) fn bundle_config(path: &str) -> bool {
    path == ".bundle/config" || path.ends_with("/.bundle/config")
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
    /// Why lookups that depend on the file at `path` fail, and the warning
    /// a scan that reached its read rounds without it gives.
    pub fn reason(self, path: &str) -> String {
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
