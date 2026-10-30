//! Text from outside the Count on its way into GitHub Markdown: tool output,
//! registry errors, advisory summaries. GitHub turns `@name` into a mention and
//! `#123` into a backlink, so everything here ends up inert, in a code span, or
//! in a code block.
use chrono::{DateTime, Utc};
pub use patchula_datasource::notes::inert;

/// Failure text kept on a dashboard: a one-line summary, and the decisive
/// lines of the tool's output for a collapsed block.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub summary: String,
    pub detail: Option<String>,
}

const DETAIL_LINES: usize = 10;
const DETAIL_CHARS: usize = 1000;
const LINE_CHARS: usize = 300;
const SUMMARY_CHARS: usize = 240;

/// Removes terminal escape sequences: colours, cursor movement, links.
pub fn strip_ansi(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for ch in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(ch) = chars.next() {
                    if ch == '\u{7}' || (ch == '\u{1b}' && chars.next() == Some('\\')) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    output
}

/// A line of a progress bar or spinner, which says nothing once it is over.
fn progress(line: &str) -> bool {
    line.chars().any(|ch| {
        ('\u{2580}'..='\u{259f}').contains(&ch) || ('\u{2800}'..='\u{28ff}').contains(&ch)
    })
}

enum Line<'a> {
    /// Output of the command itself.
    Command(&'a str),
    /// A tool being installed first; mise prefixes each of its lines.
    Install(&'a str),
    Noise,
}
fn classify(line: &str) -> Line<'_> {
    let line = line.trim();
    if progress(line) || line.starts_with("info:") || line.starts_with("mise by ") {
        return Line::Noise;
    }
    if let Some(rest) = line.strip_prefix("mise ")
        && let Some((tool, rest)) = rest.split_once(' ')
        && (tool.contains('@') || tool == "✓" || tool == "✗")
    {
        let rest = rest.trim_start();
        // The tool name repeats once the install finishes.
        let rest = rest
            .strip_prefix(tool)
            .map_or(rest, |after| after.trim_start());
        return if rest.starts_with("info:") || rest.is_empty() {
            Line::Noise
        } else {
            Line::Install(rest)
        };
    }
    Line::Command(line)
}
fn decisive(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    ["error", "fatal", "npm err", "panic", "traceback", "failed"]
        .iter()
        .any(|start| line.starts_with(start))
}
fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.into();
    }
    let mut cut: String = text.chars().take(limit).collect();
    cut.push('…');
    cut
}

/// The decisive lines of a command's output: from its first error line on,
/// without installer chatter and progress bars, and capped.
fn excerpt(output: &str) -> Option<String> {
    let mut command = Vec::new();
    let mut install = Vec::new();
    for line in output.split(['\n', '\r']) {
        match classify(line) {
            Line::Command(line) if !line.is_empty() => command.push(line),
            Line::Install(line) => install.push(line),
            _ => {}
        }
    }
    let lines = if command.is_empty() { install } else { command };
    let start = lines.iter().position(|line| decisive(line)).unwrap_or(0);
    let mut text = String::new();
    for line in lines.iter().skip(start).take(DETAIL_LINES) {
        let mut line = cut(line, LINE_CHARS);
        // A fence in the output must not close the one around it.
        if line.starts_with("```") || line.starts_with("~~~") {
            line.insert(0, '\u{200b}');
        }
        text.push_str(&line);
        text.push('\n');
    }
    let text = text.trim_end();
    if text.is_empty() {
        return None;
    }
    Some(inert(&cut(text, DETAIL_CHARS)))
}

