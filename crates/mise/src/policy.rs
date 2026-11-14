//! Which tool arguments the Count passes to mise, options included.
use crate::Error;
use toml_edit::{TableLike, Value};

/// Backends that clone a plugin and run its scripts: refused as tool
/// names here and disabled inside mise itself.
pub(crate) const PLUGIN_BACKENDS: &[&str] = &["asdf", "vfox"];

/// The longest options the Count passes in brackets.
const OPTIONS_LIMIT: usize = 1024;

/// A tool argument split at its bracketed options: `github:org/tool[bin=tool]`
/// is `github:org/tool` with `bin=tool`. The options are everything between
/// the first `[` and a closing `]` that ends the argument; `check` refuses an
/// argument whose brackets do not close there.
pub fn split(tool: &str) -> (&str, Option<&str>) {
    match tool.split_once('[') {
        Some((name, rest)) => (name, Some(rest.strip_suffix(']').unwrap_or(rest))),
        None => (tool, None),
    }
}

/// Whether `tool`'s options may name a host: any `:` in them, which every
/// absolute URL has. mise sends `MISE_GITHUB_TOKEN` to any host whose URL
/// looks like a GitHub API, so such a lookup is handed no token.
pub fn options_name_a_host(tool: &str) -> bool {
    split(tool).1.is_some_and(|options| options.contains(':'))
}

/// Tool arguments the Count lets through to `mise ls-remote`: a registry
/// shorthand or `backend:tool` for any backend mise has, apart from the plugin
/// backends, with options in brackets as mise reads them. An unknown backend
/// is left to mise, which fails the lookup without running anything. mise
/// would otherwise take these arguments as flags or a `tool@version`.
pub fn check(tool: &str) -> Result<(), Error> {
    let refuse = |why: &str| {
        Err(Error::Refused(format!(
            "tool `{tool}` is not looked up: {why}"
        )))
    };
    let (name, options) = split(tool);
    if name.is_empty() || name.len() > 256 {
        return refuse("the name is empty or too long");
    }
    // The tool name follows the backend and is handed to a command such as
    // `npm view` or `go list`, which would read a leading `-` as a flag.
    let bare = name.split_once(':').map_or(name, |(_, bare)| bare);
    if name.starts_with('-') || bare.starts_with('-') {
        return refuse("the name starts with `-`");
    }
    // `@` opens an npm scope (`npm:@types/node`) and nowhere else: anywhere else
    // mise reads `tool@version` as a selector, and a bare `@scope/pkg` is not
    // a registry name, so mise tries it as a plugin.
    let scope = bare.starts_with('@') && name.contains(':');
    if name.matches('@').count() > usize::from(scope) {
        return refuse(
            "`@` is allowed only to open an npm scope right after the backend, as in `npm:@scope/pkg`; \
             a `tool@version` selector is not looked up",
        );
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "._:/@+~%-".contains(*c)))
    {
        return refuse(&format!(
            "the character `{}` is not allowed in a tool name",
            bad.escape_default()
        ));
    }
    if let Some((backend, _)) = name.split_once(':')
        && PLUGIN_BACKENDS.contains(&backend)
    {
        return refuse(&format!(
            "the {backend} backend runs plugin code, which the Count never runs"
        ));
    }
    if let Some(options) = options
        && let Err(why) = check_options(tool, options)
    {
        return refuse(&why);
    }
    Ok(())
}

/// Bracketed options mise reads one way only: the canonical TOML form
/// `bracketed` writes, or mise's legacy `key=value,key=value` with plain
/// values. No key or string value starts with `-`, and no `@` appears, so
/// nothing reads as a flag or a version.
fn check_options(tool: &str, options: &str) -> Result<(), String> {
    if !tool.ends_with(']') {
        return Err("its options in brackets do not end the argument".into());
    }
    if options.is_empty() || options.len() > OPTIONS_LIMIT {
        return Err("its options are empty or too long".into());
    }
    if options.contains('@') {
        return Err("`@` is not allowed in its options".into());
    }
    if options.chars().any(char::is_control) {
        return Err("its options hold a control character".into());
    }
    if legacy(options) {
        return Ok(());
    }
    let parsed = format!("_x_ = {{ {options} }}")
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|doc| doc.get("_x_")?.as_inline_table().cloned())
        .ok_or("its options are neither TOML nor `key=value` pairs")?;
    match render_table(&parsed, false) {
        Some(rendered) if rendered == options => Ok(()),
        _ => Err(
            "its options are not written in the one form the Count passes on: \
             `key=\"value\"` pairs joined by `,`, with no spaces"
                .into(),
        ),
    }
}

