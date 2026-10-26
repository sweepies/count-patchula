//! Pure extraction and surgical manifest edits: no tools, network or filesystem access.
#![forbid(unsafe_code)]
mod actions;
mod bundler;
mod copier;
mod edit;
pub use actions::replacement as action_replacement;
pub use actions::runner_replacement;
pub use edit::{Edit, apply, edits};
mod cargo;
mod composer;
mod custom;
mod devcontainer;
mod go;
mod gradle;
mod gradle_vars;
mod helm;
mod images;
mod locked;
pub use locked::Locked;
mod maven;
mod mise;
mod npm;
mod nuget;
mod precommit;
mod pyproject;
mod requirements;
mod terraform;
mod toolchains;
mod woodpecker;
mod yaml;
mod yaml_locations;

use patchula_config::{Config, DependencyKind, Manager, Patterns, Scheme, Source};
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
}
impl Dependency {
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
    /// An action or reusable workflow looked up by its repository's tags,
    /// on GitHub or on a Gitea/Forgejo host.
    pub fn tagged_action(&self) -> bool {
        self.manager == Manager::GithubActions
            && matches!(self.source, Source::GithubTags | Source::ForgeTags)
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
        | Manager::Requirements
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
        self.config.managers.iter().any(|m| self.selected(*m, path))
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
                        | "bun.lock"
                        | "pnpm-workspace.yaml"
                        | ".yarnrc.yml"
                        | ".nvmrc"
                        | ".node-version"
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
            Manager::Devcontainer => {
                matches!(leaf, "devcontainer.json" | ".devcontainer.json")
            }
            Manager::PreCommit => {
                matches!(leaf, ".pre-commit-config.yaml" | ".pre-commit-config.yml")
            }
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
            Manager::Requirements => {
                (leaf.starts_with("requirements")
                    && (leaf.ends_with(".txt") || leaf.ends_with(".in")))
                    || (path.contains("requirements/")
                        && (leaf.ends_with(".txt") || leaf.ends_with(".in")))
            }
        }
    }
    pub fn extract(&self, files: &Files) -> Result<Extraction, String> {
        let mut result = Extraction::default();
        let locks = npm::locks(files)?;
        let cargo = if self.config.managers.contains(&Manager::Cargo) {
            cargo::documents(files)?
        } else {
            BTreeMap::new()
        };
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
        // package.json shared by npm/bun is scanned once, preferring bun only
        // when npm is disabled. Lockfiles provide resolution, never transitive deps.
        let node = if self.config.managers.contains(&Manager::Npm) {
            Manager::Npm
        } else {
            Manager::Bun
        };
        for (path, text) in files {
            let extraction = if self.selected(node, path) && path.ends_with("package.json") {
                npm::extract(path, text, node, &locks)
            } else if self.selected(node, path)
                && (path.ends_with("pnpm-workspace.yaml") || path.ends_with(".yarnrc.yml"))
            {
                npm::catalogs(path, text, node)
            } else if self.selected(node, path)
                && (path.ends_with(".nvmrc") || path.ends_with(".node-version"))
            {
                Ok(npm::node_version(path, text, node))
            } else if self.selected(Manager::GithubActions, path) {
                actions::extract(path, text)
            } else if self.selected(Manager::Mise, path) {
                mise::extract(path, text)
            } else if self.selected(Manager::Cargo, path)
                && (path.ends_with("rust-toolchain") || path.ends_with("rust-toolchain.toml"))
            {
                toolchains::rust(path, text)
            } else if self.selected(Manager::Cargo, path) {
                cargo::extract(path, text, &cargo)
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
                composer::extract(path, text, files)
            } else if self.selected(Manager::Bundler, path) {
                bundler::extract(path, text, files)
            } else if self.selected(Manager::Nuget, path) {
                nuget::extract(path, text)
            } else if self.selected(Manager::Gradle, path) {
                gradle::extract(path, text)
            } else if self.selected(Manager::Maven, path) {
                maven::extract(path, text)
            } else if self.selected(Manager::Devcontainer, path) {
                devcontainer::extract(path, text)
            } else if self.selected(Manager::Woodpecker, path) {
                woodpecker::extract(path, text, &mut result.skipped)
            } else if self.selected(Manager::Copier, path) {
                copier::extract(path, text, &mut result.skipped)
            } else if self.selected(Manager::Helm, path) {
                helm::extract(path, text)
            } else if self.selected(Manager::PreCommit, path) {
                precommit::extract(path, text)
            } else if self.selected(Manager::Pyproject, path) {
                pyproject::extract(path, text, files)
            } else if self.selected(Manager::Requirements, path) {
                requirements::extract(path, text)
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
            if self.selected(Manager::Regex, path) {
                result
                    .dependencies
                    .extend(custom::extract(path, text, &self.config.extractors)?);
            }
        }
        if self.config.managers.contains(&Manager::Gradle) {
            result
                .dependencies
                .extend(gradle_vars::extract(files, |path| {
                    self.selected(Manager::Gradle, path)
                }));
        }
        result.locked = locked::cargo(files, &cargo, &result.dependencies);
        if node == Manager::Npm && self.config.managers.contains(&Manager::Npm) {
            result
                .locked
                .extend(locked::npm(files, &locks, &result.dependencies));
        }
        Ok(result)
    }
}
pub fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .expect("enum serializes")
        .as_str()
        .expect("enum word")
        .into()
}
