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

use patchula_config::{Config, DependencyKind, Manager, PackageId, Patterns, Scheme, Source};
use serde::{Deserialize, Serialize};
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
pub type Files = BTreeMap<String, String>;
pub fn supported(manager: Manager) -> bool {
    match manager {
        Manager::Npm
        | Manager::Bun
        | Manager::GithubActions
        | Manager::Mise
        | Manager::Cargo
        | Manager::Dockerfile
        | Manager::Compose
        | Manager::Go
        | Manager::Terraform
        | Manager::Kubernetes
        | Manager::Kustomize
        | Manager::Flux
        | Manager::HelmValues
        | Manager::Pyproject
        | Manager::PreCommit
        | Manager::Helm
        | Manager::Devcontainer
        | Manager::Woodpecker
        | Manager::Copier
        | Manager::Maven
        | Manager::Gradle
        | Manager::Nuget
        | Manager::Bundler
        | Manager::Composer
        | Manager::Gitlabci
        | Manager::GitlabciInclude
        | Manager::Nix
        | Manager::Apm
        | Manager::Regex => true,
    }
}

pub struct Scanner {
    config: Config,
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
    /// The files among `paths` that what `files` says makes the Count read
    /// as well: the sources of the builds Gradle settings include, which can
    /// add repositories.
    pub fn more<'a>(&self, files: &Files, paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        if !self.config.managers.contains(&Manager::Gradle) {
            return Vec::new();
        }
        let included = gradle::included_builds(files);
        paths
            .into_iter()
            .filter(|path| {
                !files.contains_key(*path)
                    && gradle::logic_source(path)
                    && included
                        .iter()
                        .any(|folder| path.starts_with(folder.as_str()))
            })
            .map(str::to_owned)
            .collect()
    }
    /// Registry settings are read whatever `files` and `exclude` select:
    /// without them, a scoped package would be looked up on the public
    /// registry, where its name may be someone else's.
    fn registry_file(&self, path: &str) -> bool {
        let npm_extractors = self.config.managers.contains(&Manager::Regex)
            && self
                .config
                .extractors
                .iter()
                .any(|e| e.source == "npm" || e.source.contains('{'));
        let leaf = path.rsplit('/').next().unwrap_or(path);
        (([Manager::Npm, Manager::Bun]
            .iter()
            .any(|m| self.config.managers.contains(m))
            || npm_extractors)
            && matches!(leaf, ".npmrc" | ".yarnrc.yml"))
            || (self.config.managers.contains(&Manager::Pyproject)
                && (python_settings(path) || matches!(leaf, "uv.lock" | "poetry.lock")))
            || (self.config.managers.contains(&Manager::Bundler)
                && (bundle_config(path) || leaf == "Gemfile.lock"))
            || (self.config.managers.contains(&Manager::Composer) && leaf == "composer.lock")
            || (self.config.managers.contains(&Manager::Nuget)
                && (nuget::config_file(path) || nuget::msbuild_file(path)))
            || (self.config.managers.contains(&Manager::Maven) && maven::repository_file(path))
            || (self.config.managers.contains(&Manager::Gradle) && gradle::build_logic(path))
    }
    /// Fail every Maven lookup of a build system whose build could take
    /// artifacts from a repository besides the public ones; see
    /// `maven::private_repositories` and `gradle::private_repositories`.
    fn maven_repositories(&self, files: &Files, deps: &mut [Dependency]) {
        for (manager, reason) in [
            (
                Manager::Maven,
                self.config
                    .managers
                    .contains(&Manager::Maven)
                    .then(|| maven::private_repositories(files))
                    .flatten(),
            ),
            (
                Manager::Gradle,
                self.config
                    .managers
                    .contains(&Manager::Gradle)
                    .then(|| gradle::private_repositories(files))
                    .flatten(),
            ),
        ] {
            let Some(reason) = reason else {
                continue;
            };
            for dep in deps
                .iter_mut()
                .filter(|dep| dep.manager == manager && dep.source == Source::Maven)
            {
                dep.registry = origins::Named::Unusable(reason.clone()).recorded();
            }
        }
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
    fn selected(&self, manager: Manager, path: &str) -> bool {
        if !self.config.managers.contains(&manager)
            || (!self.config.exclude.is_empty() && self.exclude.matches(path))
        {
            return false;
        }
        if let Some((_, p)) = self.selections.iter().find(|(m, _)| *m == manager) {
            return p.matches(path);
        }
        let leaf = path.rsplit('/').next().unwrap_or(path);
        match manager {
            Manager::Npm | Manager::Bun => {
                matches!(
                    leaf,
                    "package.json"
                        | "package-lock.json"
                        | "aube-lock.yaml"
                        | "bun.lock"
                        | "pnpm-workspace.yaml"
                        | ".yarnrc.yml"
                        | ".nvmrc"
                        | ".node-version"
                        | ".npmrc"
                )
            }
            Manager::GithubActions => {
                ([
                    ".github/workflows/",
                    ".gitea/workflows/",
                    ".forgejo/workflows/",
                ]
                .iter()
                .any(|dir| path.starts_with(dir))
                    && matches!(leaf.rsplit('.').next(), Some("yml" | "yaml")))
                    || matches!(leaf, "action.yml" | "action.yaml")
            }
            Manager::Mise => matches!(leaf, "mise.toml" | ".mise.toml" | ".tool-versions"),
            Manager::Regex => self
                .config
                .extractors
                .iter()
                .any(|e| Patterns::compile(&e.paths).is_ok_and(|p| p.matches(path))),
            Manager::Cargo => matches!(
                leaf,
                "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
            ),
            Manager::Dockerfile => {
                leaf == "Dockerfile"
                    || leaf.starts_with("Dockerfile.")
                    || leaf.ends_with(".Dockerfile")
                    || leaf.ends_with(".dockerfile")
            }
            Manager::Compose => {
                (leaf.starts_with("docker-compose") || leaf.starts_with("compose"))
                    && (leaf.ends_with(".yml") || leaf.ends_with(".yaml"))
            }
            Manager::Go => leaf == "go.mod",
            Manager::Terraform => {
                leaf.ends_with(".tf")
                    || matches!(leaf, ".terraform.lock.hcl" | ".terraform-version")
            }
            Manager::Kubernetes | Manager::Flux => {
                leaf.ends_with(".yaml") || leaf.ends_with(".yml")
            }
            Manager::Kustomize => matches!(
                leaf,
                "kustomization.yaml" | "kustomization.yml" | "Kustomization"
            ),
            Manager::HelmValues => {
                (leaf.starts_with("values")
                    || leaf.ends_with("values.yaml")
                    || leaf.ends_with("values.yml"))
                    && (leaf.ends_with(".yaml") || leaf.ends_with(".yml"))
            }
            Manager::Helm => leaf == "Chart.yaml",
            Manager::Maven => leaf == "pom.xml",
            Manager::Gradle => gradle::selected(path),
            Manager::Nuget => nuget::selected(path),
            Manager::Bundler => matches!(leaf, "Gemfile" | "Gemfile.lock" | ".ruby-version"),
            Manager::Composer => matches!(leaf, "composer.json" | "composer.lock"),
            Manager::Gitlabci | Manager::GitlabciInclude => {
                leaf.ends_with(".gitlab-ci.yml") || leaf.ends_with(".gitlab-ci.yaml")
            }
            Manager::Nix => matches!(leaf, "flake.nix" | "flake.lock"),
            Manager::Devcontainer => {
                matches!(leaf, "devcontainer.json" | ".devcontainer.json")
            }
            Manager::PreCommit => {
                matches!(leaf, ".pre-commit-config.yaml" | ".pre-commit-config.yml")
            }
            Manager::Apm => matches!(leaf, "apm.yml" | "apm.yaml"),
            Manager::Woodpecker => {
                let stem = path
                    .strip_suffix(".yml")
                    .or_else(|| path.strip_suffix(".yaml"))
                    .unwrap_or_default();
                stem == ".woodpecker"
                    || stem
                        .strip_prefix(".woodpecker/")
                        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
            }
            Manager::Copier => copier::selected(leaf),
            Manager::Pyproject => matches!(
                leaf,
                "pyproject.toml" | "uv.lock" | "poetry.lock" | ".python-version"
            ),
        }
    }
    pub fn extract(&self, files: &Files) -> Result<Extraction, String> {
        let mut result = Extraction::default();
        let locks = npm::locks(files)?;
        let npmrcs = npm::npmrcs(files);
        let cargo = if self.config.managers.contains(&Manager::Cargo) {
            cargo::documents(files)?
        } else {
            BTreeMap::new()
        };
        let feeds = nuget::Feeds::new(files);
        let terraform = if self.config.managers.contains(&Manager::Terraform) {
            terraform::documents(files)?
        } else {
            BTreeMap::new()
        };
        let yaml_managers = [
            Manager::Compose,
            Manager::Kubernetes,
            Manager::Kustomize,
            Manager::Flux,
            Manager::HelmValues,
        ];
        // A file that does not parse (a template such as ERB-rendered YAML)
        // is skipped with a warning. Warnings suppress branch cleanup, so the
        // skipped file's open updates are never closed as obsolete.
        let mut documents = BTreeMap::new();
        let mut locations = BTreeMap::new();
        for (path, text) in files
            .iter()
            .filter(|(path, _)| yaml_managers.iter().any(|m| self.selected(*m, path)))
        {
            match yaml::docs(text).and_then(|d| Ok((d, yaml_locations::parse(text)?))) {
                Ok((docs, spans)) => {
                    documents.insert(path.as_str(), docs);
                    locations.insert(path.as_str(), spans);
                }
                Err(e) => result.warnings.push(format!("{path}: skipped: {e}")),
            }
        }
        let repos = yaml::repositories(&documents);
        let task_files = if self.config.managers.contains(&Manager::Mise) {
            mise::task_files(
                files
                    .iter()
                    .filter(|(path, _)| self.selected(Manager::Mise, path)),
                files,
            )
        } else {
            BTreeMap::new()
        };
        // package.json shared by npm/bun is scanned once, preferring bun only
        // when npm is disabled. Lockfiles provide resolution, never transitive deps.
        let node = if self.config.managers.contains(&Manager::Npm) {
            Manager::Npm
        } else {
            Manager::Bun
        };
        for (path, text) in files {
            let extraction = if self.selected(node, path) && path.ends_with("package.json") {
                npm::extract(path, text, node, &locks).map(|mut deps| {
                    npm::assign_registries(&mut deps, &npmrcs);
                    deps
                })
            } else if self.selected(node, path)
                && (path.ends_with("pnpm-workspace.yaml") || path.ends_with(".yarnrc.yml"))
            {
                npm::catalogs(path, text, node).map(|mut deps| {
                    npm::assign_registries(&mut deps, &npmrcs);
                    deps
                })
            } else if self.selected(node, path) && path.ends_with(".npmrc") {
                // Read for the registries of the manifests beside and below it.
                Ok(Vec::new())
            } else if self.selected(node, path)
                && (path.ends_with(".nvmrc") || path.ends_with(".node-version"))
            {
                Ok(npm::node_version(path, text, node))
            } else if self.selected(Manager::GithubActions, path) {
                actions::extract(path, text)
            } else if self.selected(Manager::Mise, path) {
                mise::extract(
                    path,
                    text,
                    task_files.get(path).map(String::as_str),
                    &mut result.skipped,
                )
            } else if self.selected(Manager::Cargo, path)
                && (path.ends_with("rust-toolchain") || path.ends_with("rust-toolchain.toml"))
            {
                toolchains::rust(path, text)
            } else if self.selected(Manager::Cargo, path) {
                cargo::extract(path, text, &cargo, &mut result.skipped)
            } else if self.selected(Manager::Dockerfile, path) {
                images::dockerfile(path, text)
            } else if self.selected(Manager::Go, path) {
                go::extract(path, text)
            } else if self.selected(Manager::Terraform, path)
                && path.ends_with(".terraform-version")
            {
                Ok(toolchains::terraform(path, text))
            } else if self.selected(Manager::Terraform, path) {
                terraform::extract(path, text, &terraform)
            } else if self.selected(Manager::Composer, path) {
                composer::extract(path, text, files, &mut result.skipped)
            } else if self.selected(Manager::Nix, path) {
                nix::extract(path, text, files, &mut result.skipped)
            } else if self.selected(Manager::Bundler, path) {
                bundler::extract(path, text, files, &mut result.skipped)
            } else if self.selected(Manager::Nuget, path) {
                nuget::extract(path, text).map(|mut deps| {
                    feeds.assign(&mut deps);
                    deps
                })
            } else if self.selected(Manager::Gradle, path) {
                gradle::extract(path, text)
            } else if self.selected(Manager::Maven, path) {
                maven::extract(path, text)
            } else if self.selected(Manager::Devcontainer, path) {
                devcontainer::extract(path, text)
            } else if self.selected(Manager::Apm, path) {
                apm::extract(path, text, &mut result.skipped)
            } else if self.selected(Manager::Woodpecker, path) {
                woodpecker::extract(path, text, &mut result.skipped)
            } else if self.selected(Manager::Copier, path) {
                copier::extract(path, text, &mut result.skipped)
            } else if self.selected(Manager::Helm, path) {
                helm::extract(path, text)
            } else if self.selected(Manager::PreCommit, path) {
                precommit::extract(path, text)
            } else if self.selected(Manager::Pyproject, path) {
                pyproject::extract(path, text, files, &mut result.skipped)
            } else {
                Ok(Vec::new())
            };
            match extraction {
                Ok(dependencies) => result.dependencies.extend(dependencies),
                Err(e) => result.warnings.push(format!("{path}: skipped: {e}")),
            }
            for manager in [
                Manager::Compose,
                Manager::Kubernetes,
                Manager::Kustomize,
                Manager::Flux,
                Manager::HelmValues,
            ] {
                if self.selected(manager, path)
                    && let (Some(docs), Some(spans)) =
                        (documents.get(path.as_str()), locations.get(path.as_str()))
                {
                    match yaml::extract(manager, path, text, docs, spans, &repos) {
                        Ok(dependencies) => result.dependencies.extend(dependencies),
                        Err(e) => result.warnings.push(format!("{path}: skipped: {e}")),
                    }
                }
            }
            for manager in [Manager::Gitlabci, Manager::GitlabciInclude] {
                if self.selected(manager, path) {
                    let host = &self.config.gitlab.host;
                    match gitlabci::extract(manager, path, text, host, &mut result.skipped) {
                        Ok(dependencies) => result.dependencies.extend(dependencies),
                        Err(e) => result.warnings.push(format!("{path}: skipped: {e}")),
                    }
                }
            }
            if self.selected(Manager::Regex, path) {
                result.dependencies.extend(custom::extract(
                    path,
                    text,
                    &self.config.extractors,
                    &npmrcs,
                )?);
            }
        }
        if self.config.managers.contains(&Manager::Gradle) {
            result
                .dependencies
                .extend(gradle_vars::extract(files, |path| {
                    self.selected(Manager::Gradle, path)
                }));
        }
        self.maven_repositories(files, &mut result.dependencies);
        result.locked = locked::cargo(files, &cargo, &result.dependencies);
        let npm = node == Manager::Npm && self.config.managers.contains(&Manager::Npm);
        result
            .locked
            .extend(locked::aube(files, &mut result.dependencies, node, npm)?);
        if npm {
            result
                .locked
                .extend(locked::npm(files, &locks, &result.dependencies));
        }
        npm::assign_locked_registries(&mut result.locked, &npmrcs);
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

pub fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .expect("enum serializes")
        .as_str()
        .expect("enum word")
        .into()
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
}

/// A file that can name a Python package index.
fn python_settings(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    matches!(
        leaf,
        "pyproject.toml" | "uv.toml" | "pdm.toml" | "pixi.toml" | "hatch.toml"
    )
}

/// Bundler's settings for a project.
fn bundle_config(path: &str) -> bool {
    path == ".bundle/config" || path.ends_with("/.bundle/config")
}

/// The settings file a symlink at `path` could stand for: the path itself
/// when it is a settings file, or the `config` inside a linked `.bundle`
/// directory, which Bundler follows.
pub fn linked_settings(path: &str) -> Option<String> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    if registry_settings(path) {
        Some(path.to_owned())
    } else {
        (leaf == ".bundle").then(|| format!("{path}/config"))
    }
}

/// What a settings file the Count could not read becomes: text no TOML or
/// YAML mapping parser accepts, so it is unreadable rather than absent.
pub const UNREADABLE_SETTINGS: &str = "= the Count could not read this file\n";

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
