//! Event paths prevent identical scalars elsewhere in a document from stealing
//! a location. Parser character marks are translated to UTF-8 byte offsets.
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
    bytes: Vec<usize>,
    doc: usize,
    stack: Vec<Frame>,
    locations: BTreeMap<String, Location>,
}
impl Receiver<'_> {
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
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                let pointer = self.next();
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
            Event::Scalar(value, style, ..) => {
                if let Some(frame) = self.stack.last_mut()
                    && frame.mapping
                    && frame.key.is_none()
                {
                    frame.key = Some(value);
                    return;
                }
                let pointer = self.next();
                if let Some(start) = self.bytes.get(mark.index()).copied()
                    && let Some(end) = self.end(start, style, &value)
                {
                    self.locations.insert(
                        pointer.clone(),
                        Location::new(self.text, start, end, pointer),
                    );
                }
            }
            Event::Alias(_) => {
                self.next();
            }
            _ => {}
        }
    }
}
pub(super) fn parse(text: &str) -> Result<BTreeMap<String, Location>, String> {
    let mut receiver = Receiver {
        text,
        bytes: text
            .char_indices()
            .map(|(n, _)| n)
            .chain(std::iter::once(text.len()))
            .collect(),
        doc: 0,
        stack: Vec::new(),
        locations: BTreeMap::new(),
    };
    Parser::new_from_str(text)
        .load(&mut receiver, true)
        .map_err(|e| e.to_string())?;
    Ok(receiver.locations)
}
