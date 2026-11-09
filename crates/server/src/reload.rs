//! The live settings snapshot: invalid edits keep the last good settings.
use patchula_config::settings::Settings;
use std::{
    path::Path,
    sync::{Arc, RwLock},
};

/// Invalid edits leave the active snapshot usable but readiness reports the error.
#[derive(Clone)]
pub struct Reload(Arc<RwLock<Snapshot>>);
struct Snapshot {
    settings: Settings,
    valid: bool,
}
impl Reload {
    pub fn new(settings: Settings) -> Self {
        Self(Arc::new(RwLock::new(Snapshot {
            settings,
            valid: true,
        })))
    }
    pub fn current(&self) -> Settings {
        self.0.read().expect("settings lock").settings.clone()
    }
    pub fn valid(&self) -> bool {
        self.0.read().expect("settings lock").valid
    }
    pub fn refresh(&self, path: &Path) {
        // The engine-side rules (registry prefixes, mirrors, host
        // concurrency) reject a file the same way load's own validation does.
        let result = Settings::load(path).and_then(|settings| {
            patchula_engine::settings::check(&settings)
                .map(|_| settings)
                .map_err(|(_, message)| miette::miette!("{message}"))
        });
        let mut state = self.0.write().expect("settings lock");
        match result {
            Ok(settings) => {
                if !state.valid || settings != state.settings {
                    tracing::info!(target: "patchula_server", "settings loaded");
                }
                state.settings = settings;
                state.valid = true;
            }
            Err(_) => {
                if state.valid {
                    tracing::error!(target: "patchula_server", "settings rejected; keeping last good settings; run config check-settings for diagnostics");
                }
                state.valid = false;
            }
        }
    }
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
}
