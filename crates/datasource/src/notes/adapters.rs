//! Upstream-specific notes for upstreams without GitHub releases.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

/// The cached representation of one parsed upstream document.
type Derive = fn(&[u8]) -> std::result::Result<Vec<u8>, serde_json::Error>;

/// Upstreams whose release notes are published outside GitHub releases.
#[derive(Clone, Copy)]
pub(super) enum Adapter {
    /// PostgreSQL keeps release notes in SGML on the release branch.
    Postgres,
    /// Go publishes its release history as data in the website repository.
    Go,
}

/// The adapter table. Upstream-specific rules live here and nowhere else.
pub(super) fn adapter(repo: &str) -> Option<Adapter> {
    match repo {
        "postgres/postgres" => Some(Adapter::Postgres),
        "golang/go" => Some(Adapter::Go),
        _ => None,
    }
}

/// One file of an upstream directory listing.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    name: String,
    path: String,
    sha: String,
}

/// Keeps directory listings small: only the fields the adapters use.
pub(super) fn compact_listing(bytes: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Listed {
        name: String,
        path: String,
        sha: String,
        #[serde(rename = "type")]
        kind: String,
    }
    let listed: Vec<Listed> = serde_json::from_slice(bytes)?;
    let listing: Vec<Entry> = listed
        .into_iter()
        .filter(|file| matches!(file.kind.as_str(), "file" | "blob"))
        .map(|file| Entry {
            name: file.name,
            path: file.path,
            sha: file.sha,
        })
        .collect();
    serde_json::to_vec(&listing)
}

/// Decode a GitHub blob into its text content.
fn blob_text(bytes: &[u8]) -> std::result::Result<String, serde_json::Error> {
    #[derive(Deserialize)]
    struct Blob {
        content: String,
    }
    let blob: Blob = serde_json::from_slice(bytes)?;
    let decoded = STANDARD
        .decode(
            blob.content
                .bytes()
                .filter(|byte| !byte.is_ascii_whitespace())
                .collect::<Vec<_>>(),
        )
        .map_err(serde::de::Error::custom)?;
    String::from_utf8(decoded).map_err(serde::de::Error::custom)
}

/// Parse the PostgreSQL release notes of one major.
pub(super) fn compact_sgml(bytes: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&parse_release_sgml(&blob_text(bytes)?))
}

/// Parse the Go release history.
pub(super) fn compact_history(bytes: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&parse_go_history(&blob_text(bytes)?))
}

impl Client {
    /// Adapter notes for the requested range, and whether the adapter covered
    /// the whole range.
    pub(super) async fn adapter_notes(
        &self,
        adapter: Adapter,
        old: &Parsed,
        new: &Parsed,
    ) -> Result<(Vec<Section>, bool)> {
        let (sections, complete) = match adapter {
            Adapter::Postgres => self.postgres_notes(old, new).await?,
            Adapter::Go => self.go_notes().await?,
        };
        // The range filter, the newest-first order and the version budget are
        // the same as for releases and changelogs.
        let mut sorted: Vec<_> = sections
            .into_iter()
            .filter_map(|section| Some((parsed(&section.version)?, section)))
            .filter(|(version, _)| in_range(version, old, new))
            .collect();
        sorted.sort_by(|(a, _), (b, _)| Ecosystem::Loose.order(b, a));
        let complete = complete && sorted.len() <= 32;
        sorted.truncate(32);
        Ok((
            sorted.into_iter().map(|(_, section)| section).collect(),
            complete,
        ))
    }

