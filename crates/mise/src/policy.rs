//! Which tool names the Count passes to mise.
use crate::Error;

/// Backends that clone a plugin and run its scripts.
const PLUGINS: &[&str] = &["asdf", "vfox"];

/// Names the Count lets through to `mise ls-remote`: a registry shorthand or
/// `backend:tool` for any backend mise has, apart from the plugin backends.
/// An unknown backend is left to mise, which fails the lookup without running
/// anything. mise would otherwise take these arguments as flags, bracketed tool
/// options (which can redirect a backend to another host, token included) or a
/// `tool@version`.
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
    // `@` opens an npm scope (`npm:@types/node`) and nowhere else: anywhere else
    // mise reads `tool@version` as a selector, and a bare `@scope/pkg` is not
    // a registry name, so mise tries it as a plugin.
    let scope = tool
        .split_once(':')
        .is_some_and(|(_, name)| name.starts_with('@'));
    if tool.matches('@').count() > usize::from(scope) {
        return refuse(
            "`@` is allowed only to open an npm scope right after the backend, as in `npm:@scope/pkg`; \
             a `tool@version` selector is not looked up",
        );
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
    if let Some((backend, _)) = tool.split_once(':')
        && PLUGINS.contains(&backend)
    {
        return refuse(&format!(
            "the {backend} backend runs plugin code, which the Count never runs"
        ));
    }
    Ok(())
}
