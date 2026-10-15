//! Preserve upstream Markdown while preventing mention and backlink notifications.
use std::{collections::BTreeSet, sync::LazyLock};

/// Only upstream prose passes through this function. Generated tables, package
/// names, source links and ownership markers must never be mention-neutralised.
pub fn sanitise(text: &str, repo: &str) -> String {
    static TOKENS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"\A(?:",
            r#"(?P<url>(?:[A-Za-z][A-Za-z0-9+.-]*://|www\.)[^\s<>\[\]()"']+)"#,
            r"|(?P<email>[A-Za-z0-9][A-Za-z0-9_.+-]*@[A-Za-z0-9.-]+)",
            r"|(?P<cross>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)#(?P<number>[0-9]+)",
            r"|(?P<issue>#[0-9]+)",
            r"|@(?P<mention>[A-Za-z0-9][A-Za-z0-9-]*(?:/[A-Za-z0-9][A-Za-z0-9_.-]*)?))"
        ))
        .expect("upstream Markdown tokens")
    });
    static DEFINITIONS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?m)^[ \t]{0,3}\[([^\]\n]+)\]:").expect("reference definitions")
    });
    static LIST_MARKER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(?:[-+*]|[0-9]{1,9}[.)])\s+").expect("Markdown list marker")
    });
    let references: BTreeSet<_> = DEFINITIONS
        .captures_iter(text)
        .map(|c| reference_label(&c[1]))
        .collect();
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    let mut fence = None;
    while offset < text.len() {
        let rest = &text[offset..];
        // Fences may use either character and any run of at least three. A
        // shorter run inside a fence is content, not its closing delimiter.
        if offset == 0 || text.as_bytes()[offset - 1] == b'\n' {
            let end = rest.find('\n').map_or(rest.len(), |n| n + 1);
            let line = &rest[..end];
            let mut trimmed = line.trim_start();
            while let Some(quoted) = trimmed.strip_prefix('>') {
                trimmed = quoted.trim_start();
            }
            let trimmed = LIST_MARKER
                .find(trimmed)
                .map_or(trimmed, |m| &trimmed[m.end()..]);
            let ch = trimmed.as_bytes().first().copied().unwrap_or_default();
            let count = trimmed.bytes().take_while(|b| *b == ch).count();
            if matches!(ch, b'`' | b'~') && count >= 3 {
                match fence {
                    None => fence = Some((ch, count)),
                    Some((kind, size))
                        if kind == ch && count >= size && trimmed[count..].trim().is_empty() =>
                    {
                        fence = None
                    }
                    _ => {}
                }
                output.push_str(line);
                offset += end;
                continue;
            }
            if fence.is_some() {
                output.push_str(line);
                offset += end;
                continue;
            }
        }
        if rest.starts_with('\\') {
            let end = rest.char_indices().nth(2).map_or(rest.len(), |(i, _)| i);
            output.push_str(&rest[..end]);
            offset += end;
            continue;
        }
        if rest.starts_with('`') {
            let count = rest.bytes().take_while(|b| *b == b'`').count();
            let delimiter = &rest[..count];
            let mut end = count;
            while let Some(next) = rest[end..].find(delimiter) {
                let start = end + next;
                end = start + count;
                if rest.as_bytes().get(end) != Some(&b'`') {
                    break;
                }
                end += rest[end..].bytes().take_while(|b| *b == b'`').count();
            }
            // An unmatched delimiter is literal Markdown; do not consume prose.
            output.push_str(&rest[..end]);
            offset += end;
            continue;
        }
        // Preserve labels (including nested/image/reference labels) verbatim.
        // Destinations and reference definitions are visited separately below.
        if rest.starts_with('[') {
            let mut depth = 0;
            let mut escaped = false;
            let mut end = None;
            let mut code_delimiter = 0;
            let mut backticks = 0;
            for (i, ch) in rest.char_indices() {
                if ch == '`' && !escaped {
                    backticks += 1;
                    continue;
                }
                if backticks > 0 {
                    if code_delimiter == 0 {
                        code_delimiter = backticks;
                    } else if code_delimiter == backticks {
                        code_delimiter = 0;
                    }
                    backticks = 0;
                }
                if code_delimiter > 0 {
                    continue;
                }
                if escaped {
                    escaped = false;
                    continue;
                }
                match ch {
                    '\\' => escaped = true,
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                let destination = inline_destination(&rest[end..]);
                let definition = rest[end..].starts_with(':')
                    && text[..offset]
                        .rsplit('\n')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .is_empty();
                if destination.is_some()
                    || definition
                    || rest[end..].starts_with('[')
                    || references.contains(&reference_label(&rest[1..end - 1]))
                {
                    output.push_str(&rest[..end]);
                    offset += end;
                    if let Some(destination) = destination {
                        output.push_str(&redirect_destination(destination));
                        offset += destination.len();
                    } else if definition {
                        let destination = rest[end..].split_inclusive('\n').next().unwrap();
                        output.push_str(&redirect_destination(destination));
                        offset += destination.len();
                    }
                    continue;
                }
            }
        }
        if let Some(c) = TOKENS.captures(rest) {
            let token = c.get(0).unwrap().as_str();
            let previous = text[..offset].chars().next_back();
            let boundary = previous.is_none_or(|ch| {
                ch.is_whitespace() || (!ch.is_alphanumeric() && !"_/@".contains(ch))
            });
            if c.name("url").is_some() {
                output.push_str(&without_backlink(token));
            } else if boundary && (c.name("cross").is_some() || c.name("issue").is_some()) {
                let (owner, number) = if let Some(owner) = c.name("cross") {
                    (owner.as_str(), &c["number"])
                } else {
                    (repo, &c["issue"][1..])
                };
                if owner.is_empty() {
                    output.push_str(token);
                } else {
                    output.push_str(&format!(
                        "[{owner}#{number}](https://redirect.github.com/{owner}/issues/{number})"
                    ));
                }
            } else if let Some(mention) = c.name("mention") {
                let following = &rest[token.len()..];
                let digest = mention.as_str().starts_with("sha") && following.starts_with(':');
                // GitHub's generated dependency-release prose can contain bare
                // scoped npm names, whose spelling also matches a team mention.
                let prefix = text[..offset]
                    .rsplit('\n')
                    .next()
                    .unwrap_or("")
                    .trim_start_matches([' ', '*', '-']);
                let package_context = mention.as_str().contains('/')
                    && ["Bump ", "Update ", "Upgrade ", "Install ", "Remove "]
                        .iter()
                        .any(|verb| prefix.starts_with(verb))
                    && !prefix.contains(" by ");
                // Emails, package@version, scoped packages with versions and
                // image digest algorithms are data, not GitHub mentions.
                let email_suffix = following.starts_with('.')
                    && following[1..].starts_with(|ch: char| ch.is_alphanumeric());
                if boundary
                    && !digest
                    && !package_context
                    && !email_suffix
                    && !following.starts_with(['@', '+'])
                {
                    output.push_str("@\u{200d}");
                    output.push_str(mention.as_str());
                } else {
                    output.push_str(token);
                }
            } else {
                output.push_str(token);
            }
            offset += token.len();
        } else {
            let ch = rest.chars().next().expect("nonempty text");
            output.push(ch);
            offset += ch.len_utf8();
        }
    }
    // Upstream text cannot forge the bot's ownership or control markers.
    output.replace("<!--", "&lt;!--").replace("-->", "--&gt;")
}