    /// PostgreSQL publishes release notes as SGML on the release branch only.
    /// The file at the target tag is immutable, so read it there.
    async fn postgres_notes(&self, old: &Parsed, new: &Parsed) -> Result<(Vec<Section>, bool)> {
        let major = new.0.major;
        let tag = if new.0.minor == 0 {
            format!("REL_{major}")
        } else {
            format!("REL_{major}_{}", new.0.minor)
        };
        let listing = self
            .adapter_listing("postgres/postgres", "doc/src/sgml", &format!("?ref={tag}"))
            .await?;
        let name = format!("release-{major}.sgml");
        let file = listing
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| Error::Notes("missing PostgreSQL release notes".into()))?;
        let sections = self
            .adapter_sections(
                "postgres/postgres",
                &file.path,
                &file.sha,
                "parsed-postgres-sgml-v1",
                compact_sgml,
            )
            .await?;
        // Only the target major's file is read, so a range spanning majors
        // keeps the notes of the earlier major missing.
        Ok((sections, old.0.major == major))
    }

    /// Go publishes its release history as data in the website repository.
    async fn go_notes(&self) -> Result<(Vec<Section>, bool)> {
        let listing = self
            .adapter_listing("golang/website", "internal/history", "")
            .await?;
        let file = listing
            .iter()
            .find(|entry| entry.name == "release.go")
            .ok_or_else(|| Error::Notes("missing Go release history".into()))?;
        let sections = self
            .adapter_sections(
                "golang/website",
                &file.path,
                &file.sha,
                "parsed-go-history-v1",
                compact_history,
            )
            .await?;
        Ok((sections, true))
    }

    /// A small directory listing, so an immutable blob can be read by SHA.
    async fn adapter_listing(
        &self,
        repo: &str,
        directory: &str,
        query: &str,
    ) -> Result<Vec<Entry>> {
        let path = format!("repos/{repo}/contents/{directory}{query}");
        let bytes = self
            .note_read(&path, async {
                Ok(self
                    .github
                    .get_derived(&path, "notes-adapter-listing-v1", compact_listing)
                    .await?
                    .body)
            })
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Parsed notes of one immutable blob, cached by blob SHA.
    async fn adapter_sections(
        &self,
        repo: &str,
        path: &str,
        sha: &str,
        config_hash: &'static str,
        derive: Derive,
    ) -> Result<Vec<Section>> {
        let key = ExtractionKey {
            repo: format!("notes-adapter:{repo}:{path}"),
            default_branch_commit: sha.to_string(),
            config_hash: config_hash.to_string(),
        };
        let shared = self
            .changelogs
            .lock()
            .await
            .entry(format!("adapter:{repo}:{path}:{sha}"))
            .or_default()
            .clone();
        let mut saved = shared.lock().await;
        if let Some(sections) = &*saved {
            return Ok(sections.iter().cloned().collect());
        }
        let bytes = if let Some(bytes) = self.cache.extraction_get(&key).await? {
            bytes
        } else {
            let response = self
                .github
                .get_derived(
                    &format!("repos/{repo}/git/blobs/{sha}"),
                    config_hash,
                    derive,
                )
                .await?;
            self.cache.extraction_put(&key, &response.body).await?;
            response.body
        };
        let sections = Arc::new(serde_json::from_slice::<Vec<Section>>(&bytes)?);
        *saved = Some(sections.clone());
        Ok(sections.iter().cloned().collect())
    }
}

/// A parsed SGML node. The release notes use a small, regular subset.
enum Node {
    Text(String),
    Element {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<Node>,
    },
}

/// An open SGML element.
type Frame = (String, Vec<(String, String)>, Vec<Node>);

/// Parse the SGML subset used by the PostgreSQL release notes.
fn sgml_nodes(text: &str) -> Vec<Node> {
    let mut root: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < text.len() {
        let Some(offset) = text[index..].find('<') else {
            break;
        };
        let open = index + offset;
        if open > start {
            sgml_push_text(&mut stack, &mut root, &text[start..open]);
        }
        if text[open..].starts_with("<!--") {
            index = match text[open..].find("-->") {
                Some(end) => open + end + 3,
                None => text.len(),
            };
            start = index;
            continue;
        }
        let Some(end) = sgml_tag_end(text, open) else {
            break;
        };
        let tag = text[open + 1..end].trim();
        index = end + 1;
        start = index;
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            sgml_close(&mut stack, &mut root, name.trim());
            continue;
        }
        let self_closing = tag.ends_with('/');
        let (name, attrs) = sgml_tag(tag.trim_end_matches('/').trim());
        if !sgml_element(&name) {
            // Not an element: keep the text, such as an address.
            sgml_push_text(&mut stack, &mut root, &text[open..end + 1]);
            continue;
        }
        if self_closing {
            sgml_push_element(&mut stack, &mut root, name, attrs, Vec::new());
        } else {
            stack.push((name, attrs, Vec::new()));
        }
    }
    if start < text.len() {
        sgml_push_text(&mut stack, &mut root, &text[start..]);
    }
    while let Some((name, attrs, children)) = stack.pop() {
        sgml_push_element(&mut stack, &mut root, name, attrs, children);
    }
    root
}

