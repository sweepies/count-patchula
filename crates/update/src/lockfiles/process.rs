use super::Runner;
use patchula_process::CommandSpec;
use std::{path::Path, time::Duration};

impl Runner {
    pub(super) fn command(&self, driver: &Path) -> CommandSpec {
        let home = driver.join("home");
        // The caller creates driver; failures creating HOME are reported by the
        // tool without falling back to the caller's home or credential files.
        let _ = std::fs::create_dir_all(&home);
        let mut env = patchula_mise::sandbox_env(&home, &self.mise_data);
        env.insert("MISE_AUTO_INSTALL".into(), "true".into());
        CommandSpec {
            program: self.mise.clone(),
            args: vec!["--no-config".into()],
            cwd: driver.into(),
            env,
            timeout: Duration::from_secs(180),
            output_limit: 4096,
            complete_output: false,
        }
    }
}
