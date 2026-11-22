//! Configs evaluate against supplied files, never the host filesystem.
use crate::*;
use std::collections::BTreeMap;

/// The config at `path` evaluated against `files`. The bundled schema
/// stands in for a `PatchulaConfig.pkl` beside it that `files` leaves out.
pub fn load_files(path: &str, mut files: BTreeMap<String, String>) -> miette::Result<Config> {
    let schema = Path::new(path).with_file_name(SCHEMA_NAME);
    if let Some(schema) = schema.to_str() {
        files
            .entry(schema.to_owned())
            .or_insert_with(|| SCHEMA.to_owned());
    }
    let source = files.get(path).cloned().unwrap_or_default();
    let value = evaluate(path, files)?;
    decode(Path::new(path), &source, value)
}

/// The module at `path` evaluated offline, reading only `files`: no
/// environment, no resources, no network.
pub(super) fn evaluate(
    path: &str,
    files: BTreeMap<String, String>,
) -> miette::Result<serde_json::Value> {
    let mut evaluator = pklr::Evaluator::with_capabilities(Files(files));
    evaluator.set_offline(true);
    evaluator.set_allowed_resources(Vec::new());
    evaluator.set_environment_variables(BTreeMap::new());
    let value = evaluator
        .eval_file(Path::new(path))
        .map_err(miette::Report::new)?;
    Ok(value.to_json())
}
/// The modules an evaluation may read, by normalized path.
struct Files(BTreeMap<String, String>);
fn denied() -> pklr::Error {
    pklr::Error::Eval("configuration may access only supplied local modules".into())
}
fn normalize(path: &Path) -> pklr::Result<PathBuf> {
    crate::normalize_path(path).ok_or_else(denied)
}
impl Files {
    fn get(&self, path: &Path) -> pklr::Result<Option<&String>> {
        let path = normalize(path)?;
        Ok(self.0.get(path.to_str().ok_or_else(denied)?))
    }
}
impl pklr::EvalCapabilities for Files {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.get(path)?
            .cloned()
            .ok_or_else(|| pklr::Error::ImportNotFound(path.display().to_string()))
    }
    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        Ok(self.get(path)?.is_some())
    }
    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        normalize(path)
    }
    fn read_bytes(&mut self, path: &Path) -> pklr::Result<Vec<u8>> {
        self.read_to_string(path).map(String::into_bytes)
    }
    fn read_env(&mut self, _: &str) -> pklr::Result<Option<String>> {
        Err(denied())
    }
    fn fetch_text(&mut self, _: &str) -> pklr::Result<String> {
        Err(denied())
    }
    fn fetch_bytes(&mut self, _: &str) -> pklr::Result<Vec<u8>> {
        Err(denied())
    }
    fn temp_dir(&mut self, _: &str) -> pklr::Result<PathBuf> {
        Err(denied())
    }
    fn glob(&mut self, _: &Path, _: &str) -> pklr::Result<Vec<PathBuf>> {
        Err(denied())
    }
    fn create_dir_all(&mut self, _: &Path) -> pklr::Result<()> {
        Err(denied())
    }
    fn write_atomic(&mut self, _: &Path, _: &[u8]) -> pklr::Result<()> {
        Err(denied())
    }
    fn remove_file(&mut self, _: &Path) -> pklr::Result<()> {
        Err(denied())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use patchula_words::Manager;
    #[test]
    fn isolated_modules_and_no_host_imports() {
        let files = BTreeMap::from([
            ("/repo/PatchulaConfig.pkl".into(), SCHEMA.into()),
            (
                "/repo/patchula.pkl".into(),
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing {\"npm\"}\n".into(),
            ),
        ]);
        let config = load_files("/repo/patchula.pkl", files.clone()).unwrap();
        assert_eq!(config.managers, vec![Manager::Npm]);
        let mut files = files;
        files.insert("/repo/patchula.pkl".into(), "amends \"/etc/passwd\"".into());
        assert!(load_files("/repo/patchula.pkl", files).is_err());
    }
}
