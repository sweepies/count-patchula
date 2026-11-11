//! The runtime every ecosystem's lock regeneration shares: the run's
//! driver, checkout, baseline, config and tool versions, plus the small
//! filesystem helpers the drivers use.
use super::*;

/// Everything one directory's lock regeneration needs.
pub(super) struct Context<'a> {
    pub driver: &'a Path,
    pub root: &'a Path,
    pub baseline: &'a BTreeMap<String, BaseFile>,
    pub directory: &'a str,
    pub config: &'a Config,
    pub versions: &'a BTreeMap<String, String>,
    pub maintenance: bool,
    /// Whether every file the run can read decided public-only, so its
    /// tools may share the `public` cache partition.
    pub public: bool,
}

pub(super) use super::contract::join;

pub(super) fn version(versions: &BTreeMap<String, String>, tool: &str) -> Result<String, String> {
    versions
        .get(tool)
        .cloned()
        .ok_or_else(|| format!("missing {tool} version"))
}

/// Files only this user can read, removed when dropped, so tools that run
/// later never see what they hold.
#[derive(Default)]
pub(super) struct Private(Vec<std::path::PathBuf>);

impl Private {
    /// Write `text` to a new file at `path`, replacing whatever is there
    /// without following it. The file is removed on drop even if writing fails.
    pub(super) fn write(&mut self, path: &Path, text: &str) -> Result<(), String> {
        use std::io::Write;
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(path).map_err(|e| e.to_string())?;
        self.0.push(path.to_path_buf());
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}
