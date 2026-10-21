//! Pure extraction and surgical manifest edits: no tools, network or filesystem access.
#![forbid(unsafe_code)]
mod actions;
mod bundler;
mod edit;
pub use actions::replacement as action_replacement;
pub use actions::runner_replacement;
pub use edit::{Edit, apply, edits};
/// Included in persistent extraction fingerprints because backend mappings can change.
pub const MISE_REGISTRY: &str = rustovate_mise::REGISTRY_JSON;
mod cargo;
mod composer;
mod custom;
mod devcontainer;
mod go;
mod gradle;
mod helm;
mod images;
mod maven;
mod mise;
mod npm;
mod nuget;
mod precommit;
mod pyproject;
mod requirements;
mod terraform;
mod toolchains;
mod yaml;
mod yaml_locations;

use rustovate_config::{Config, DependencyKind, Manager, Patterns, Scheme, Source};
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
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Extraction {
    pub dependencies: Vec<Dependency>,
    pub warnings: Vec<String>,
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
                (path.starts_with(".github/workflows/")
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
        let documents: BTreeMap<_, _> = files
            .iter()
            .filter(|(path, _)| yaml_managers.iter().any(|m| self.selected(*m, path)))
            .map(|(p, t)| Ok((p.as_str(), yaml::docs(t).map_err(|e| format!("{p}: {e}"))?)))
            .collect::<Result<_, String>>()?;
        let repos = yaml::repositories(&documents);
        let locations: BTreeMap<_, _> = documents
            .keys()
            .map(|path| {
                Ok((
                    *path,
                    yaml_locations::parse(&files[*path]).map_err(|e| format!("{path}: {e}"))?,
                ))
            })
            .collect::<Result<_, String>>()?;
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
            result
                .dependencies
                .extend(extraction.map_err(|e| format!("{path}: {e}"))?);
            for manager in [
                Manager::Compose,
                Manager::Kubernetes,
                Manager::Kustomize,
                Manager::Flux,
                Manager::HelmValues,
            ] {
                if self.selected(manager, path) {
                    result.dependencies.extend(
                        yaml::extract(
                            manager,
                            path,
                            text,
                            &documents[path.as_str()],
                            &locations[path.as_str()],
                            &repos,
                        )
                        .map_err(|e| format!("{path}: {e}"))?,
                    );
                }
            }
            if self.selected(Manager::Regex, path) {
                result
                    .dependencies
                    .extend(custom::extract(path, text, &self.config.extractors)?);
            }
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
