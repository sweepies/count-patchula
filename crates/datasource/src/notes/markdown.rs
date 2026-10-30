//! Keep upstream text from mentioning anyone or linking back to an issue.
use std::{collections::BTreeSet, sync::LazyLock};

/// Follows `@` and `#` so GitHub no longer reads a mention or an issue
/// reference there. Unlike `&#64;`, which GitHub still renders as a mention,
/// it is invisible and survives in the stored text.
const JOINER: char = '\u{200d}';

/// Plain upstream text (tool output, registry errors, advisory summaries,
/// titles) made harmless in GitHub Markdown: `@name` mentions and `#123`
/// references get a joiner after the sigil, and links to GitHub issues, pull
/// requests, discussions and commits go through `redirect.github.com`, which
/// leaves no backlink. Prose that keeps its Markdown goes through `sanitise`.
pub fn inert(text: &str) -> String {
    static REFERENCES: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)https?://github\.com/[^/\s]+/[^/\s]+/(?:issues|pull|discussions|commit|commits)/[^\s<>()\[\]"']+"#,
        )
        .expect("GitHub reference URLs")
    });
    // `&#64;` renders as a mention too, so it is read as the `@` it stands for.
    static AT_ENTITY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)&#(?:0*64|x0*40);").expect("@ entity"));
    let text = AT_ENTITY.replace_all(text, "@");
    let text = REFERENCES.replace_all(&text, |c: &regex::Captures| without_backlink(&c[0]));
    let mut output = String::with_capacity(text.len());
    let mut previous = None::<char>;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        output.push(ch);
        let next = chars.peek().copied();
        let mention = ch == '@'
            && next.is_some_and(char::is_alphanumeric)
            && previous.is_none_or(|p| !p.is_alphanumeric() && p != '_');
        let reference =
            ch == '#' && next.is_some_and(|n| n.is_ascii_digit()) && previous != Some('&');
        if mention || reference {
            output.push(JOINER);
        }
        previous = Some(ch);
    }
    output
}

/// Only upstream prose passes through this function. Generated tables, package
/// names, source links and ownership markers must never be mention-neutralised.
/// Where references in upstream text point: a GitHub repository (through
/// `redirect.github.com`, avoiding backlinks) or another forge.
#[derive(Clone, Copy)]
pub enum Links<'a> {
    Github(&'a str),
    Forge(&'a super::source::Forge),
}
impl Links<'_> {
    fn issue(&self, owner: Option<&str>, number: &str) -> Option<String> {
        match self {
            Self::Github(repo) => {
                let owner = owner.unwrap_or(repo);
                (!owner.is_empty()).then(|| {
                    format!(
                        "[{owner}#{number}](https://redirect.github.com/{owner}/issues/{number})"
                    )
                })
            }
            Self::Forge(forge) => {
                let path = owner.unwrap_or(&forge.path);
                let route = match forge.kind {
                    super::source::ForgeKind::Gitlab => "-/issues",
                    super::source::ForgeKind::Gitea => "issues",
                };
                Some(format!(
                    "[{path}#{number}](https://{}/{path}/{route}/{number})",
                    forge.host
                ))
            }
        }
    }
}

pub fn sanitise(text: &str, links: Links<'_>) -> String {
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
                    (Some(owner.as_str()), &c["number"])
                } else {
                    (None, &c["issue"][1..])
                };
                match links.issue(owner, number) {
                    Some(link) => output.push_str(&link),
                    None => output.push_str(token),
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
                    output.push('@');
                    output.push(JOINER);
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
                        return Some(&text[..=i]);
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

/// An opening or closing code fence: its character and length.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_start_matches(' ');
    let ch = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let count = line.chars().take_while(|c| *c == ch).count();
    (count >= 3).then(|| (ch, count, &line[count..]))
}

/// An ATX heading's level, if the line is one.
fn heading(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = line[indent..].trim_end_matches(['\n', '\r']);
    let level = rest.bytes().take_while(|b| *b == b'#').count();
    (indent <= 3
        && (1..=6).contains(&level)
        && (rest.len() == level || rest[level..].starts_with([' ', '\t'])))
    .then_some(level)
}

fn list_item(line: &str) -> bool {
    static MARKER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^ {0,1}(?:[-+*]|[0-9]{1,9}[.)])[ \t]").expect("list marker")
    });
    MARKER.is_match(line)
}

