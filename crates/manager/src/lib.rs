//! Pure extraction: no tools, network, filesystem access or manifest rewriting.
#![forbid(unsafe_code)]
mod actions;
mod custom;
mod mise;
mod npm;

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
    matches!(
        manager,
        Manager::Npm | Manager::Bun | Manager::GithubActions | Manager::Mise | Manager::Regex
    )
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
                matches!(leaf, "package.json" | "package-lock.json" | "bun.lock")
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
            _ => false,
        }
    }
    pub fn extract(&self, files: &Files) -> Result<Extraction, String> {
        let mut result = Extraction::default();
        let locks = npm::locks(files)?;
        for manager in &self.config.managers {
            if !supported(*manager) {
                result.warnings.push(format!(
                    "manager {} is not implemented in this wave",
                    word(manager)
                ));
            }
        }
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
            } else if self.selected(Manager::GithubActions, path) {
                actions::extract(path, text)
            } else if self.selected(Manager::Mise, path) {
                mise::extract(path, text)
            } else {
                Ok(Vec::new())
            };
            result
                .dependencies
                .extend(extraction.map_err(|e| format!("{path}: {e}"))?);
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
