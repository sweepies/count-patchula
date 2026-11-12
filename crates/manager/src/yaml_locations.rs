//! Event paths prevent identical scalars elsewhere in a document from stealing
//! a location. Parser marks are translated to UTF-8 byte offsets through their
//! line and character column: yaml-rust2's index switches from characters to
//! bytes after a block scalar holding non-ASCII text, which would misplace
//! every later location.
use crate::{Lines, Location};
use std::collections::BTreeMap;
use yaml_rust2::{
    parser::{Event, MarkedEventReceiver, Parser},
    scanner::{Marker, TScalarStyle},
};

struct Frame {
    /// `None` inside a node no pointer can name, such as a complex key.
    pointer: Option<String>,
    mapping: bool,
    slot: Slot,
    index: usize,
}
/// What a mapping's next node is.
#[derive(PartialEq)]
enum Slot {
    Key,
    /// The value of this key; `None` when the key was not a scalar, such as
    /// `? [a, b]` or an alias, so no pointer names the value.
    Value(Option<String>),
}
struct Receiver<'a, 'b> {
    text: &'a Lines<'b>,
    doc: usize,
    stack: Vec<Frame>,
    locations: BTreeMap<String, Location>,
    anchors: BTreeMap<usize, String>,
    aliases: BTreeMap<String, String>,
}
impl Receiver<'_, '_> {
    /// The byte offset of a 1-indexed line and 0-indexed character column.
    fn offset(&self, mark: Marker) -> Option<usize> {
        let start = self.text.line_start(mark.line())?;
        let line = &self.text[start..];
        let column = line
            .char_indices()
            .map(|(n, _)| n)
            .chain(std::iter::once(line.len()))
            .nth(mark.col())?;
        Some(start + column)
    }
    /// The pointer of the node starting now, moving its parent past it.
    fn next(&mut self) -> Option<String> {
        let Some(frame) = self.stack.last_mut() else {
            return Some(format!("/{}", self.doc));
        };
        if !frame.mapping {
            let index = frame.index;
            frame.index += 1;
            return Some(format!("{}/{index}", frame.pointer.as_ref()?));
        }
        match std::mem::replace(&mut frame.slot, Slot::Key) {
            // A key that is not a scalar: it and its value are unnamed.
            Slot::Key => {
                frame.slot = Slot::Value(None);
                None
            }
            Slot::Value(key) => Some(format!(
                "{}/{}",
                frame.pointer.as_ref()?,
                crate::pointer_token(&key?)
            )),
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
impl MarkedEventReceiver for Receiver<'_, '_> {
    fn on_event(&mut self, event: Event, mark: Marker) {
        match event {
            Event::DocumentEnd => self.doc += 1,
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                let pointer = self.next();
                if anchor > 0
                    && let Some(pointer) = &pointer
                {
                    self.anchors.insert(anchor, pointer.clone());
                }
                self.stack.push(Frame {
                    pointer,
                    mapping: matches!(event, Event::MappingStart(..)),
                    slot: Slot::Key,
                    index: 0,
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                self.stack.pop();
            }
            Event::Scalar(value, style, anchor, _) => {
                if let Some(frame) = self.stack.last_mut()
                    && frame.mapping
                    && frame.slot == Slot::Key
                {
                    frame.slot = Slot::Value(Some(value));
                    return;
                }
                let Some(pointer) = self.next() else {
                    return;
                };
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
                if let Some(pointer) = self.next()
                    && let Some(target) = self.anchors.get(&anchor)
                {
                    self.aliases.insert(pointer, target.clone());
                }
            }
            _ => {}
        }
    }
}
pub(super) fn parse(text: &Lines<'_>) -> Result<BTreeMap<String, Location>, String> {
    Ok(parse_aliased(text)?.0)
}
/// Locations, and each alias's pointer mapped to the pointer of the node it
/// refers to. Only managers that edit an anchor once for all its aliases
/// resolve them; the rest leave aliased scalars unlocated.
pub(super) fn parse_aliased(
    text: &Lines<'_>,
) -> Result<(BTreeMap<String, Location>, Aliases), String> {
    let mut receiver = Receiver {
        text,
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
    /// Whether the value at a pointer is reached through an alias, or sits
    /// inside an anchored node that aliases reuse.
    pub(super) fn shared(&self, pointer: &str) -> bool {
        self.resolve(pointer) != pointer
            || self.0.values().any(|target| {
                pointer
                    .strip_prefix(target.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
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
            let locations = super::parse(&crate::Lines::new(text)).unwrap();
            let b = &locations["/0/b"];
            assert_eq!(&text[b.start..b.end], "c", "{text:?}");
        }
    }
    /// A complex or alias key names no pointer, and the keys after it keep
    /// their own values.
    #[test]
    fn keys_after_a_complex_or_alias_key_keep_their_values() {
        for text in [
            "versions:\n  ? [a, b]\n  : 1.0\n  jackson: 2.15.0\n",
            "versions:\n  ? {a: b}\n  : 1.0\n  jackson: 2.15.0\n",
            "base: &k key\nversions:\n  *k : 1.0\n  jackson: 2.15.0\n",
        ] {
            let locations = super::parse(&crate::Lines::new(text)).unwrap();
            let jackson = &locations["/0/versions/jackson"];
            assert_eq!(&text[jackson.start..jackson.end], "2.15.0", "{text:?}");
            assert!(
                !locations
                    .keys()
                    .any(|k| k.contains("1.0") || k.ends_with("/a")),
                "{text:?}: {locations:?}"
            );
        }
    }
}