/// Lines with the fence state in force at their start.
fn lines_with_fences(text: &str) -> Vec<(usize, &str, bool)> {
    let mut open = None::<(char, usize)>;
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let in_fence = open.is_some();
        lines.push((offset, line, in_fence));
        match (open, fence(line.trim_end_matches('\n'))) {
            (None, Some((ch, count, _))) => open = Some((ch, count)),
            (Some((kind, size)), Some((ch, count, rest)))
                if kind == ch && count >= size && rest.trim().is_empty() =>
            {
                open = None;
            }
            _ => {}
        }
        offset += line.len();
    }
    lines
}

/// Shifts upstream headings down so the shallowest sits at `top`, under the
/// Count's own package and version headings. Headings in code stay as they are.
pub(super) fn demote_headings(text: &str, top: usize) -> String {
    let lines = lines_with_fences(text);
    let shallowest = lines
        .iter()
        .filter(|(_, _, fenced)| !fenced)
        .filter_map(|(_, line, _)| heading(line))
        .min();
    let Some(shift) = shallowest
        .map(|level| top.saturating_sub(level))
        .filter(|s| *s > 0)
    else {
        return text.into();
    };
    let mut output = String::with_capacity(text.len() + lines.len() * shift);
    for (_, line, fenced) in lines {
        match heading(line).filter(|_| !fenced) {
            Some(level) => {
                let indent = line.len() - line.trim_start_matches(' ').len();
                output.push_str(&line[..indent]);
                output.push_str(&"#".repeat((level + shift).min(6)));
                output.push_str(&line[indent + level..]);
            }
            None => output.push_str(line),
        }
    }
    output
}

/// Closes what the end of `text` leaves open: a code fence, then every
/// `<details>`, so a cut can't swallow what follows it.
pub(super) fn close_open_blocks(text: &str) -> String {
    let mut output = text.trim_end().to_owned();
    let mut open = None::<(char, usize)>;
    let mut details = 0_i32;
    for line in output.lines() {
        match (open, fence(line)) {
            (None, Some((ch, count, _))) => open = Some((ch, count)),
            (Some((kind, size)), Some((ch, count, rest)))
                if kind == ch && count >= size && rest.trim().is_empty() =>
            {
                open = None;
            }
            (None, _) => {
                let lower = line.to_ascii_lowercase();
                details += lower.matches("<details").count() as i32;
                details -= lower.matches("</details>").count() as i32;
            }
            _ => {}
        }
    }
    if let Some((ch, count)) = open {
        output.push('\n');
        output.push_str(&ch.to_string().repeat(count));
    }
    for _ in 0..details.max(0) {
        output.push_str("\n</details>");
    }
    output.push('\n');
    output
}

/// The longest prefix of `text`, at most `limit` bytes, that ends where a
/// block does: never inside a paragraph (and so never inside a link or code
/// span), a code fence or after a heading with nothing under it. Anything open
/// at the end is closed. The flag says whether all of `text` fit.
pub fn fit(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (close_open_blocks(text), true);
    }
    let lines = lines_with_fences(text);
    let mut cut = 0;
    let mut previous_heading = false;
    let mut previous_blank = true;
    for (offset, line, fenced) in &lines {
        if *offset > limit {
            break;
        }
        let blank = line.trim().is_empty();
        let starts_block = !fenced
            && !blank
            && (previous_blank
                || heading(line).is_some()
                || list_item(line)
                || fence(line).is_some());
        if starts_block && !previous_heading {
            cut = *offset;
        }
        if !blank {
            previous_heading = !fenced && heading(line).is_some();
        }
        previous_blank = blank && !fenced;
    }
    if cut == 0 {
        // One block bigger than the room: settle for the last line end that
        // leaves no code span or link label open.
        for (offset, line, fenced) in lines.iter().rev() {
            let end = offset + line.len();
            if end > limit || *fenced || line.trim().is_empty() {
                continue;
            }
            let prefix = &text[..end];
            if prefix.matches('`').count().is_multiple_of(2)
                && prefix.matches('[').count() == prefix.matches(']').count()
                && heading(line).is_none()
            {
                cut = end;
                break;
            }
        }
    }
    (close_open_blocks(&text[..cut]), false)
}
