//! Strict, local Pkl configuration evaluation and discovery.
#![forbid(unsafe_code)]

mod model;
mod validate;
pub use model::*;

use miette::{Diagnostic, IntoDiagnostic, NamedSource, SourceSpan, WrapErr};
use std::path::{Path, PathBuf};

/// The schema is part of the executable, independent of the current directory.
pub const SCHEMA: &str = include_str!("../../../Rustovate.pkl");

/// Locate a repository config, preferring `.github/rustovate.pkl`.
/// Absence is distinct from an unreadable path so callers can skip unconfigured repos.
pub fn discover(repo: &Path) -> std::io::Result<Option<PathBuf>> {
    for relative in [".github/rustovate.pkl", "rustovate.pkl"] {
        let path = repo.join(relative);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{} is not a config file", path.display()),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Evaluate a file or discover a config under a repository directory.
/// Local imports/amends work; network imports and external resource reads are disabled.
pub fn load(path: &Path) -> miette::Result<Config> {
    let path = if path.is_dir() {
        discover(path)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot discover config in {}", path.display()))?
            .ok_or_else(|| {
                miette::miette!(
                    "no rustovate config in {}; checked .github/rustovate.pkl then rustovate.pkl",
                    path.display()
                )
            })?
    } else {
        path.to_owned()
    };
    let source = std::fs::read_to_string(&path)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot read config {}", path.display()))?;
    let json = evaluate_local(&path, "Rustovate.pkl", SCHEMA)?;
    decode(&path, &source, json)
}

/// Restricted operator modules share the repository evaluator's offline boundary.
/// The bundled schema resolves even when it has not been copied beside the file.
pub fn evaluate_local(
    path: &Path,
    schema_name: &str,
    schema: &str,
) -> miette::Result<serde_json::Value> {
    // Evaluate exactly like API-supplied repository modules. In particular a
    // missing schema must be a real module, so Pkl retains nested class defaults.
    let absolute = std::path::absolute(path).into_diagnostic()?;
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![absolute.clone()];
    let imports = regex::Regex::new(r#"(?:amends|extends|import\*?)\s*\(?\s*"([^"]+)""#)
        .expect("import regex");
    while let Some(module) = pending.pop() {
        let virtual_path = virtual_files::local_path(&module)?;
        if files.contains_key(&virtual_path) {
            continue;
        }
        if files.len() >= 100 {
            return Err(miette::miette!("config import budget exceeded"));
        }
        let text = match std::fs::read_to_string(&module) {
            Ok(text) => text,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && module.file_name().is_some_and(|name| name == schema_name) =>
            {
                schema.into()
            }
            Err(error) => {
                return Err(error)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("cannot read config module {}", module.display()));
            }
        };
        for captures in imports.captures_iter(&text) {
            let relative = &captures[1];
            if relative.contains(':') || relative.starts_with('/') {
                return Err(miette::miette!(
                    "config imports must be relative local modules; remote imports are disabled"
                ));
            }
            pending.push(module.parent().expect("absolute file").join(relative));
        }
        files.insert(virtual_path, text);
    }
    virtual_files::evaluate(&virtual_files::local_path(&absolute)?, files)
}

fn decode(path: &Path, source: &str, json: serde_json::Value) -> miette::Result<Config> {
    let config: Config = serde_path_to_error::deserialize(json).map_err(|error| {
        let mut field = error.path().to_string();
        let detail = error.inner().to_string();
        if let Some(unknown) = detail
            .strip_prefix("unknown field `")
            .and_then(|text| text.split('`').next())
        {
            field = if field == "." {
                unknown.to_owned()
            } else if field.rsplit('.').next() == Some(unknown) {
                field
            } else {
                format!("{field}.{unknown}")
            };
        }
        let message = format!("{field}: {detail}");
        problem(path, source, &field, &message)
    })?;
    config
        .validate()
        .map_err(|issue| problem(path, source, &issue.field, &issue.message))?;
    Ok(config)
}

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error("invalid configuration {path}: {message}")]
#[diagnostic(code(rustovate::config))]
struct ConfigProblem {
    path: String,
    message: String,
    #[source_code]
    src: NamedSource<String>,
    #[label("check this property")]
    span: Option<SourceSpan>,
}

fn problem(path: &Path, source: &str, field: &str, message: &str) -> miette::Report {
    // Highlight only an unambiguous property assignment in this file. Inherited
    // properties have no source map after evaluation; never invent their location.
    let leaf = field.rsplit('.').next().unwrap_or(field);
    let leaf = leaf.split('[').next().unwrap_or(leaf);
    let assignments: Vec<_> = source
        .match_indices(leaf)
        .filter(|(offset, _)| {
            let before = &source[..*offset];
            let after = &source[*offset + leaf.len()..];
            before
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                && after.trim_start().starts_with(['=', ':', '{'])
        })
        .collect();
    let span = if !leaf.is_empty() && assignments.len() == 1 {
        Some((assignments[0].0, leaf.len()).into())
    } else {
        None
    };
    miette::Report::new(ConfigProblem {
        path: path.display().to_string(),
        message: message.to_owned(),
        src: NamedSource::new(path.display().to_string(), source.to_owned()),
        span,
    })
}

mod matching;
pub use matching::Patterns;

mod virtual_files;
pub use virtual_files::load_files;