fn sgml_push_text(stack: &mut [Frame], root: &mut Vec<Node>, text: &str) {
    let nodes = match stack.last_mut() {
        Some(frame) => &mut frame.2,
        None => root,
    };
    match nodes.last_mut() {
        Some(Node::Text(existing)) => existing.push_str(text),
        _ => nodes.push(Node::Text(text.to_string())),
    }
}

fn sgml_push_element(
    stack: &mut [Frame],
    root: &mut Vec<Node>,
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
) {
    let nodes = match stack.last_mut() {
        Some(frame) => &mut frame.2,
        None => root,
    };
    nodes.push(Node::Element {
        name,
        attrs,
        children,
    });
}

fn sgml_close(stack: &mut Vec<Frame>, root: &mut Vec<Node>, name: &str) {
    let Some(index) = stack.iter().rposition(|frame| frame.0 == name) else {
        return;
    };
    while stack.len() > index {
        let (name, attrs, children) = stack.pop().expect("open element");
        sgml_push_element(stack, root, name, attrs, children);
    }
}

/// The index of the `>` that closes the tag opened at `open`.
fn sgml_tag_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut quote = 0u8;
    let mut index = open + 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if quote != 0 {
            if byte == quote {
                quote = 0;
            }
        } else if byte == b'"' || byte == b'\'' {
            quote = byte;
        } else if byte == b'>' {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The name and attributes of one tag.
fn sgml_tag(tag: &str) -> (String, Vec<(String, String)>) {
    let mut end = tag.len();
    for (index, character) in tag.char_indices() {
        if character.is_whitespace() {
            end = index;
            break;
        }
    }
    let name = tag[..end].to_ascii_lowercase();
    let rest = &tag[end..];
    let bytes = rest.as_bytes();
    let mut attrs = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let start = index;
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'=' {
            index += 1;
        }
        if start == index {
            break;
        }
        let key = rest[start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            attrs.push((key, String::new()));
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let value = if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            index += 1;
            let value_start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            let value = rest[value_start..index].to_string();
            index += 1;
            value
        } else {
            let value_start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            rest[value_start..index].to_string()
        };
        attrs.push((key, value));
    }
    (name, attrs)
}

