//! `.terraform.lock.hcl` through `terraform providers lock`, which records
//! the provider checksums for every platform.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[],
    tools: &["terraform"],
    versions: true,
    install: Install::Plain,
    proof: Proof::None,
    run: Run::Direct,
    refresh: Refresh::Locks(&[Manager::Terraform]),
    output: Output::Default,
};

impl Runner {
    /// Regenerate the directory's provider lockfile.
    pub(super) async fn terraform_lock(&self, context: context::Context<'_>) -> Result<(), Error> {
        let context::Context {
            driver,
            root,
            directory,
            versions,
            ..
        } = context;
        let cwd = root.join(directory);
        if cwd.join(".terraform.lock.hcl").exists() {
            std::fs::remove_file(cwd.join(".terraform.lock.hcl")).map_err(|e| e.to_string())?;
        }
        let (env, entries) = self.terraform_env();
        self.held_tool(
            driver,
            Invocation {
                descriptor: &DESCRIPTOR,
                tools: vec![("terraform".into(), context::version(versions, "terraform")?)],
                bin: caches::Bin::Terraform,
                cwd: None,
                env,
                credentials: credentials::scopes(entries.iter().copied()),
                secrets: credentials::secrets(entries.iter().copied()),
                public: false,
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