/// Splits a failure reason, such as a lockfile tool's whole output, into a
/// summary and the lines that matter. The report keeps the full reason; this
/// is what GitHub gets.
pub fn failure(reason: &str) -> Failure {
    let clean = strip_ansi(reason);
    let clean = clean.trim();
    if !clean.contains(['\n', '\r']) && clean.chars().count() <= SUMMARY_CHARS {
        return Failure {
            summary: inert(clean),
            detail: None,
        };
    }
    // "<what failed>: exit status: N: <stdout> <stderr>"
    if let Some(at) = clean.find("status: ") {
        let digits = clean[at + 8..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let end = at + 8 + digits;
        let head = &clean[..end];
        if digits > 0 && !head.contains(['\n', '\r']) && head.chars().count() <= SUMMARY_CHARS {
            return Failure {
                summary: inert(&head.replace("exit exit ", "exit ")),
                detail: excerpt(clean[end..].trim_start_matches(':')),
            };
        }
    }
    let first = clean.split(['\n', '\r']).next().unwrap_or("").trim();
    Failure {
        summary: inert(&cut(first, SUMMARY_CHARS)),
        detail: excerpt(clean),
    }
}

/// An inline code span that nothing inside can end early. Code never mentions
/// anyone, so the text stays as it is.
pub fn code(text: &str) -> String {
    let text = text
        .replace(['\n', '\r'], " ")
        .replace("<!--", "<\u{200b}!--")
        .replace("-->", "--\u{200b}>");
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// A fenced block, indented for a list item.
pub fn block(text: &str, indent: &str) -> String {
    let mut output = format!("{indent}```text\n");
    for line in text.lines() {
        output.push_str(&format!("{indent}{line}\n"));
    }
    output.push_str(&format!("{indent}```\n"));
    output
}

/// "31 Oct 08:29 UTC" from an RFC 3339 time, to the minute.
pub fn when(time: &str) -> String {
    DateTime::parse_from_rfc3339(time).map_or_else(
        |_| inert(time),
        |time| {
            time.with_timezone(&Utc)
                .format("%-d %b %H:%M UTC")
                .to_string()
        },
    )
}
/// The same for Unix seconds.
pub fn when_unix(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0).map_or_else(
        || seconds.to_string(),
        |time| time.format("%-d %b %Y %H:%M UTC").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = "lockfile generation failed: cargo: exit exit status: 101:  mise by @jdx – installing 1 tool\n\
mise rust@1.99.0 info: downloading installer\n\
mise rust@1.99.0 error: $HOME differs from euid-obtained home directory: you may be using sudo\n\
mise rust@1.99.0 Rust is installed now. Great!\n\
mise ███████████░░░░░ 0/1 · 3.0s   rust@1.99.0  Downloading rustup-init  3.0s  30/30 kB · 10 kB/s\r\
mise ███████████░░░░░ 0/1 · 6.0s   rust@1.99.0  Downloading rustup-init  6.0s  30/30 kB · 5 kB/s\n\
mise ✓ rust@1.99.0  13.2s\n\
mise ████████████████ 1/1 · installed 1 tool in 13.2s\n\
    Updating crates.io index\n\
\u{1b}[1m\u{1b}[31merror\u{1b}[0m: failed to select a version for the requirement `base64 = \"^0.22\"`\n\
candidate versions found which didn't match: 0.23.1\n\
location searched: crates.io index\n\
required by package `jsonwebtoken v10.4.0`\n\
    ... which satisfies dependency `jsonwebtoken = \"^10\"` of package `patchula-github v0.6.0`\n";

    #[test]
    fn a_lockfile_failure_keeps_the_error_and_its_chain_only() {
        let failure = failure(CARGO);
        assert_eq!(
            failure.summary,
            "lockfile generation failed: cargo: exit status: 101"
        );
        let detail = failure.detail.unwrap();
        assert!(
            detail.starts_with("error: failed to select a version for the requirement"),
            "{detail}"
        );
        assert!(detail.contains("required by package `jsonwebtoken v10.4.0`"));
        for noise in ["installer", "██", "rustup", "Updating", "@jdx", "\u{1b}"] {
            assert!(!detail.contains(noise), "{noise}: {detail}");
        }
        assert!(detail.lines().count() <= DETAIL_LINES);
        assert!(detail.chars().count() <= DETAIL_CHARS + 1);
    }

    #[test]
    fn a_short_one_line_reason_stays_as_it_is() {
        assert_eq!(
            failure("lockfile generation failed: mise lock did not resolve node to 26.13.0"),
            Failure {
                summary: "lockfile generation failed: mise lock did not resolve node to 26.13.0"
                    .into(),
                detail: None
            }
        );
    }

    #[test]
    fn the_detail_is_capped_and_cannot_close_its_fence() {
        let long = format!(
            "tool: exit status: 1: error: first\n```\n{}",
            "x".repeat(5000)
        );
        let failure = failure(&long);
        let detail = failure.detail.unwrap();
        assert!(
            detail.chars().count() <= DETAIL_CHARS + 1,
            "{}",
            detail.len()
        );
        assert!(detail.lines().all(|l| !l.starts_with("```")), "{detail}");
    }

    #[test]
    fn mentions_in_tool_output_are_neutralised_in_summary_and_detail() {
        let failure =
            failure("tool: exit status: 2: error: ask @jdx or @org/team about #12\nsecond line");
        let all = format!("{} {}", failure.summary, failure.detail.unwrap());
        assert!(
            !all.contains("@jdx") && !all.contains("@org") && !all.contains("#12"),
            "{all}"
        );
    }

    #[test]
    fn times_read_to_the_minute() {
        assert_eq!(when("2026-10-31T08:29:16.914Z"), "31 Oct 08:29 UTC");
        assert_eq!(when("not a time"), "not a time");
        assert_eq!(when_unix(123), "1 Jan 1970 00:02 UTC");
    }

    #[test]
    fn code_spans_survive_backticks_and_forged_markers() {
        assert_eq!(code("a`b"), "``a`b``");
        assert!(!code("<!-- patchula-action:x -->").contains("<!--"));
    }
}
