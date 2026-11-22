//! `.terraform.lock.hcl` through `terraform providers lock`, which records
//! the provider checksums for every platform.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[],
    tools: &["terraform"],
    install: Install::Plain,
    proof: Proof::None,
    run: Run::Direct,
    output: Output::Default,
};

impl Runner {
    /// Regenerate the directory's provider lockfile.
    pub(super) async fn terraform_lock(
        &self,
        context: context::Context<'_>,
    ) -> Result<(), Failure> {
        let context::Context {
            driver,
            root,
            directory,
            versions,
            public,
            ..
        } = context;
        let cwd = root.join(directory);
        if cwd.join(".terraform.lock.hcl").exists() {
            std::fs::remove_file(cwd.join(".terraform.lock.hcl")).map_err(|e| e.to_string())?;
        }
        let (env, entries) = self.terraform_env();
        self.tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("terraform".into(), context::version(versions, "terraform")?)],
                bin: caches::Bin::Terraform,
                cwd: None,
                env,
                held: credentials::Held::of(entries.iter().copied()),
                refused: None,
                public,
                args: vec![
                    format!("-chdir={}", cwd.display()),
                    "providers".into(),
                    "lock".into(),
                ],
            },
        )
        .await?;
        Ok(())
    }
}