/// mise's legacy option form: `key=value` pairs joined by `,`, a segment
/// without `=` continuing the previous value as mise reads it.
fn legacy(options: &str) -> bool {
    let plain = |value: &str| {
        !value.is_empty()
            && !value.starts_with('-')
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._/:+~%*-".contains(c))
    };
    let mut first = true;
    options.split(',').all(|segment| {
        let ok = match segment.split_once('=') {
            Some((key, value)) => key_ok(key) && plain(value),
            None => !first && plain(segment),
        };
        first = false;
        ok
    })
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('-')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
}

/// `value` in the canonical form: basic strings, integers, booleans, arrays
/// and inline tables, with no spaces. Anything else, or a string the form
/// cannot hold as written, is `None`.
fn render(value: &Value, sort: bool) -> Option<String> {
    Some(match value {
        Value::String(s) => {
            let s = s.value();
            if s.starts_with('-') || s.contains(['"', '\\']) || s.chars().any(char::is_control) {
                return None;
            }
            format!("\"{s}\"")
        }
        Value::Integer(i) => i.value().to_string(),
        Value::Boolean(b) => b.value().to_string(),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|v| render(v, sort))
                .collect::<Option<Vec<_>>>()?
                .join(",")
        ),
        Value::InlineTable(table) => format!("{{{}}}", render_table(table, sort)?),
        Value::Float(_) | Value::Datetime(_) => return None,
    })
}

