//! Resolve and validate all local overrides before starting batch reads.
use crate::{Error, Result};
use rustovate_config::Config;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) fn load(
    dir: Option<&Path>,
    repos: &BTreeSet<String>,
) -> Result<BTreeMap<String, Config>> {
    let mut configs = BTreeMap::new();
    let Some(dir) = dir else { return Ok(configs) };
    for repo in repos {
        // Repository identities are validated by the caller before path construction.
        let (_, name) = repo.split_once('/').expect("validated repository");
        for relative in [format!("{repo}.pkl"), format!("{name}.pkl")] {
            let path = dir.join(relative);
            let diagnostic = |detail| {
                Error::Configuration(format!(
                    "{repo}: invalid local override {}: {detail}",
                    path.display()
                ))
            };
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    let config = rustovate_config::load(&path)
                        .map_err(|error| diagnostic(format!("{error:#}")))?;
                    configs.insert(repo.clone(), config);
                    break;
                }
                Ok(_) => return Err(diagnostic("not a file".into())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(diagnostic(error.to_string())),
            }
        }
    }
    Ok(configs)
}
