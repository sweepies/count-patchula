//! Release archives: the binary, its man page, completions and license.

use crate::{Args, Result, output, run};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

/// Builds static release archives into target/dist. The first target must run
/// on this host, since its binary generates the man page and completions.
pub fn package(args: Args) -> Result {
    let targets = args.positional(1, usize::MAX)?;
    let metadata: Value = serde_json::from_str(&output(Command::new("cargo").args([
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
    ]))?)?;
    let version = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == "patchula-cli")
        .and_then(|p| p["version"].as_str())
        .ok_or("cargo metadata lists no patchula-cli version")?;
    let dist = Path::new("target/dist");
    fs::create_dir_all(dist)?;
    for (index, target) in targets.iter().enumerate() {
        let platform = match target.as_str() {
            "x86_64-unknown-linux-musl" => "linux-x64-musl",
            "aarch64-unknown-linux-musl" => "linux-arm64-musl",
            "x86_64-apple-darwin" => "darwin-x64",
            "aarch64-apple-darwin" => "darwin-arm64",
            _ => return Err(format!("no release platform for {target}").into()),
        };
        run(Command::new("rustup").args(["target", "add", target]))?;
        run(Command::new("cargo").args([
            "build",
            "--release",
            "--locked",
            "-p",
            "patchula-cli",
            "--target",
            target,
        ]))?;
        let bin = format!("target/{target}/release/patchula");
        if index == 0 {
            run(Command::new("mise")
                .args(["run", "docs:cli"])
                .env("PATCHULA_BIN", &bin))?;
            fs::copy(
                "generated/patchula.usage.kdl",
                dist.join("patchula.usage.kdl"),
            )?;
        }
        let name = format!("patchula-v{version}-{platform}");
        let stage = std::env::temp_dir().join(&name);
        if stage.exists() {
            fs::remove_dir_all(&stage)?;
        }
        for (from, to) in [
            (bin.as_str(), "bin/patchula"),
            ("generated/patchula.1", "share/man/man1/patchula.1"),
            (
                "generated/completions/patchula.bash",
                "share/bash-completion/completions/patchula",
            ),
            (
                "generated/completions/patchula.zsh",
                "share/zsh/site-functions/_patchula",
            ),
            (
                "generated/completions/patchula.fish",
                "share/fish/vendor_completions.d/patchula.fish",
            ),
            ("LICENSE", "LICENSE"),
            ("README.md", "README.md"),
        ] {
            let to = stage.join(to);
            fs::create_dir_all(to.parent().ok_or("staged file has no parent")?)?;
            fs::copy(from, &to).map_err(|e| format!("could not stage {from}: {e}"))?;
        }
        let archive = dist.join(format!("{name}.tar.gz"));
        run(Command::new("tar")
            .arg("-C")
            .arg(&stage)
            .arg("-czf")
            .arg(&archive)
            .arg("."))?;
        fs::remove_dir_all(&stage)?;
        println!("{}", archive.display());
    }
    Ok(())
}
