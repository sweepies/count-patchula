//! Repository configs evaluate against supplied files, never the host filesystem.
use crate::*;
use std::collections::BTreeMap;

pub fn load_files(path: &str, files: BTreeMap<String, String>) -> miette::Result<Config> {
    let source = files.get(path).cloned().unwrap_or_default();
    let value = evaluate(path, files)?;
    decode(Path::new(path), &source, value)
}

pub(super) fn evaluate(
    path: &str,
    files: BTreeMap<String, String>,
) -> miette::Result<serde_json::Value> {
    evaluate_modules(path, files, false)
}

pub(super) fn evaluate_modules(
    path: &str,
    files: BTreeMap<String, String>,
    local: bool,
) -> miette::Result<serde_json::Value> {
    let mut evaluator = pklr::Evaluator::with_capabilities(Files { files, local });
    evaluator.set_offline(true);
    evaluator.set_allowed_resources(Vec::new());
    evaluator.set_environment_variables(BTreeMap::new());
    let value = evaluator
        .eval_file(Path::new(path))
        .map_err(miette::Report::new)?;
    Ok(value.to_json())
}
struct Files {
    files: BTreeMap<String, String>,
    local: bool,
}
fn denied() -> pklr::Error {
    pklr::Error::Eval("repository configuration may access only supplied local modules".into())
}
pub(super) fn normalize(path: &Path, local: bool) -> pklr::Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return Err(denied());
                }
            }
            std::path::Component::CurDir => {}
            part => out.push(part.as_os_str()),
        }
    }
    if !local && !out.starts_with("/repo") {
        return Err(denied());
    }
    Ok(out)
}
impl pklr::EvalCapabilities for Files {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        let path = normalize(path, self.local)?;
        self.files
            .get(path.to_str().ok_or_else(denied)?)
            .cloned()
            .or_else(|| {
                (!self.local && path.file_name().is_some_and(|name| name == SCHEMA_NAME))
                    .then(|| SCHEMA.into())
            })
            .ok_or_else(|| pklr::Error::ImportNotFound(path.display().to_string()))
    }
    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        let path = normalize(path, self.local)?;
        Ok(self.files.contains_key(path.to_str().ok_or_else(denied)?)
            || (!self.local && path.file_name().is_some_and(|name| name == SCHEMA_NAME)))
    }
    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        normalize(path, self.local)
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
    #[test]
    fn isolated_modules_and_no_host_imports() {
        let files = BTreeMap::from([
            ("/repo/patchula-schema.pkl".into(), SCHEMA.into()),
            (
                "/repo/patchula.pkl".into(),
                "amends \"patchula-schema.pkl\"\nmanagers = new Listing {\"npm\"}\n".into(),
            ),
        ]);
        let config = load_files("/repo/patchula.pkl", files.clone()).unwrap();
        assert_eq!(config.managers, vec![Manager::Npm]);
        let mut files = files;
        files.insert("/repo/patchula.pkl".into(), "amends \"/etc/passwd\"".into());
        assert!(load_files("/repo/patchula.pkl", files).is_err());
    }
}
