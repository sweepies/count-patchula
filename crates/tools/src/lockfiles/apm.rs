//! `apm.lock.yaml` through `apm lock`, the lock-only mode of APM's CLI. It
//! resolves and downloads packages into `apm_modules/` to hash them, but deploys
//! nothing to agent targets and runs no package content; only the lock is kept.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[],
    tools: &["apm"],
    versions: true,
    install: Install::Plain,
    proof: Proof::None,
    run: Run::Direct,
    output: Output::Default,
};

impl Runner {
    pub(super) async fn apm_lock(&self, context: context::Context<'_>) -> Result<(), Failure> {
        let context::Context {
            driver,
            root,
            baseline,
            directory,
            versions,
            maintenance,
            ..
        } = context;
        let lock = context::join(directory, "apm.lock.yaml");
        let mut args = vec!["lock".to_string()];
        if maintenance {
            args.push("--update".into());
        }
        self.tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("apm".into(), context::version(versions, "apm")?)],
                bin: caches::Bin::Apm,
                cwd: Some(root.join(directory)),
                // APM clones each package from its own local bare cache.
                env: vec![("GIT_ALLOW_PROTOCOL".into(), "https:file".into())],
                credentials: Vec::new(),
                secrets: Vec::new(),
                public: false,
                args,
            },
        )
        .await?;
        let before = baseline.get(&lock).map(|b| b.content.as_str());
        let after = std::fs::read_to_string(root.join(&lock)).ok();
        if !maintenance && before == after.as_deref() {
            return Err(context::unchanged(
                "apm.lock.yaml",
                directory,
                context::Trigger::ManifestEdit,
            ));
        }
        Ok(())
    }
}
