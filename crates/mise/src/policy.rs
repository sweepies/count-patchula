//! Which tool names the Count passes to mise.
use crate::Error;

/// Backends that list versions from a registry or an API. `mise backends ls`
/// names these and the asdf and vfox plugin backends.
const BACKENDS: &[&str] = &[
    "aqua", "cargo", "conda", "core", "dotnet", "forgejo", "gem", "github", "gitlab", "go", "http",
    "npm", "packslip", "pypi", "s3", "spinel", "spm", "ubi",
];
/// Backends that clone a plugin and run its scripts.
const PLUGINS: &[&str] = &["asdf", "vfox"];

/// Names the Count lets through to `mise ls-remote`: a registry shorthand or
/// `backend:tool` for a backend that only reads release metadata. mise would
/// otherwise take these arguments as flags, bracketed tool options (which can
/// redirect a backend to another host, token included) or a `tool@version`.
pub fn check(tool: &str) -> Result<(), Error> {
    let refuse = |why: &str| {
        Err(Error::Refused(format!(
            "tool `{tool}` is not looked up: {why}"
        )))
    };
    if tool.is_empty() || tool.len() > 256 {
        return refuse("the name is empty or too long");
    }
    // The tool name follows the backend and is handed to a command such as
    // `npm view` or `go list`, which would read a leading `-` as a flag.
    let name = tool.split_once(':').map_or(tool, |(_, name)| name);
    if tool.starts_with('-') || name.starts_with('-') {
        return refuse("the name starts with `-`");
    }
    if let Some(bad) = tool
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "._:/@+~%-".contains(*c)))
    {
        return refuse(&format!(
            "the character `{}` is not allowed; tool options in brackets are not read",
            bad.escape_default()
        ));
    }
    if let Some((backend, _)) = tool.split_once(':') {
        if PLUGINS.contains(&backend) {
            return refuse(&format!(
                "the {backend} backend runs plugin code, which the Count never runs"
            ));
        }
        if !BACKENDS.contains(&backend) {
            return refuse(&format!("`{backend}` is not a backend the Count reads"));
        }
    }
    Ok(())
}