/// A table's pairs as `key=value` joined by `,`, sorted by key when `sort`.
fn render_table(table: &dyn TableLike, sort: bool) -> Option<String> {
    let mut pairs = Vec::new();
    for (key, item) in table.iter() {
        if !key_ok(key) {
            return None;
        }
        pairs.push((key, render(item.as_value()?, sort)?));
    }
    if sort {
        pairs.sort();
    }
    Some(
        pairs
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// The tool argument for `name` with the options on its entry, every key
/// but `version`, in brackets as mise reads them: mise applies an entry's
/// options from its table and from brackets alike. Keys are sorted, so one
/// set of options is always written the same way. A name that already
/// carries brackets keeps them as written and must have no other options.
/// A refusal says why without repeating what the entry holds.
pub fn bracketed(name: &str, entry: &dyn TableLike) -> Result<String, Error> {
    let refuse = |why: &str| Err(Error::Refused(why.into()));
    let mut table = toml_edit::InlineTable::new();
    for (key, item) in entry.iter().filter(|(key, _)| *key != "version") {
        let Some(value) = item.as_value() else {
            return refuse("its entry has an option that is not a value the Count passes to mise");
        };
        table.insert(key, value.clone());
    }
    // A name the Count refuses is refused for itself when it is looked up.
    if table.is_empty() || check(name).is_err() {
        return Ok(name.to_owned());
    }
    if name.contains('[') {
        return refuse(
            "its entry sets options both in brackets on its name and in its table, \
             which the Count does not combine",
        );
    }
    let Some(rendered) = render_table(&table, true) else {
        return refuse(
            "its entry has an option the Count does not pass to mise: a float, a date, \
             a key that is not a plain name, or a string with a quote, backslash, \
             control character or leading `-`",
        );
    };
    let tool = format!("{name}[{rendered}]");
    if check(&tool).is_err() {
        return refuse(
            "its options are too long or hold an `@`, which the Count does not pass to mise",
        );
    }
    Ok(tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> toml_edit::Table {
        text.parse::<toml_edit::DocumentMut>()
            .unwrap()
            .as_table()
            .clone()
    }

    #[test]
    fn options_are_written_sorted_and_canonical() {
        for (text, tool) in [
            (
                r#"version = "1.100.0"
profile = "minimal"
components = "clippy,rustfmt"
mr_boxington = true"#,
                r#"rust[components="clippy,rustfmt",mr_boxington=true,profile="minimal"]"#,
            ),
            (
                r#"version = "1.7.1"
api_url = "https://codeberg.org/api/v1"
os = [ "linux", ]"#,
                r#"rust[api_url="https://codeberg.org/api/v1",os=["linux"]]"#,
            ),
            (
                r#"version = "1"
platforms = { linux-x64 = { url = "https://x/a" }, darwin = { url = "https://x/b", checksum = "c" } }"#,
                r#"rust[platforms={darwin={checksum="c",url="https://x/b"},linux-x64={url="https://x/a"}}]"#,
            ),
            (r#"version = "1""#, "rust"),
        ] {
            assert_eq!(bracketed("rust", &entry(text)).unwrap(), tool, "{text}");
        }
    }

    #[test]
    fn options_the_count_cannot_pass_are_refused() {
        for text in [
            r#"bin = "-x""#,
            r#"bin = "a\"b""#,
            r#"bin = 'a\b'"#,
            "bin = 1.5",
            "bin = 1979-05-27",
            r#""a key" = "x""#,
            r#"url = "https://user@host/""#,
            r#"bin = "a\nb""#,
        ] {
            assert!(
                matches!(bracketed("tool", &entry(text)), Err(Error::Refused(_))),
                "{text}"
            );
        }
        assert!(matches!(
            bracketed("github:o/r[bin=x]", &entry(r#"os = ["linux"]"#)),
            Err(Error::Refused(_))
        ));
        assert_eq!(
            bracketed("github:o/r[bin=x]", &entry(r#"version = "1""#)).unwrap(),
            "github:o/r[bin=x]"
        );
    }

    #[test]
    fn bracketed_names_pass_in_the_canonical_or_legacy_form() {
        for tool in [
            "github:github/gh-actions-lock[bin=gh-actions-lock]",
            "ubi:org/tool[exe=rg,matching=musl]",
            "aqua:org/tool[components=clippy,rustfmt]",
            r#"forgejo:o/r[api_url="https://codeberg.org/api/v1",os=["linux"]]"#,
            "github:o/r[api_url=https://ghe.example/api/v3]",
            "pypi:black[extras=jupyter]",
        ] {
            assert!(check(tool).is_ok(), "{tool}: {:?}", check(tool));
        }
        for tool in [
            "github:o/r[bin=x]@1",
            "github:o/r[bin=x",
            "github:o/r[]",
            "github:o/r[bin=-x]",
            "github:o/r[-bin=x]",
            "github:o/r[bin=x@1]",
            r#"github:o/r[bin = "x"]"#,
            r#"github:o/r[bin="x",]"#,
            r#"github:o/r[bin='x']"#,
            r#"github:o/r[bin="-x"]"#,
            "github:o/r[bin=x\ny]",
            "github:o/r[bin=x][os=linux]",
            "github:o/r[=x]",
            "github:o/r[,bin=x]",
            "asdf:o/r[bin=x]",
        ] {
            assert!(check(tool).is_err(), "{tool}");
        }
        let long = format!("github:o/r[bin={}]", "x".repeat(OPTIONS_LIMIT));
        assert!(check(&long).is_err());
    }

    #[test]
    fn only_options_with_a_colon_may_name_a_host() {
        assert!(options_name_a_host(
            r#"forgejo:o/r[api_url="https://codeberg.org/api/v1"]"#
        ));
        assert!(options_name_a_host("github:o/r[api_url=https:evil]"));
        assert!(!options_name_a_host("github:o/r[bin=x]"));
        assert!(!options_name_a_host("cargo:https://github.com/o/r"));
        assert!(!options_name_a_host(r#"rust[profile="minimal"]"#));
    }
}
