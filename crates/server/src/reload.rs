//! The live settings snapshot: invalid edits keep the last good settings.
use futures_util::FutureExt as _;
use patchula_config::settings::{SCHEMA, SCHEMA_NAME, Settings};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, RwLock},
};

/// Every module a settings evaluation reads, by path, with its text.
type Modules = BTreeMap<String, String>;

/// Invalid edits leave the active snapshot usable but readiness reports the error.
#[derive(Clone)]
pub struct Reload(Arc<RwLock<Snapshot>>);
struct Snapshot {
    settings: Settings,
    valid: bool,
    /// The modules the last refresh evaluated, so an unchanged file and
    /// imports skip evaluation.
    read: Option<Modules>,
}
impl Reload {
    pub fn new(settings: Settings) -> Self {
        Self(Arc::new(RwLock::new(Snapshot {
            settings,
            valid: true,
            read: None,
        })))
    }
    pub fn current(&self) -> Settings {
        self.0.read().expect("settings lock").settings.clone()
    }
    pub fn valid(&self) -> bool {
        self.0.read().expect("settings lock").valid
    }
    /// Load `path` again when it or a module it imports changed since the
    /// last refresh. Settings evaluate with no environment, resources or
    /// network, so the modules' text decides the result: reading them is a
    /// few file reads, where evaluating them runs Pkl.
    pub fn refresh(&self, path: &Path) {
        let read = modules(path);
        if read.is_some() && self.0.read().expect("settings lock").read == read {
            return;
        }
        // The engine-side rules (registry prefixes, mirrors, host
        // concurrency) reject a file the same way load's own validation does.
        let result = Settings::load(path)
            .and_then(|settings| patchula_engine::settings::checked(settings, Some(path)));
        let mut state = self.0.write().expect("settings lock");
        state.read = read;
        match result {
            Ok(settings) => {
                if !state.valid || settings != state.settings {
                    tracing::info!(target: "patchula_server", "settings loaded");
                }
                state.settings = settings;
                state.valid = true;
            }
            Err(error) => {
                if state.valid {
                    tracing::error!(target: "patchula_server", %error, "settings rejected; keeping last good settings; run config check-settings for diagnostics");
                }
                state.valid = false;
            }
        }
    }
}

/// The modules `Settings::load` evaluates for `path`, walked the way it
/// walks them; `None` when the walk fails, so the load reports why.
fn modules(path: &Path) -> Option<Modules> {
    let entry = std::path::absolute(path).ok()?.to_str()?.to_owned();
    // The reads are synchronous, so the walk completes in its first poll.
    patchula_config::import_files(&entry, SCHEMA_NAME, SCHEMA, &mut |module| {
        std::future::ready(match std::fs::read_to_string(module) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        })
    })
    .now_or_never()?
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_keeps_last_good_settings_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let source = "amends \"PatchulaSettings.pkl\"\nconcurrency = 3\n";
        std::fs::write(&path, source).unwrap();
        let settings = Settings::load(&path).unwrap();
        let reload = Reload::new(settings);
        std::fs::write(&path, "amends \"PatchulaSettings.pkl\"\nconcurrency = 0\n").unwrap();
        reload.refresh(&path);
        assert!(!reload.valid());
        assert_eq!(reload.current().concurrency, 3);
        std::fs::write(&path, source.replace("3", "5")).unwrap();
        reload.refresh(&path);
        assert!(reload.valid());
        assert_eq!(reload.current().concurrency, 5);
        std::fs::write(
            &path,
            "amends \"PatchulaSettings.pkl\"\nwebhookSecret = \"forbidden\"\n",
        )
        .unwrap();
        assert!(Settings::load(&path).is_err());
    }

    /// An unchanged file is not evaluated again, and an edit to a module
    /// it imports reloads as an edit to the file itself does.
    #[test]
    fn reload_skips_unchanged_modules_and_follows_imports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let shared = dir.path().join("shared.pkl");
        std::fs::write(&shared, "concurrency = 3\n").unwrap();
        std::fs::write(
            &path,
            "amends \"PatchulaSettings.pkl\"\nimport \"shared.pkl\"\nconcurrency = shared.concurrency\n",
        )
        .unwrap();
        let reload = Reload::new(Settings::load(&path).unwrap());
        reload.refresh(&path);
        assert_eq!(reload.current().concurrency, 3);
        let read = reload.0.read().unwrap().read.clone().unwrap();
        assert_eq!(read.len(), 3, "{:?}", read.keys());
        // A snapshot changed behind the reload's back stays put while
        // nothing it read moved: the refresh never evaluated.
        reload.0.write().unwrap().settings.concurrency = 7;
        reload.refresh(&path);
        assert_eq!(reload.current().concurrency, 7);
        std::fs::write(&shared, "concurrency = 5\n").unwrap();
        reload.refresh(&path);
        assert!(reload.valid());
        assert_eq!(reload.current().concurrency, 5);
        std::fs::write(&shared, "concurrency = 0\n").unwrap();
        reload.refresh(&path);
        assert!(!reload.valid());
        assert_eq!(reload.current().concurrency, 5);
        // An unchanged invalid file stays invalid without evaluating.
        reload.refresh(&path);
        assert!(!reload.valid());
    }
}
