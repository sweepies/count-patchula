//! `apm.lock.yaml` through `apm lock`, the lock-only mode of APM's CLI. It
//! resolves and downloads packages into `apm_modules/` to hash them, but deploys
//! nothing to agent targets and runs no package content; only the lock is kept.
use super::*;

impl Runner {
    pub(super) async fn apm_lock(&self, context: node::NodeContext<'_>) -> Result<(), String> {
        let node::NodeContext {
            driver,
            root,
            baseline,
            directory,
            versions,
            maintenance,
            ..
        } = context;
        let lock = node::join(directory, "apm.lock.yaml");
        let mut args = vec!["lock".to_string()];
        if maintenance {
            args.push("--update".into());
        }
        self.tool(
            driver,
            Invocation {
                tools: vec![("apm".into(), node::version(versions, "apm")?)],
                bin: "apm".into(),
                cwd: Some(root.join(directory)),
                // APM clones each package from its own local bare cache.
                env: vec![("GIT_ALLOW_PROTOCOL".into(), "https:file".into())],
                args,
            },
        )
        .await?;
        let before = baseline.get(&lock).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&lock)).ok();
        if !maintenance && before == after.as_deref() {
            return Err(format!(
                "apm.lock.yaml did not change after the manifest edit in {}",
                if directory.is_empty() {
                    "the repository root"
                } else {
                    directory
                }
            ));
        }
        Ok(())
    }
}
