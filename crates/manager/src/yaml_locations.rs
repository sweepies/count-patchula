//! Event paths prevent identical scalars elsewhere in a document from stealing
//! a location. Parser marks are translated to UTF-8 byte offsets through their
//! line and character column: yaml-rust2's index switches from characters to
//! bytes after a block scalar holding non-ASCII text, which would misplace
//! every later location.
use crate::Location;
use std::collections::BTreeMap;
use yaml_rust2::{
    parser::{Event, MarkedEventReceiver, Parser},
    scanner::{Marker, TScalarStyle},
};

struct Frame {
    pointer: String,
    mapping: bool,
    key: Option<String>,
    index: usize,
}
struct Receiver<'a> {
    text: &'a str,
    lines: Vec<usize>,
    doc: usize,
    stack: Vec<Frame>,
    locations: BTreeMap<String, Location>,
    anchors: BTreeMap<usize, String>,
    aliases: BTreeMap<String, String>,
}
impl Receiver<'_> {
    /// The byte offset of a 1-indexed line and 0-indexed character column.
    fn offset(&self, mark: Marker) -> Option<usize> {
        let start = *self.lines.get(mark.line().checked_sub(1)?)?;
        let line = &self.text[start..];
        let column = line
            .char_indices()
            .map(|(n, _)| n)
            .chain(std::iter::once(line.len()))
            .nth(mark.col())?;
        Some(start + column)
    }
    fn next(&mut self) -> String {
        if let Some(frame) = self.stack.last_mut() {
            if frame.mapping {
                format!(
                    "{}/{}",
                    frame.pointer,
                    frame
                        .key
                        .take()
                        .unwrap_or_default()
                        .replace('~', "~0")
                        .replace('/', "~1")
                )
            } else {
                let p = format!("{}/{}", frame.pointer, frame.index);
                frame.index += 1;
                p
            }
        } else {
            format!("/{}", self.doc)
        }
    }
    fn end(&self, start: usize, style: TScalarStyle, value: &str) -> Option<usize> {
        let raw = &self.text[start..];
        if style == TScalarStyle::Plain {
            return raw.starts_with(value).then_some(start + value.len());
        }
        let quote = match style {
            TScalarStyle::SingleQuoted => '\'',
            TScalarStyle::DoubleQuoted => '"',
            _ => return None,
        };
        if !raw.starts_with(quote) {
            return None;
        }
        let mut chars = raw.char_indices().skip(1).peekable();
        while let Some((n, c)) = chars.next() {
            if c == '\\' && quote == '"' {
                chars.next();
            } else if c == quote {
                if quote == '\'' && chars.peek().is_some_and(|(_, c)| *c == quote) {
                    chars.next();
                } else {
                    return Some(start + n + c.len_utf8());
                }
            }
        }
        None
    }
}
impl MarkedEventReceiver for Receiver<'_> {
    fn on_event(&mut self, event: Event, mark: Marker) {
        match event {
            Event::DocumentEnd => self.doc += 1,
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                let pointer = self.next();
                if anchor > 0 {
                    self.anchors.insert(anchor, pointer.clone());
                }
                self.stack.push(Frame {
                    pointer,
                    mapping: matches!(event, Event::MappingStart(..)),
                    key: None,
                    index: 0,
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                self.stack.pop();
            }
            Event::Scalar(value, style, anchor, _) => {
                if let Some(frame) = self.stack.last_mut()
                    && frame.mapping
                    && frame.key.is_none()
                {
                    frame.key = Some(value);
                    return;
                }
                let pointer = self.next();
                if anchor > 0 {
                    self.anchors.insert(anchor, pointer.clone());
                }
                if let Some(start) = self.offset(mark)
                    && let Some(end) = self.end(start, style, &value)
                {
                    self.locations.insert(
                        pointer.clone(),
                        Location::new(self.text, start, end, pointer),
                    );
                }
            }
            Event::Alias(anchor) => {
                let pointer = self.next();
                if let Some(target) = self.anchors.get(&anchor) {
                    self.aliases.insert(pointer, target.clone());
                }
            }
            _ => {}
        }
    }
}
pub(super) fn parse(text: &str) -> Result<BTreeMap<String, Location>, String> {
    Ok(parse_aliased(text)?.0)
}
/// Locations, and each alias's pointer mapped to the pointer of the node it
/// refers to. Only managers that edit an anchor once for all its aliases
/// resolve them; the rest leave aliased scalars unlocated.
pub(super) fn parse_aliased(text: &str) -> Result<(BTreeMap<String, Location>, Aliases), String> {
    let mut receiver = Receiver {
        text,
        lines: std::iter::once(0)
            .chain(text.match_indices('\n').map(|(n, _)| n + 1))
            .collect(),
        doc: 0,
        stack: Vec::new(),
        locations: BTreeMap::new(),
        anchors: BTreeMap::new(),
        aliases: BTreeMap::new(),
    };
    Parser::new_from_str(text)
        .load(&mut receiver, true)
        .map_err(|e| e.to_string())?;
    Ok((receiver.locations, Aliases(receiver.aliases)))
}
pub(super) struct Aliases(BTreeMap<String, String>);
impl Aliases {
    /// The pointer a value is defined at, following aliases anywhere along
    /// its path: `/0/steps/a/image` under `a: *base` becomes `/0/base/image`.
    pub(super) fn resolve(&self, pointer: &str) -> String {
        let mut pointer = pointer.to_owned();
        for _ in 0..16 {
            let Some((alias, target)) = self.0.iter().rev().find(|(alias, _)| {
                pointer
                    .strip_prefix(alias.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            }) else {
                break;
            };
            pointer = format!("{target}{}", &pointer[alias.len()..]);
        }
        pointer
    }
}
#[cfg(test)]
mod tests {
    /// Non-ASCII text in a block scalar must not shift later locations.
    #[test]
    fn locations_after_non_ascii_block_scalars() {
        for text in [
            "a: |\n  x \u{2192} y\nb: c\n",
            "a: >\n  caf\u{e9} \u{2014} ok\n\nb: c\n",
            "a: \u{2192}\nb: c\n",
        ] {
            let locations = super::parse(text).unwrap();
            let b = &locations["/0/b"];
            assert_eq!(&text[b.start..b.end], "c", "{text:?}");
        }
    }
}
