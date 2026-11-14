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
/// values that TOML would not read as anything but a string. No key or
/// string value starts with `-`, and no `@` appears, so nothing reads as a
/// flag or a version.
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
    // mise reads options as TOML when they parse as TOML, and as legacy
    // pairs otherwise, so the canonical form is tried first.
    if canonical(options).is_some() {
        return Ok(());
    }
    match legacy(options) {
        Some(_) => Ok(()),
        None => Err(
            "its options are not written in a form the Count passes on: \
             `key=\"value\"` pairs joined by `,` with no spaces, or plain `key=value` \
             pairs whose values are not numbers, booleans or dates"
                .into(),
        ),
    }
}

/// The options as TOML, when they are written exactly as `render_table`
/// writes them.
fn canonical(options: &str) -> Option<toml_edit::InlineTable> {
    let parsed = format!("_x_ = {{ {options} }}")
        .parse::<toml_edit::DocumentMut>()
        .ok()?
        .get("_x_")?
        .as_inline_table()?
        .clone();
    (render_table(&parsed, false)? == options).then_some(parsed)
}

/// mise's legacy option form: `key=value` pairs joined by `,`, a segment
/// without `=` continuing the previous value as mise reads it. Every value
/// is a string to mise; one TOML would read otherwise (`1e5`, `0x10`, `1_0`,
/// `true`, a date) is refused, so the two readings never disagree.
fn legacy(options: &str) -> Option<toml_edit::InlineTable> {
    let plain = |value: &str| {
        !value.is_empty()
            && !value.starts_with('-')
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._/:+~%*-".contains(c))
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for segment in options.split(',') {
        match (segment.split_once('='), pairs.last_mut()) {
            (Some((key, value)), _) if key_ok(key) && plain(value) => {
                pairs.push((key.into(), value.into()));
            }
            (None, Some((_, value))) if plain(segment) => {
                value.push(',');
                value.push_str(segment);
            }
            _ => return None,
        }
    }
    let mut table = toml_edit::InlineTable::new();
    for (key, value) in pairs {
        let not_a_string = format!("x = {value}")
            .parse::<toml_edit::DocumentMut>()
            .ok()
            .and_then(|doc| doc.get("x").map(|item| !item.is_str()))
            .unwrap_or(false);
        if not_a_string {
            return None;
        }
        table.insert(&key, value.into());
    }
    Some(table)
}

/// The options `tool` carries in brackets, as mise reads them. `None` when
/// it carries none or `check` would refuse them.
pub fn options(tool: &str) -> Option<toml_edit::InlineTable> {
    let options = split(tool).1?;
    check(tool).ok()?;
    canonical(options).or_else(|| legacy(options))
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
        // A string with a `"` is a literal string, which has no escapes and
        // which mise's bracket splitter reads as quoted, as it does `"…"`.
        Value::String(s) => {
            let s = s.value();
            if s.starts_with('-') || s.contains('\\') || s.chars().any(char::is_control) {
                return None;
            }
            match (s.contains('"'), s.contains('\'')) {
                (false, _) => format!("\"{s}\""),
                (true, false) => format!("'{s}'"),
                (true, true) => return None,
            }
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
             a key that is not a plain name, or a string with both kinds of quote, a \
             backslash, a control character or a leading `-`",
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
            (
                r#"url = 'https://x/{{ os(macos="darwin") }}'"#,
                r#"rust[url='https://x/{{ os(macos="darwin") }}']"#,
            ),
        ] {
            assert_eq!(bracketed("rust", &entry(text)).unwrap(), tool, "{text}");
        }
    }

    #[test]
    fn options_the_count_cannot_pass_are_refused() {
        for text in [
            r#"bin = "-x""#,
            r#"bin = "a\"b'c""#,
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
            // Legacy values TOML would read as something other than a string.
            "github:o/r[n=1e5]",
            "github:o/r[n=0x10]",
            "github:o/r[n=1_0]",
            "github:o/r[bin=x,n=1]",
            "github:o/r[bin=x,flag=true]",
            "github:o/r[when=1979-05-27]",
            "github:o/r[n=inf]",
            "github:o/r[bin=x,n=1e5]",
        ] {
            assert!(check(tool).is_err(), "{tool}");
        }
        let long = format!("github:o/r[bin={}]", "x".repeat(OPTIONS_LIMIT));
        assert!(check(&long).is_err());
    }

    #[test]
    fn bracketed_options_read_back_as_mise_reads_them() {
        let read = |tool: &str| options(tool).map(|table| render_table(&table, true).unwrap());
        assert_eq!(
            read("aqua:org/tool[components=clippy,rustfmt,bin=x]").as_deref(),
            Some(r#"bin="x",components="clippy,rustfmt""#)
        );
        assert_eq!(
            read(r#"s3:tool[platforms={linux-x64={version_list_url="s3://b/v"}}]"#).as_deref(),
            Some(r#"platforms={linux-x64={version_list_url="s3://b/v"}}"#)
        );
        assert_eq!(read("rust"), None);
        assert_eq!(read("github:o/r[n=1e5]"), None);
    }
}
