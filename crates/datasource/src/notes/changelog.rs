use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::BTreeMap, sync::LazyLock};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Section {
    pub version: String,
    pub link: String,
    pub body: String,
}

pub(super) fn version_text(text: &str) -> Option<&str> {
    static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?x)(?:^|[\s\[(@/_-])v?(\d+\.\d+(?:\.\d+)?(?:[-.]?(?:alpha|beta|rc|pre|dev)[.\d-]*)?(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?(?:\+[0-9A-Za-z.-]+)?)(?:$|[\s\])_:])").unwrap()
    });
    VERSION
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

fn anchor(text: &str) -> String {
    // GitHub's heading slug: retain letters/digits/_/-, remove punctuation.
    let mut out = String::new();
    let mut link_target = false;
    let mut previous = '\0';
    for c in text.chars().flat_map(char::to_lowercase) {
        if c == '(' && previous == ']' {
            link_target = true;
        }
        if !link_target {
            if c.is_alphanumeric() || matches!(c, '_' | '-') {
                out.push(c);
            } else if c.is_whitespace() {
                out.push('-');
            }
        }
        if c == ')' {
            link_target = false;
        }
        previous = c;
    }
    out
}

/// One linear pass over a document, independent of selected versions or heading
/// levels. Byte offsets avoid copying subsection bodies until a section closes.
pub fn parse_changelog(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut active: Option<(String, String, usize, usize)> = None;
    let mut slugs = BTreeMap::<String, usize>::new();
    let mut fence: Option<(char, usize)> = None;
    let mut lines = text.split_inclusive('\n').peekable();
    let mut offset = 0;
    while let Some(line) = lines.next() {
        let start = offset;
        offset += line.len();
        let trim = line.trim();
        let fence_char = trim.chars().next().unwrap_or('\0');
        let fence_len = trim.chars().take_while(|c| *c == fence_char).count();
        if matches!(fence_char, '`' | '~') && fence_len >= 3 {
            if let Some((kind, len)) = fence {
                if kind == fence_char && fence_len >= len && trim.chars().all(|c| c == kind) {
                    fence = None;
                }
            } else {
                fence = Some((fence_char, fence_len));
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let hashes = trim.bytes().take_while(|b| *b == b'#').count();
        let atx = (1..=6).contains(&hashes)
            && trim
                .as_bytes()
                .get(hashes)
                .is_some_and(u8::is_ascii_whitespace);
        let setext = lines.peek().is_some_and(|line| {
            let s = line.trim();
            !trim.is_empty()
                && s.len() >= 3
                && (s.bytes().all(|b| b == b'=') || s.bytes().all(|b| b == b'-'))
        });
        let plain = trim.starts_with("Changes with nginx ")
            || trim.starts_with("Redis ") && trim.contains(" Released ");
        let (heading, level) = if atx {
            (trim[hashes..].trim().trim_end_matches('#').trim(), hashes)
        } else if setext {
            let underline = lines.next().expect("peeked");
            offset += underline.len();
            (
                trim,
                if underline.trim().starts_with('=') {
                    1
                } else {
                    2
                },
            )
        } else if plain {
            (trim, 1)
        } else {
            continue;
        };
        let slug = anchor(heading);
        let count = slugs.entry(slug.clone()).or_default();
        let slug = if *count == 0 {
            slug
        } else {
            format!("{slug}-{count}")
        };
        *count += 1;
        let version = version_text(heading);
        if active
            .as_ref()
            .is_some_and(|(_, _, _, old_level)| version.is_some() || level <= *old_level)
        {
            let (version, link, body_start, _) = active.take().expect("active");
            sections.push(Section {
                version,
                link,
                body: text[body_start..start].trim().into(),
            });
        }
        if let Some(version) = version {
            active = Some((version.into(), slug, offset, level));
        }
    }
    if let Some((version, link, start, _)) = active {
        sections.push(Section {
            version,
            link,
            body: text[start..].trim().into(),
        });
    }
    sections
}

#[derive(Serialize, Deserialize)]
pub(super) struct Tree {
    #[serde(default)]
    pub tree: Vec<File>,
    #[serde(default)]
    pub truncated: bool,
}
#[derive(Serialize, Deserialize)]
pub(super) struct File {
    pub path: String,
    pub sha: String,
    #[serde(rename = "type")]
    pub kind: String,
}
pub(super) fn compact_tree(bytes: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Document {
        Directory(Vec<File>),
        Tree(Tree),
    }
    let mut tree = match serde_json::from_slice(bytes)? {
        Document::Directory(tree) => Tree {
            truncated: tree.len() >= 1000,
            tree,
        },
        Document::Tree(tree) => tree,
    };
    tree.tree
        .retain(|file| matches!(file.kind.as_str(), "dir" | "tree") || is_changelog(&file.path));
    serde_json::to_vec(&tree)
}
fn is_changelog(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_uppercase();
    let stem = name.split('.').next().unwrap_or(&name);
    let base = [
        "CHANGELOG",
        "CHANGES",
        "HISTORY",
        "RELEASES",
        "NEWS",
        "00-RELEASENOTES",
    ];
    base.iter().any(|base| {
        stem == *base
            || stem
                .strip_prefix(base)
                .is_some_and(|s| s.starts_with(['_', '-']))
    }) && (!name.contains('.')
        || ["MD", "MARKDOWN", "TXT", "RST"].contains(&name.rsplit('.').next().unwrap_or("")))
}

pub(super) fn relevant(file: &File, directory: Option<&str>, low: u64, high: u64) -> bool {
    let local = directory
        .and_then(|dir| file.path.strip_prefix(&format!("{dir}/")))
        .unwrap_or(&file.path);
    let local = local.to_ascii_lowercase();
    if local.contains('/')
        && ![
            "doc/changelogs/",
            "docs/changelogs/",
            "changelogs/",
            "changelog/",
            "changes/",
            "news/",
            "releases/",
            "docs/",
            "doc/",
        ]
        .iter()
        .any(|prefix| local.strip_prefix(prefix).is_some_and(|p| !p.contains('/')))
    {
        return false;
    }
    let name = file
        .path
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if let Some(major) = name
        .strip_prefix("CHANGELOG_V")
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u64>().ok())
    {
        return (low..=high).contains(&major);
    }
    true
}

pub(super) fn compact_blob(bytes: &[u8]) -> std::result::Result<Vec<u8>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Blob {
        content: String,
    }
    let blob: Blob = serde_json::from_slice(bytes)?;
    let decoded = STANDARD
        .decode(
            blob.content
                .bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .collect::<Vec<_>>(),
        )
        .map_err(serde::de::Error::custom)?;
    let text = std::str::from_utf8(&decoded).map_err(serde::de::Error::custom)?;
    serde_json::to_vec(&parse_changelog(text))
}
