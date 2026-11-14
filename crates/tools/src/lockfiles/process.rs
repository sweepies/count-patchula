use super::Runner;
use patchula_process::CommandSpec;
use std::{path::Path, time::Duration};

/// How long one tool run may take once its tools are installed.
const TOOL_TIMEOUT: Duration = Duration::from_secs(180);
/// The bytes of a run's output the executor keeps.
pub(super) const OUTPUT_LIMIT: usize = 4096;

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
            secrets: Vec::new(),
            timeout: TOOL_TIMEOUT,
            output_limit: OUTPUT_LIMIT,
            complete_output: false,
        }
    }
}
