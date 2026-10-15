//! Repository configs evaluate against supplied files, never the host filesystem.
use crate::*;
use std::collections::BTreeMap;

pub fn load_files(path: &str, files: BTreeMap<String, String>) -> miette::Result<Config> {
    let mut evaluator = pklr::Evaluator::with_capabilities(Files(files));
    evaluator.set_offline(true);
    evaluator.set_allowed_resources(Vec::new());
    evaluator.set_environment_variables(BTreeMap::new());
    let value = evaluator
        .eval_file(Path::new(path))
        .map_err(miette::Report::new)?;
    let config: Config = serde_json::from_value(value.to_json()).into_diagnostic()?;
    config
        .validate()
        .map_err(|issue| miette::miette!("{}", issue.message))?;
    Ok(config)
}
struct Files(BTreeMap<String, String>);
fn denied() -> pklr::Error {
    pklr::Error::Eval("repository configuration may access only supplied local modules".into())
}
fn clean(path: &Path) -> pklr::Result<PathBuf> {
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
    if !out.starts_with("/repo") {
        return Err(denied());
    }
    Ok(out)
}
impl pklr::EvalCapabilities for Files {
    fn read_to_string(&mut self, path: &Path) -> pklr::Result<String> {
        self.0
            .get(clean(path)?.to_str().ok_or_else(denied)?)
            .cloned()
            .ok_or_else(|| pklr::Error::ImportNotFound(path.display().to_string()))
    }
    fn path_exists(&mut self, path: &Path) -> pklr::Result<bool> {
        Ok(self
            .0
            .contains_key(clean(path)?.to_str().ok_or_else(denied)?))
    }
    fn canonicalize(&mut self, path: &Path) -> pklr::Result<PathBuf> {
        clean(path)
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
            ("/repo/Rustovate.pkl".into(), SCHEMA.into()),
            (
                "/repo/.github/rustovate.pkl".into(),
                "amends \"../Rustovate.pkl\"\nmanagers = new Listing {\"npm\"}\n".into(),
            ),
        ]);
        let config = load_files("/repo/.github/rustovate.pkl", files.clone()).unwrap();
        assert_eq!(config.managers, vec![Manager::Npm]);
        let mut files = files;
        files.insert(
            "/repo/.github/rustovate.pkl".into(),
            "amends \"/etc/passwd\"".into(),
        );
        assert!(load_files("/repo/.github/rustovate.pkl", files).is_err());
    }
}
