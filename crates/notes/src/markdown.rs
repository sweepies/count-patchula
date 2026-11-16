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
    // `&#64;`, `&#x40;` and `&commat;` render as mentions too, so each is read
    // as the `@` it stands for.
    static AT_ENTITY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)&(?:#(?:0*64|x0*40)|commat);").expect("@ entity"));
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
    Forge(&'a crate::Forge),
    /// Text from no repository, such as a chart's own list of changes. A bare
    /// `#123` names nothing GitHub should link, and `owner/repo#123` is a
    /// GitHub reference.
    None,
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
                    crate::ForgeKind::Gitlab => "-/issues",
                    crate::ForgeKind::Gitea => "issues",
                };
                Some(format!(
                    "[{path}#{number}](https://{}/{path}/{route}/{number})",
                    forge.host
                ))
            }
            Self::None => Some(match owner {
                Some(owner) => format!(
                    "[{owner}#{number}](https://redirect.github.com/{owner}/issues/{number})"
                ),
                None => format!("#\u{200D}{number}"),
            }),
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
    let (definitions, references) = definitions(text);
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    let mut fences = Fences::default();
    while offset < text.len() {
        let rest = &text[offset..];
        if offset == 0 || text.as_bytes()[offset - 1] == b'\n' {
            let end = rest.find('\n').map_or(rest.len(), |n| n + 1);
            let line = &rest[..end];
            if fences.advance(line) || fences.is_open() {
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
        // A link's label is kept verbatim: GitHub links nothing inside it.
        // Only a label GitHub really renders as a link (or a reference
        // definition) qualifies; any other bracketed text is prose, since
        // GitHub shows it literally, mentions and references included.
        if rest.starts_with('[')
            && let Some(end) = label_end(rest)
        {
            let after = &rest[end..];
            let destination = inline_destination(after);
            let definition = definitions.contains(&offset) && after.starts_with(':');
            if definition || is_link(&rest[1..end - 1], after, &references) {
                output.push_str(&rest[..end]);
                offset += end;
                if let Some(destination) = destination {
                    output.push_str(&redirect_destination(destination));
                    offset += destination.len();
                } else if definition {
                    let destination = after.split_inclusive('\n').next().unwrap();
                    output.push_str(&redirect_destination(destination));
                    offset += destination.len();
                }
                continue;
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

/// Link reference definitions GitHub honours: where each one's `[` sits, and
/// their normalised labels. Strict on purpose: a definition missed here only
/// leaves its links' labels neutralised, while one counted that GitHub does
/// not honour would leave a literal `[@name][label]` live. A definition sits
/// outside code, starts a block (after a blank line, a heading or another
/// definition, since it cannot interrupt a paragraph), and fits on one line:
/// `[label]: destination`, then optionally a closed title.
fn definitions(text: &str) -> (BTreeSet<usize>, BTreeSet<String>) {
    static DEFINITION: LazyLock<regex::Regex> = LazyLock::new(|| {
        // No backslash: an escaped `\]` would not close the label.
        regex::Regex::new(r"^ {0,3}\[([^\[\]\\\n]+)\]:").expect("reference definition")
    });
    let mut offsets = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut starts_block = true;
    for (offset, line, fenced) in lines_with_fences(text) {
        if fenced || Fences::opens(line) {
            starts_block = true;
            continue;
        }
        let definition = starts_block
            .then(|| DEFINITION.captures(line))
            .flatten()
            .filter(|c| definition_tail(&line[c.get(0).unwrap().end()..]))
            .map(|c| reference_label(&c[1]))
            .filter(|label| !label.is_empty());
        starts_block = match definition {
            Some(label) => {
                offsets.insert(offset + line.find('[').expect("a definition's label"));
                labels.insert(label);
                true
            }
            None => line.trim().is_empty() || heading(line).is_some(),
        };
    }
    (offsets, labels)
}

/// Whether the rest of a definition's line, after `[label]:`, is a
/// destination and an optional title, and nothing else.
fn definition_tail(text: &str) -> bool {
    let bytes = text.trim_end_matches(['\n', '\r']).as_bytes();
    let start = bytes
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    let Some(end) = destination_end(bytes, start).filter(|end| *end > start) else {
        return false;
    };
    let spaces = |at: usize| {
        at + bytes[at..]
            .iter()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count()
    };
    let mut rest = spaces(end);
    if rest > end
        && let Some(title) = title_end(bytes, rest)
    {
        rest = spaces(title);
    }
    rest == bytes.len()
}

/// Whether GitHub renders `[label]` followed by `after` as a link or image:
/// an inline destination, or a reference (full, collapsed or shortcut) to a
/// defined label. Links do not nest, so a label holding a link is not one.
fn is_link(label: &str, after: &str, references: &BTreeSet<String>) -> bool {
    if holds_link(label, references) {
        return false;
    }
    if inline_destination(after).is_some() {
        return true;
    }
    let defined = |label: &str| references.contains(&reference_label(label));
    match after.starts_with('[').then(|| label_end(after)).flatten() {
        // `[label][]`
        Some(2) => defined(label),
        // `[text][label]`; an undefined second label leaves both literal.
        Some(end) => defined(&after[1..end - 1]),
        // `[label]`
        None => defined(label),
    }
}

/// Whether link text holds something that may be a link: brackets followed
/// by `(` or `[`, or around a defined label. Images may sit in link text and
/// do not count. It errs towards yes, which only neutralises a label.
fn holds_link(text: &str, references: &BTreeSet<String>) -> bool {
    let mut open = Vec::new();
    let mut previous = None;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
                previous = None;
                continue;
            }
            '[' => open.push((i, previous == Some('!'))),
            ']' => {
                if let Some((start, image)) = open.pop()
                    && !image
                    && (text[i + 1..].starts_with(['(', '['])
                        || references.contains(&reference_label(&text[start + 1..i])))
                {
                    return true;
                }
            }
            _ => {}
        }
        previous = Some(ch);
    }
    false
}

/// The end of the bracketed label at the start of `text`, with nested
/// brackets balanced and code spans and escapes skipped.
fn label_end(text: &str) -> Option<usize> {
    let mut depth = 0;
    let mut escaped = false;
    let mut code_delimiter = 0;
    let mut backticks = 0;
    for (i, ch) in text.char_indices() {
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
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `(destination "title")` after a link label, when CommonMark reads it
/// as one: optional whitespace (at most one line ending), a destination in
/// `<…>` or one without spaces or control characters and with balanced
/// parentheses, an optional title after whitespace, optional whitespace,
/// then `)`. Anything else, such as `(see notes)`, leaves the label literal.
fn inline_destination(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let start = link_space(bytes, 1)?;
    let end = destination_end(bytes, start)?;
    let mut close = link_space(bytes, end)?;
    if close > end
        && let Some(title) = title_end(bytes, close)
    {
        close = link_space(bytes, title)?;
    }
    (bytes.get(close) == Some(&b')')).then(|| &text[..=close])
}

/// Whether a backslash at `at` escapes the ASCII punctuation after it.
fn escapes(bytes: &[u8], at: usize) -> bool {
    bytes[at] == b'\\' && bytes.get(at + 1).is_some_and(u8::is_ascii_punctuation)
}

/// Past spaces, tabs and at most one line ending; none where a blank line
/// would end the paragraph.
fn link_space(bytes: &[u8], mut at: usize) -> Option<usize> {
    let mut line_ending = false;
    loop {
        match bytes.get(at) {
            Some(b' ' | b'\t') => at += 1,
            Some(b'\n' | b'\r') => {
                if line_ending {
                    return None;
                }
                line_ending = true;
                at += if bytes[at..].starts_with(b"\r\n") {
                    2
                } else {
                    1
                };
            }
            _ => return Some(at),
        }
    }
}

/// The end of a link destination starting at `at`, which may be empty.
fn destination_end(bytes: &[u8], mut at: usize) -> Option<usize> {
    if bytes.get(at) == Some(&b'<') {
        at += 1;
        loop {
            match *bytes.get(at)? {
                _ if escapes(bytes, at) => at += 2,
                b'>' => return Some(at + 1),
                b'<' | b'\n' | b'\r' => return None,
                _ => at += 1,
            }
        }
    }
    let mut depth = 0_usize;
    while let Some(&b) = bytes.get(at) {
        match b {
            _ if escapes(bytes, at) => {
                at += 2;
                continue;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            _ if b <= b' ' || b == 0x7f => break,
            _ => {}
        }
        at += 1;
    }
    (depth == 0).then_some(at)
}

/// The end of a link title in `"…"`, `'…'` or `(…)` starting at `at`. A
/// title holds no blank line, and a parenthesised one no unescaped `(`.
fn title_end(bytes: &[u8], mut at: usize) -> Option<usize> {
    let close = match bytes.get(at)? {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    let parenthesised = close == b')';
    let mut blank = false;
    at += 1;
    loop {
        let b = *bytes.get(at)?;
        match b {
            _ if escapes(bytes, at) => {
                at += 2;
                blank = false;
                continue;
            }
            _ if b == close => return Some(at + 1),
            b'(' if parenthesised => return None,
            b'\n' if blank => return None,
            b'\n' => blank = true,
            b' ' | b'\t' | b'\r' => {}
            _ => blank = false,
        }
        at += 1;
    }
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

/// Which lines are code: the one place a document's fences are tracked, so
/// every reader of release notes agrees on them. Fences may use either
/// character and any run of at least three, and may sit in a block quote or
/// a list item. As in CommonMark, a fence in a quote or list item ends with
/// that container, and only a fence line in the same container closes it; a
/// shorter run inside a fence is content, not its close.
#[derive(Clone, Copy, Default)]
pub(super) struct Fences(Option<Open>);
#[derive(Clone, Copy)]
struct Open {
    ch: char,
    count: usize,
    /// Block quotes enclosing the fence.
    quotes: usize,
    /// The column a list item's content starts at, for a fence opened in
    /// one.
    list: Option<usize>,
}
impl Fences {
    pub(super) fn is_open(self) -> bool {
        self.0.is_some()
    }
    /// Moves past `line`: true when it opens or closes a fence.
    pub(super) fn advance(&mut self, line: &str) -> bool {
        static LIST_MARKER: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"^(?:[-+*]|[0-9]{1,9}[.)])[ \t]+").expect("list marker")
        });
        let line = line.trim_end_matches(['\n', '\r']);
        let Some(open) = self.0 else {
            let (quotes, rest) = unquote(line, usize::MAX);
            let (width, bytes) = indent(rest);
            let (list, rest) = match LIST_MARKER.find(&rest[bytes..]) {
                // The content column counts the marker's padding in columns, a
                // tab reaching the next tab stop, as GitHub does.
                Some(marker) => (
                    Some(column(width, marker.as_str())),
                    &rest[bytes + marker.end()..],
                ),
                None => (None, rest),
            };
            let Some((ch, count, _)) = token(rest) else {
                return false;
            };
            self.0 = Some(Open {
                ch,
                count,
                quotes,
                list,
            });
            return true;
        };
        let (quotes, rest) = unquote(line, open.quotes);
        let ended = |fences: &mut Self| {
            // The container ended, and the fence with it; the line is read
            // afresh, so it may open a fence of its own.
            fences.0 = None;
            fences.advance(line)
        };
        if quotes < open.quotes {
            return ended(self);
        }
        let rest = match open.list {
            Some(column) => {
                if indent(rest).0 < column && !rest.trim().is_empty() {
                    return ended(self);
                }
                rest
            }
            None => rest,
        };
        match token(rest) {
            Some((ch, count, tail))
                if ch == open.ch && count >= open.count && tail.trim().is_empty() =>
            {
                self.0 = None;
                true
            }
            _ => false,
        }
    }
    /// The line that closes the open fence where it is: inside its quotes,
    /// and indented to its list item's content.
    fn closer(self) -> Option<String> {
        self.0.map(|open| {
            format!(
                "{}{}{}",
                "> ".repeat(open.quotes),
                " ".repeat(open.list.unwrap_or(0)),
                open.ch.to_string().repeat(open.count)
            )
        })
    }
    /// Whether `line` would open a fence where none is open.
    fn opens(line: &str) -> bool {
        Self::default().advance(line)
    }
}

/// The column after `text` written from column `start`, a tab reaching the
/// next multiple of four.
fn column(start: usize, text: &str) -> usize {
    text.chars().fold(start, |column, c| match c {
        '\t' => column + 4 - column % 4,
        _ => column + 1,
    })
}

/// The width of `text`'s leading spaces and tabs, a tab reaching the next
/// multiple of four as in CommonMark, and how many bytes they take.
fn indent(text: &str) -> (usize, usize) {
    let mut width = 0;
    let mut bytes = 0;
    for b in text.bytes() {
        match b {
            b' ' => width += 1,
            b'\t' => width += 4 - width % 4,
            _ => break,
        }
        bytes += 1;
    }
    (width, bytes)
}

/// Up to `depth` block-quote markers stripped from `line`: how many there
/// were, and the rest.
fn unquote(line: &str, depth: usize) -> (usize, &str) {
    let mut rest = line;
    let mut quotes = 0;
    while quotes < depth {
        let Some(inner) = rest.trim_start_matches([' ', '\t']).strip_prefix('>') else {
            break;
        };
        rest = inner.strip_prefix(' ').unwrap_or(inner);
        quotes += 1;
    }
    (quotes, rest)
}

/// A fence delimiter at the start of `text`: its character, its length and
/// what follows it.
fn token(text: &str) -> Option<(char, usize, &str)> {
    let text = text.trim_start_matches([' ', '\t']);
    let ch = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let count = text.chars().take_while(|c| *c == ch).count();
    (count >= 3).then(|| (ch, count, &text[count..]))
}

/// Lines with the fence state in force at their start.
fn lines_with_fences(text: &str) -> Vec<(usize, &str, bool)> {
    let mut fences = Fences::default();
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        lines.push((offset, line, fences.is_open()));
        fences.advance(line);
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

/// A line with its inline code spans blanked, so a tag shown in code isn't a
/// tag. A backtick run that never closes is literal text.
fn without_code_spans(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut open = None::<(usize, usize)>;
    let mut offset = 0;
    while let Some(at) = line[offset..].find('`') {
        let start = offset + at;
        let run = line[start..].bytes().take_while(|b| *b == b'`').count();
        match open {
            None => {
                output.push_str(&line[offset..start]);
                open = Some((start, run));
            }
            Some((_, size)) if size == run => open = None,
            Some(_) => {}
        }
        offset = start + run;
    }
    match open {
        None => output.push_str(&line[offset..]),
        Some((start, _)) => output.push_str(&line[start..]),
    }
    output
}

/// Closes what the end of `text` leaves open: a code fence, then every
/// `<details>`, so a cut can't swallow what follows it.
pub(super) fn close_open_blocks(text: &str) -> String {
    let mut output = text.trim_end().to_owned();
    let mut fences = Fences::default();
    let mut details = 0_i32;
    for line in output.lines() {
        if !fences.advance(line) && !fences.is_open() {
            let lower = without_code_spans(line).to_ascii_lowercase();
            details += lower.matches("<details").count() as i32;
            details -= lower.matches("</details>").count() as i32;
        }
    }
    if let Some(closer) = fences.closer() {
        output.push('\n');
        output.push_str(&closer);
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
                || Fences::opens(line));
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
