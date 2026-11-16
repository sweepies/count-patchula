//! NuGet `packages.lock.json` files, which the Count refuses to regenerate.
//!
//! The only tool that writes them is `dotnet restore`, and restore evaluates
//! MSBuild: the repository's project files, `Directory.Build.props` and
//! `.targets`, and the `build/` props and targets of every package it
//! restores. Evaluation runs property functions, inline tasks and
//! repository-chosen task assemblies, so no restore can run without code the
//! repository or a package chose. An update that needs the lock moved is
//! skipped with this reason instead.
use super::*;

pub(super) static DESCRIPTOR: Tool = Tool {
    steers: &[Manager::Nuget],
    tools: &[],
    versions: false,
    install: Install::Plain,
    proof: Proof::None,
    run: Run::Direct,
    output: Output::Default,
};

/// Why a `packages.lock.json` in `directory` is not regenerated.
pub(super) fn refused(directory: &str) -> Failure {
    Failure::Refused(format!(
        "packages.lock.json in {} is not regenerated: dotnet restore evaluates MSBuild projects and package targets, which runs code the repository and its packages chose",
        context::describe_dir(directory)
    ))
}