fn reference_label(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn inline_destination(text: &str) -> Option<&str> {
    if !text.starts_with('(') {
        return None;
    }
    let mut depth = 0;
    let mut escaped = false;
    let mut quote = None;
    for (i, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() {
            match ch {
                '\'' | '"' => quote = Some(ch),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[..i + 1]);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn redirect_destination(text: &str) -> String {
    // Keep anchors, titles, whitespace and Markdown delimiters byte-for-byte;
    // only the destination URL is eligible for a host replacement.
    let start = text[1..]
        .find(|ch: char| !ch.is_whitespace())
        .map_or(text.len(), |i| i + 1);
    let start = start + usize::from(text.as_bytes().get(start) == Some(&b'<'));
    let end = text[start..]
        .find(|ch: char| ch.is_whitespace() || ch == '>')
        .map_or(text.len() - usize::from(text.starts_with('(')), |i| {
            start + i
        });
    format!(
        "{}{}{}",
        &text[..start],
        without_backlink(&text[start..end]),
        &text[end..]
    )
}

fn without_backlink(url: &str) -> String {
    static REFERENCE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)^https?://github\.com/[^/\s]+/[^/\s]+/(?:issues|pull|discussions|commit|commits)/[^/\s]+",
        )
        .expect("GitHub reference URL")
    });
    if REFERENCE.is_match(url) {
        format!(
            "https://redirect.github.com/{}",
            url.splitn(4, '/').nth(3).unwrap()
        )
    } else {
        url.into()
    }
}