/// Whether a name is an SGML element name.
fn sgml_element(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// One attribute of an element.
fn sgml_attr(attrs: &[(String, String)], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// Expand the entities used by the upstream documents.
fn expand_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let replacement = match entity {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" => Some("'".to_string()),
            "sect" => Some("§".to_string()),
            "mdash" => Some("—".to_string()),
            "ndash" => Some("–".to_string()),
            "hellip" => Some("…".to_string()),
            "nbsp" => Some(" ".to_string()),
            "eacute" => Some("é".to_string()),
            "aring" => Some("å".to_string()),
            "commit_baseurl" => Some("https://postgr.es/c/".to_string()),
            _ => entity
                .strip_prefix('#')
                .and_then(|code| {
                    if let Some(hex) = code.strip_prefix('x').or_else(|| code.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else {
                        code.parse::<u32>().ok().and_then(char::from_u32)
                    }
                })
                .map(|character| character.to_string()),
        };
        match replacement {
            Some(text) => {
                out.push_str(&text);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapse whitespace runs into single spaces.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(character);
    }
    out
}

/// Inline SGML content as Markdown.
fn sgml_inline(node: &Node) -> String {
    match node {
        Node::Text(text) => expand_entities(text),
        Node::Element {
            name,
            attrs,
            children,
        } => {
            let text = sgml_inline_children(children);
            match name.as_str() {
                "ulink" => {
                    let url = sgml_attr(attrs, "url")
                        .map(|url| expand_entities(&url))
                        .unwrap_or_default();
                    if url.is_empty() {
                        text
                    } else {
                        format!("[{text}]({url})")
                    }
                }
                "link" | "xref" => {
                    if text.trim().is_empty() {
                        sgml_attr(attrs, "linkend").unwrap_or_default()
                    } else {
                        text
                    }
                }
                "literal" | "command" | "varname" | "filename" | "function" | "type" | "option"
                | "application" | "structname" | "structfield" | "acronym" | "productname"
                | "replaceable" | "systemitem" | "envar" | "member" => {
                    format!("`{text}`")
                }
                "programlisting" => format!("```\n{text}\n```"),
                _ => text,
            }
        }
    }
}

/// Inline content of several nodes.
fn sgml_inline_children(nodes: &[Node]) -> String {
    let mut text = String::new();
    for node in nodes {
        text.push_str(&sgml_inline(node));
    }
    text
}

/// Whether an element is a list.
fn sgml_list(name: &str) -> bool {
    matches!(name, "itemizedlist" | "simplelist")
}

/// Block-level SGML content as Markdown blocks.
fn sgml_blocks(nodes: &[Node]) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut inline = String::new();
    sgml_walk(nodes, &mut blocks, &mut inline, 0);
    sgml_paragraph(&mut blocks, &mut inline);
    blocks
}

/// Move the accumulated inline text into a paragraph block.
fn sgml_paragraph(blocks: &mut Vec<String>, inline: &mut String) {
    let text = collapse(inline);
    inline.clear();
    if !text.is_empty() {
        blocks.push(text);
    }
}

fn sgml_walk(nodes: &[Node], blocks: &mut Vec<String>, inline: &mut String, depth: usize) {
    for node in nodes {
        match node {
            Node::Text(text) => inline.push_str(&expand_entities(text)),
            Node::Element { name, children, .. } => match name.as_str() {
                // The release date and the section titles are not part of the
                // body: the renderer already shows the version.
                "formalpara" | "title" => {}
                "para" => {
                    let mut text = String::new();
                    for child in children {
                        match child {
                            Node::Text(value) => text.push_str(&expand_entities(value)),
                            Node::Element { name, .. } if sgml_list(name) => {
                                inline.push_str(&text);
                                text.clear();
                                sgml_paragraph(blocks, inline);
                                sgml_list_blocks(child, blocks, depth);
                            }
                            child => text.push_str(&sgml_inline(child)),
                        }
                    }
                    inline.push_str(&text);
                    sgml_paragraph(blocks, inline);
                }
                "itemizedlist" | "simplelist" => sgml_list_blocks(node, blocks, depth),
                "sect2" | "sect3" | "sect4" => {
                    if let Some(title) = sgml_title(children) {
                        let title = collapse(&title);
                        if !title.is_empty() {
                            blocks.push(format!("**{title}**"));
                        }
                    }
                    for child in children {
                        if matches!(child, Node::Element { name, .. } if name == "title") {
                            continue;
                        }
                        sgml_walk(std::slice::from_ref(child), blocks, inline, depth);
                    }
                }
                _ => inline.push_str(&sgml_inline(node)),
            },
        }
    }
}

/// The title of a section.
fn sgml_title(children: &[Node]) -> Option<String> {
    children.iter().find_map(|child| match child {
        Node::Element { name, children, .. } if name == "title" => {
            Some(sgml_inline_children(children))
        }
        _ => None,
    })
}

/// The items of a list, and the lists nested in them.
fn sgml_list_blocks(list: &Node, blocks: &mut Vec<String>, depth: usize) {
    let Node::Element { children, .. } = list else {
        return;
    };
    for item in children {
        let Node::Element {
            name,
            children: item,
            ..
        } = item
        else {
            continue;
        };
        if name != "listitem" {
            continue;
        }
        let mut text = String::new();
        let mut lists = Vec::new();
        sgml_item(item, &mut text, &mut lists);
        let text = collapse(&text);
        if !text.is_empty() {
            blocks.push(format!("{}- {text}", "  ".repeat(depth)));
        }
        for list in lists {
            sgml_list_blocks(list, blocks, depth + 1);
        }
    }
}

/// The text of a list item, and the lists nested in it.
fn sgml_item<'a>(nodes: &'a [Node], text: &mut String, lists: &mut Vec<&'a Node>) {
    for node in nodes {
        match node {
            Node::Text(value) => text.push_str(&expand_entities(value)),
            Node::Element { name, .. } if sgml_list(name) => lists.push(node),
            Node::Element { name, children, .. } if name == "para" => {
                for child in children {
                    match child {
                        Node::Text(value) => text.push_str(&expand_entities(value)),
                        Node::Element { name, .. } if sgml_list(name) => lists.push(child),
                        child => text.push_str(&sgml_inline(child)),
                    }
                }
            }
            node => text.push_str(&sgml_inline(node)),
        }
    }
}

/// The PostgreSQL release notes of one major, as Markdown sections.
pub fn parse_release_sgml(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    for node in &sgml_nodes(text) {
        let Node::Element {
            name,
            attrs,
            children,
        } = node
        else {
            continue;
        };
        if name != "sect1" {
            continue;
        }
        let Some(id) = sgml_attr(attrs, "id") else {
            continue;
        };
        let Some(version) = sgml_version(&id) else {
            continue;
        };
        let documentation = if version.ends_with(".0") {
            version[..version.len() - 2].to_string()
        } else {
            version.clone()
        };
        sections.push(Section {
            link: format!("https://www.postgresql.org/docs/release/{documentation}/"),
            version,
            body: sgml_body(children),
        });
    }
    sections
}

/// The version of a release identifier, such as `release-17-6` or `release-17`.
fn sgml_version(id: &str) -> Option<String> {
    let rest = id.strip_prefix("release-")?;
    let mut parts = rest.split('-');
    let major = parts.next()?;
    let minor = parts.next().unwrap_or("0");
    if parts.next().is_some() {
        return None;
    }
    if !major.bytes().all(|byte| byte.is_ascii_digit())
        || !minor.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{major}.{minor}"))
}

/// The Markdown body of one release.
fn sgml_body(children: &[Node]) -> String {
    let blocks = sgml_blocks(children);
    let mut body = String::new();
    let mut previous_item = false;
    for block in blocks {
        let item = block.trim_start().starts_with("- ");
        if !body.is_empty() {
            body.push_str(if item && previous_item { "\n" } else { "\n\n" });
        }
        body.push_str(&block);
        previous_item = item;
    }
    body
}

/// The fixes of one Go release.
#[derive(Default)]
struct GoFixes {
    quantifier: String,
    components: Vec<String>,
    packages: Vec<String>,
}

/// Go release history entries as Markdown sections.
pub fn parse_go_history(text: &str) -> Vec<Section> {
    go_entries(text)
        .iter()
        .filter_map(|entry| go_section(entry))
        .collect()
}

/// The `Releases` entries of the history file.
fn go_entries(text: &str) -> Vec<String> {
    let Some(start) = text.find("var Releases") else {
        return Vec::new();
    };
    let bytes = text.as_bytes();
    let mut index = start;
    let mut depth = 0usize;
    let mut entry: Option<usize> = None;
    let mut entries = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'`' | b'\'' => index = go_skip_literal(text, index),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index = match text[index..].find('\n') {
                    Some(end) => index + end + 1,
                    None => bytes.len(),
                };
            }
            b'{' => {
                if depth == 1 {
                    entry = Some(index);
                }
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 1
                    && let Some(start) = entry.take()
                {
                    entries.push(text[start..index + 1].to_string());
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    entries
}

/// The index after the Go literal at `index`.
fn go_skip_literal(text: &str, index: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[index];
    let mut position = index + 1;
    while position < bytes.len() {
        match bytes[position] {
            b'\\' if quote == b'"' => position += 2,
            byte if byte == quote => return position + 1,
            _ => position += 1,
        }
    }
    bytes.len()
}

/// The integers of a `Date{2026, 10, 8}` or `Version{1, 27, 2}` literal.
fn go_numbers(text: &str, field: &str) -> Option<Vec<u64>> {
    let index = text.find(field)?;
    let rest = &text[index + field.len()..];
    let open = rest.find('{')?;
    let close = rest.find('}')?;
    rest[open + 1..close]
        .split(',')
        .map(|part| part.trim().parse::<u64>().ok())
        .collect()
}

/// The index of the `}` that closes the block opened at `open`.
fn go_block_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'`' | b'\'' => index = go_skip_literal(text, index),
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

/// The Go string literal at the start of `text`, and its end.
fn go_literal(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    match *bytes.first()? {
        b'`' => {
            let end = text[1..].find('`')?;
            Some((text[1..1 + end].to_string(), end + 2))
        }
        b'"' => {
            let mut value = String::new();
            let mut index = 1;
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => return Some((value, index + 1)),
                    b'\\' => {
                        index += 1;
                        let escape = *bytes.get(index)?;
                        value.push(match escape {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            other => other as char,
                        });
                        index += 1;
                    }
                    _ => {
                        let character = text[index..].chars().next()?;
                        value.push(character);
                        index += character.len_utf8();
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// The string value of a field.
fn go_string(text: &str, field: &str) -> Option<String> {
    let index = text.find(field)?;
    let rest = text[index + field.len()..].trim_start();
    go_literal(rest).map(|(value, _)| value)
}

/// The string list of a field.
fn go_strings(text: &str, field: &str) -> Vec<String> {
    let Some(index) = text.find(field) else {
        return Vec::new();
    };
    let rest = &text[index + field.len()..];
    let Some(open) = rest.find('{') else {
        return Vec::new();
    };
    let bytes = rest.as_bytes();
    let mut values = Vec::new();
    let mut index = open + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'`' => {
                let Some((value, end)) = go_literal(&rest[index..]) else {
                    break;
                };
                values.push(value);
                index += end;
            }
            b'}' => break,
            _ => index += 1,
        }
    }
    values
}

/// The fixes of one release summary.
fn go_fixes(text: &str, field: &str) -> Option<GoFixes> {
    let index = text.find(field)?;
    let rest = &text[index + field.len()..];
    let open = rest.find('{')?;
    let close = go_block_end(rest, open)?;
    let block = &rest[open + 1..close];
    Some(GoFixes {
        quantifier: go_string(block, "Quantifier: ").unwrap_or_default(),
        components: go_strings(block, "Components: "),
        packages: go_strings(block, "Packages: "),
    })
}

/// One Go release as a Markdown section.
fn go_section(entry: &str) -> Option<Section> {
    let numbers = go_numbers(entry, "Version: Version")?;
    let date = go_numbers(entry, "Date: Date")?;
    if numbers.len() != 3 || date.len() != 3 {
        return None;
    }
    let prefix = if numbers[1] != 0 {
        format!("{}.{}", numbers[0], numbers[1])
    } else {
        format!("{}", numbers[0])
    };
    let version = format!("{}.{}.{}", numbers[0], numbers[1], numbers[2]);
    let date = format!("{:04}-{:02}-{:02}", date[0], date[1], date[2]);
    let future = entry.contains("Future: true");
    let security = go_fixes(entry, "Security: &FixSummary");
    let bug = go_fixes(entry, "Bug: &FixSummary");
    let more = go_string(entry, "More: ");
    let custom = go_string(entry, "CustomSummary: ");
    let released = if future { "planned for" } else { "released" };
    let body = match custom {
        Some(custom) => format!("go{version} ({released} {date}) {}", go_html(&custom)),
        None if numbers[2] == 0 => {
            format!(
                "Go {version} ({released} {date}) is a major release of Go. Read the [Go {prefix} Release Notes](https://go.dev/doc/go{prefix}) for more information."
            )
        }
        None => {
            let mut summary = String::new();
            summary.push_str(if future { "will include" } else { "includes" });
            if let Some(security) = &security {
                summary.push(' ');
                summary.push_str(&go_phrase(security, true, true));
            }
            if security.is_some() && bug.is_some() {
                summary.push_str(", as well as");
            }
            if let Some(bug) = &bug {
                summary.push(' ');
                summary.push_str(&go_phrase(bug, false, security.is_some()));
            }
            if security.is_some() || bug.is_some() {
                summary.push('.');
            }
            if let Some(more) = &more {
                summary.push(' ');
                summary.push_str(&go_html(more));
            }
            let milestone = format!(
                "https://github.com/golang/go/issues?q=milestone%3AGo{version}+label%3ACherryPickApproved"
            );
            if future {
                format!("go{version} ({released} {date}) {summary}")
            } else {
                format!(
                    "go{version} ({released} {date}) {summary} See the [Go {version} milestone]({milestone}) on our issue tracker for details."
                )
            }
        }
    };
    Some(Section {
        link: format!("https://go.dev/doc/devel/release#go{version}"),
        version,
        body,
    })
}

/// The phrase of one fix summary, as the website renders it.
fn go_phrase(fixes: &GoFixes, security: bool, with_security: bool) -> String {
    let singular = if security {
        fixes.quantifier == "a" || fixes.quantifier == "one"
    } else {
        fixes.quantifier == "a"
    };
    let noun = if security {
        "security"
    } else if with_security {
        "bug"
    } else {
        ""
    };
    let mut phrase = String::new();
    if !fixes.quantifier.is_empty() {
        phrase.push_str(&fixes.quantifier);
        phrase.push(' ');
    }
    if !noun.is_empty() {
        phrase.push_str(noun);
        phrase.push(' ');
    }
    phrase.push_str(if singular { "fix" } else { "fixes" });
    let target = go_target(fixes);
    if !target.is_empty() {
        phrase.push_str(" to ");
        phrase.push_str(&target);
    }
    phrase
}

/// The components and packages of one fix summary.
fn go_target(fixes: &GoFixes) -> String {
    let mut target = String::new();
    for (index, component) in fixes.components.iter().enumerate() {
        if fixes.packages.is_empty() {
            match index {
                0 => {}
                1 if fixes.components.len() == 2 => target.push_str(" and "),
                _ if fixes.components.len() >= 3 => {
                    if index + 1 < fixes.components.len() {
                        target.push_str(", ");
                    } else {
                        target.push_str(", and ");
                    }
                }
                _ => {}
            }
        } else if index > 0 {
            target.push_str(", ");
        }
        target.push_str(&go_html(component));
    }
    if !fixes.components.is_empty() && !fixes.packages.is_empty() {
        if fixes.components.len() + fixes.packages.len() >= 3 {
            target.push(',');
        }
        target.push_str(" and ");
    }
    if !fixes.packages.is_empty() {
        target.push_str("the ");
        for (index, package) in fixes.packages.iter().enumerate() {
            match index {
                0 => {}
                1 if fixes.packages.len() == 2 => target.push_str(" and "),
                _ if fixes.packages.len() >= 3 => {
                    if index + 1 < fixes.packages.len() {
                        target.push_str(", ");
                    } else {
                        target.push_str(", and ");
                    }
                }
                _ => {}
            }
            target.push_str(&format!("`{package}`"));
        }
        target.push_str(if fixes.packages.len() == 1 {
            " package"
        } else {
            " packages"
        });
    }
    target
}

/// The HTML of the Go history as Markdown.
fn go_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('<') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        if let Some(end) = tail.find("</code>") {
            out.push('`');
            out.push_str(&tail[6..end]);
            out.push('`');
            rest = &tail[end + 7..];
            continue;
        }
        if tail.starts_with("<a href=")
            && let Some(end) = tail.find('>')
        {
            let href = tail[9..end].trim_matches('"');
            let after = &tail[end + 1..];
            if let Some(close) = after.find("</a>") {
                out.push('[');
                out.push_str(&after[..close]);
                out.push_str("](");
                out.push_str(href);
                out.push(')');
                rest = &after[close + 4..];
                continue;
            }
        }
        out.push('<');
        rest = &tail[1..];
    }
    out.push_str(&expand_entities(rest));
    out
}
