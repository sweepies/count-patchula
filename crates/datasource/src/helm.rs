//! A Helm repository's `index.yaml`, read as it streams: yaml-rust2's event
//! parser pulls the document's characters out of the body, and a walk keeps
//! only the fields each chart entry uses — never the whole index. The walk
//! mirrors how `serde_yaml_ng` would deserialize the same structure: plain
//! scalars are resolved under its schema, aliases replay their anchored
//! nodes where serde would follow them, and anything it would reject — bad
//! UTF-8, NUL, too much nesting in a kept value — is a document error.
use crate::{Ecosystem, HelmChart, Version, sort, upstream};
use patchula_http::{CacheError, Derived};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    io::Read,
    rc::Rc,
};
use yaml_rust2::{
    parser::{Event, Parser, Tag},
    scanner::{Marker, TScalarStyle},
};

/// The document's characters, decoded as they are read: UTF-8 only, the way
/// serde_yaml_ng's parser reads its input — UTF-16 and other encodings fail
/// as undecodable bytes. A U+FEFF at the start of a line reads as a blank,
/// the byte-order mark libyaml skips between tokens occupying its column.
/// An io or decoding error poisons the rest of the stream.
struct Chars<'a> {
    body: std::io::BufReader<&'a mut dyn Read>,
    /// The next character starts a line: libyaml skips one U+FEFF there.
    line_start: bool,
    /// Every character emitted so far, by index — the walk looks back
    /// through it to tell an implicit empty node from a `?`-marked one.
    seen: Rc<RefCell<Vec<char>>>,
    failed: Rc<Cell<bool>>,
}
impl Iterator for Chars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        if self.failed.get() {
            return None;
        }
        match self.utf8() {
            // libyaml's scanner skips a BOM found at column zero, but the
            // character still occupies the column: a line that opens with a
            // BOM indents one step past where it would start without one.
            // Emitting a blank reproduces that column accounting while the
            // scanner skips it like any other whitespace.
            Some('\u{FEFF}') if self.line_start => {
                self.seen.borrow_mut().push(' ');
                self.line_start = false;
                Some(' ')
            }
            Some(c) => {
                self.seen.borrow_mut().push(c);
                self.line_start = matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
                Some(c)
            }
            None => None,
        }
    }
}
impl Chars<'_> {
    fn byte(&mut self) -> Option<u8> {
        let mut byte = [0];
        match self.body.read(&mut byte) {
            Ok(1) => Some(byte[0]),
            Ok(_) => None,
            Err(_) => {
                self.failed.set(true);
                None
            }
        }
    }
    /// One UTF-8 character, or failure: libyaml rejects NUL and the other
    /// C0/C1 controls, DEL, and the U+FFFE/U+FFFF noncharacters, where
    /// yaml-rust2 would read them as content or end-of-input.
    fn utf8(&mut self) -> Option<char> {
        let lead = self.byte()?;
        let length = match lead {
            0x00..=0x7F => {
                if !matches!(lead, 0x09 | 0x0A | 0x0D | 0x20..=0x7E) {
                    self.failed.set(true);
                    return None;
                }
                return Some(lead as char);
            }
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => {
                self.failed.set(true);
                return None;
            }
        };
        let mut bytes = vec![lead];
        for _ in 1..length {
            match self.byte() {
                Some(byte) => bytes.push(byte),
                None => {
                    self.failed.set(true);
                    return None;
                }
            }
        }
        match std::str::from_utf8(&bytes) {
            Ok(text) => match text.chars().next() {
                Some('\u{86}'..='\u{9F}' | '\u{FFFE}' | '\u{FFFF}') => {
                    self.failed.set(true);
                    None
                }
                some => some,
            },
            Err(_) => {
                self.failed.set(true);
                None
            }
        }
    }
}

/// The tag serde_yaml_ng sees for one of yaml-rust2's: `!!x` resolves to the
/// yaml.org tag, `!x` stays local, and an explicit `%TAG` handle keeps its
/// resolved form — which never starts `!`.
fn serde_tag(tag: &Tag) -> String {
    match tag.handle.as_str() {
        "!!" => format!("tag:yaml.org,2002:{}", tag.suffix),
        "!" => format!("!{}", tag.suffix),
        handle => format!("{handle}{}", tag.suffix),
    }
}

/// A document's node as the walk consumes it: the structure serde would
/// see, resolved scalars excepted. Anchored nodes are kept whole so an
/// alias can replay them — aliases inside them stay aliases, to be
/// resolved when the replay reaches them.
#[derive(Clone)]
enum Node {
    Scalar(String, TScalarStyle, Option<String>),
    Seq(Option<String>, Vec<Node>),
    Map(Option<String>, Vec<(Node, Node)>),
    Alias(usize),
}

/// What an event is to the walk: aliases stay aliases until a position
/// that deserializes them asks for their node — serde's `ignore_any` just
/// steps over them.
enum Ev {
    Scalar(String, TScalarStyle, Option<String>),
    /// An empty plain scalar no `?` stands for — libyaml makes these in
    /// value positions (`key:` is `key: null`), and errors where one turns
    /// up as a key.
    EmptyScalar,
    SeqStart(Option<String>),
    SeqEnd,
    MapStart(Option<String>),
    MapEnd,
    Alias(usize),
    DocumentStart,
    DocumentEnd,
    StreamStart,
    StreamEnd,
}

/// Events still being recorded under an anchor: unanchored children build
/// the same tree.
enum Partial {
    Seq(Option<String>, Vec<Node>),
    Map(Option<String>, Vec<(Node, Node)>, Option<Node>),
}
struct Recording {
    anchor: usize,
    stack: Vec<Partial>,
}
impl Recording {
    /// Feeds one event into the tree; a completed recording reports its
    /// anchor and node. An implicit empty scalar where the map wants a key
    /// is a libyaml parse error.
    fn feed(&mut self, event: &Ev) -> Result<Option<(usize, Node)>, ()> {
        let built = match event {
            Ev::Scalar(text, style, tag) => Node::Scalar(text.clone(), *style, tag.clone()),
            Ev::EmptyScalar => {
                if matches!(self.stack.last(), Some(Partial::Map(_, _, None))) {
                    return Err(());
                }
                Node::Scalar(String::new(), TScalarStyle::Plain, None)
            }
            Ev::SeqStart(tag) => {
                self.stack.push(Partial::Seq(tag.clone(), Vec::new()));
                return Ok(None);
            }
            Ev::MapStart(tag) => {
                self.stack.push(Partial::Map(tag.clone(), Vec::new(), None));
                return Ok(None);
            }
            Ev::SeqEnd => match self.stack.pop() {
                Some(Partial::Seq(tag, items)) => Node::Seq(tag, items),
                _ => return Ok(None),
            },
            Ev::MapEnd => match self.stack.pop() {
                Some(Partial::Map(tag, pairs, _)) => Node::Map(tag, pairs),
                _ => return Ok(None),
            },
            Ev::Alias(anchor) => Node::Alias(*anchor),
            _ => return Ok(None),
        };
        match self.stack.last_mut() {
            Some(Partial::Seq(_, items)) => items.push(built),
            Some(Partial::Map(_, _, key @ None)) => *key = Some(built),
            Some(Partial::Map(_, pairs, key)) => {
                if let Some(key) = key.take() {
                    pairs.push((key, built));
                }
            }
            None => return Ok(Some((self.anchor, built))),
        }
        Ok(None)
    }
}

/// The pull side of the walk: parser events in order, with anchored nodes
/// recorded as they close and aliases resolved where serde's deserializer
/// would follow them.
struct Walk<'a> {
    parser: Parser<Chars<'a>>,
    pending: VecDeque<Ev>,
    recording: Vec<Recording>,
    anchors: BTreeMap<usize, Node>,
    /// Parser events pulled so far; caps alias jumps like serde's
    /// `jumpcount`, which fails past a hundred jumps per event.
    events: usize,
    jumps: usize,
    /// The characters the parser has read since `base`: only the previous
    /// token's wake is ever consulted, so the buffer stays a token wide.
    seen: Rc<RefCell<Vec<char>>>,
    /// Where `seen` starts in the parser's stream.
    base: usize,
    failed: Rc<Cell<bool>>,
    /// Open flow collections the scanner is inside: `{` and `[` count, a
    /// block collection does not. Needed where libyaml's plain-scalar rules
    /// differ inside flow — a `:` that precedes a flow indicator there is a
    /// scan error, where yaml-rust2 happily splits a key off.
    flow: usize,
    /// Start of the previous real parser token in `seen`, and whether it was
    /// a plain scalar — a `:` directly in its wake is part of its scan.
    previous: (usize, bool),
}
impl<'a> Walk<'a> {
    fn new(body: &'a mut dyn Read) -> Self {
        let failed = Rc::new(Cell::new(false));
        let seen = Rc::new(RefCell::new(Vec::new()));
        Self {
            parser: Parser::new(Chars {
                body: std::io::BufReader::new(body),
                line_start: true,
                seen: seen.clone(),
                failed: failed.clone(),
            }),
            pending: VecDeque::new(),
            recording: Vec::new(),
            anchors: BTreeMap::new(),
            events: 0,
            jumps: 0,
            seen,
            base: 0,
            failed,
            flow: 0,
            previous: (0, false),
        }
    }
    /// Records the event in every open anchored tree.
    fn record(&mut self, event: &Ev) -> Result<(), ()> {
        let mut done = Vec::new();
        for recording in &mut self.recording {
            if let Some(built) = recording.feed(event)? {
                done.push(built);
            }
        }
        self.recording
            .retain(|recording| !recording.stack.is_empty());
        for (anchor, node) in done {
            self.anchors.insert(anchor, node);
        }
        Ok(())
    }
    /// Scans the wake of a plain scalar the way libyaml continues one:
    /// buffered whitespace and breaks keep it going until a token character
    /// ends it, a `:` before a blank or break makes a value of it, and a `:`
    /// before `, ? [ ] { }` fails the scan. Returns true on that failure.
    fn colon_in_flow(&self, from: usize) -> bool {
        let seen = self.seen.borrow();
        let mut i = from.saturating_sub(self.base);
        while i < seen.len() {
            match seen[i] {
                ':' => match seen.get(i + 1) {
                    Some(',' | '?' | '[' | ']' | '{' | '}') => return true,
                    Some(' ' | '\t' | '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}') | None => {
                        return false;
                    }
                    _ => i += 1,
                },
                ' ' | '\t' | '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                    i += 1;
                    if let Some(c) = seen.get(i)
                        && matches!(
                            c,
                            '&' | '*'
                                | '!'
                                | '\''
                                | '"'
                                | '#'
                                | '?'
                                | ','
                                | '['
                                | ']'
                                | '{'
                                | '}'
                                | '|'
                                | '>'
                        )
                    {
                        return false;
                    }
                }
                _ => i += 1,
            }
        }
        false
    }
    /// Whether the empty node at `mark` followed a `?` — the only empty key
    /// libyaml accepts.
    fn explicit_empty(&self, mark: &Marker) -> bool {
        let seen = self.seen.borrow();
        let to = mark.index().saturating_sub(self.base).min(seen.len());
        seen[..to]
            .iter()
            .rev()
            .find(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
            == Some(&'?')
    }
    /// The events a kept node replays.
    fn replay(node: &Node, into: &mut VecDeque<Ev>) {
        match node {
            Node::Scalar(text, style, tag) => {
                into.push_back(Ev::Scalar(text.clone(), *style, tag.clone()))
            }
            Node::Alias(anchor) => into.push_back(Ev::Alias(*anchor)),
            Node::Seq(tag, items) => {
                into.push_back(Ev::SeqStart(tag.clone()));
                for item in items {
                    Self::replay(item, into);
                }
                into.push_back(Ev::SeqEnd);
            }
            Node::Map(tag, pairs) => {
                into.push_back(Ev::MapStart(tag.clone()));
                for (key, value) in pairs {
                    Self::replay(key, into);
                    Self::replay(value, into);
                }
                into.push_back(Ev::MapEnd);
            }
        }
    }
    /// The next event, or a document error: parse failures, plain scalars
    /// libyaml could not have scanned, and end of stream alike.
    fn take(&mut self) -> Result<Ev, ()> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(event);
        }
        if self.failed.get() {
            return Err(());
        }
        let (event, mark) = self.parser.next_token().map_err(|_| ())?;
        self.events += 1;
        if self.failed.get() {
            return Err(());
        }
        // A `:` the scanner met inside a flow collection while continuing a
        // plain scalar is a libyaml error when a flow indicator follows it
        // (`{a:{`, `{a:,`, `{a:?`): yaml-rust2 emits key and value instead.
        if self.flow > 0 && self.previous.1 && self.colon_in_flow(self.previous.0) {
            return Err(());
        }
        // Nothing before the earlier of the last two tokens' starts is read
        // again — a token's mark can point back before the previous one's.
        let low = self.previous.0.min(mark.index());
        {
            let mut seen = self.seen.borrow_mut();
            // Marks run past the buffer's end at stream end, where the
            // parser consumes sentinels `seen` never holds.
            let cut = low.saturating_sub(self.base).min(seen.len());
            if cut > 0 {
                seen.drain(..cut);
            }
            self.base += cut;
        }
        // Count the collections the character stream itself opens and
        // closes, so `flow` follows the scanner rather than the events.
        match self
            .seen
            .borrow()
            .get(mark.index().saturating_sub(self.base))
        {
            Some('{' | '[') => self.flow += 1,
            Some('}' | ']') => self.flow = self.flow.saturating_sub(1),
            _ => {}
        }
        self.previous = (
            mark.index(),
            matches!(&event, Event::Scalar(text, TScalarStyle::Plain, _, _) if !text.is_empty()),
        );
        let event = match event {
            Event::Scalar(text, style, anchor, tag) => {
                // A plain scalar can never open with `|` or `>`: in block
                // context those are block scalar headers, and inside flow
                // collections they fail the scan outright — yaml-rust2
                // alone swallows them into a plain scalar.
                if style == TScalarStyle::Plain && text.starts_with(['|', '>']) {
                    return Err(());
                }
                let tag = tag.as_ref().map(serde_tag);
                if anchor != 0 {
                    self.anchors
                        .insert(anchor, Node::Scalar(text.clone(), style, tag.clone()));
                }
                if style == TScalarStyle::Plain
                    && text.is_empty()
                    && tag.is_none()
                    && !self.explicit_empty(&mark)
                {
                    Ev::EmptyScalar
                } else {
                    Ev::Scalar(text, style, tag)
                }
            }
            Event::SequenceStart(anchor, tag) => {
                let event = Ev::SeqStart(tag.as_ref().map(serde_tag));
                if anchor != 0 {
                    // The start belongs to any anchored tree still open
                    // before the anchor's own recording begins.
                    self.record(&event)?;
                    self.recording.push(Recording {
                        anchor,
                        stack: vec![Partial::Seq(
                            match &event {
                                Ev::SeqStart(tag) => tag.clone(),
                                _ => None,
                            },
                            Vec::new(),
                        )],
                    });
                    return Ok(event);
                }
                event
            }
            Event::MappingStart(anchor, tag) => {
                let event = Ev::MapStart(tag.as_ref().map(serde_tag));
                if anchor != 0 {
                    self.record(&event)?;
                    self.recording.push(Recording {
                        anchor,
                        stack: vec![Partial::Map(
                            match &event {
                                Ev::MapStart(tag) => tag.clone(),
                                _ => None,
                            },
                            Vec::new(),
                            None,
                        )],
                    });
                    return Ok(event);
                }
                event
            }
            Event::SequenceEnd => Ev::SeqEnd,
            Event::MappingEnd => Ev::MapEnd,
            Event::Alias(id) => Ev::Alias(id),
            Event::DocumentStart => Ev::DocumentStart,
            Event::DocumentEnd => Ev::DocumentEnd,
            Event::StreamStart => Ev::StreamStart,
            Event::StreamEnd => Ev::StreamEnd,
            Event::Nothing => return Err(()),
        };
        self.record(&event)?;
        Ok(event)
    }
    /// The node an alias event names, replayed into `pending`: serde's
    /// jump to the anchor's events. An anchor still recording — an alias
    /// to the node being read — is serde's repetition limit.
    fn resolve(&mut self, event: Ev) -> Result<Ev, ()> {
        let Ev::Alias(id) = event else {
            return Ok(event);
        };
        self.jumps += 1;
        if self.jumps > self.events * 100 {
            return Err(());
        }
        let Some(node) = self.anchors.get(&id).cloned() else {
            return Err(());
        };
        Self::replay(&node, &mut self.pending);
        let next = self.take()?;
        self.resolve(next)
    }
    /// One whole value, consumed without deserializing it: serde's
    /// `ignore_any`, which steps over aliases unresolved and never counts
    /// depth.
    fn skip(&mut self, first: Ev) -> Result<(), ()> {
        // For each open collection, whether the next event lands on a map
        // key: an implicit empty scalar does not make a key.
        let mut open: Vec<Option<bool>> = Vec::new();
        match first {
            Ev::SeqStart(_) => open.push(None),
            Ev::MapStart(_) => open.push(Some(true)),
            Ev::Scalar(..) | Ev::Alias(_) | Ev::EmptyScalar => return Ok(()),
            _ => return Err(()),
        }
        while let Some(key) = open.last_mut() {
            match self.take()? {
                Ev::EmptyScalar if *key == Some(true) => return Err(()),
                Ev::SeqStart(_) => open.push(None),
                Ev::MapStart(_) => open.push(Some(true)),
                Ev::SeqEnd | Ev::MapEnd => {
                    open.pop();
                }
                _ => {
                    if let Some(key) = key.as_mut() {
                        *key = !*key;
                    }
                }
            }
        }
        Ok(())
    }
    /// The node starting with `first`, read to its end. What serde reads of
    /// a field or item; `depth` is serde's remaining recursion allowance.
    fn node(&mut self, first: Ev, depth: usize) -> Result<Node, ()> {
        match self.resolve(first)? {
            Ev::Scalar(text, style, tag) => Ok(Node::Scalar(text, style, tag)),
            Ev::EmptyScalar => Ok(Node::Scalar(String::new(), TScalarStyle::Plain, None)),
            Ev::SeqStart(tag) => {
                if depth == 0 {
                    return Err(());
                }
                let mut items = Vec::new();
                loop {
                    match self.take()? {
                        Ev::SeqEnd => break,
                        event => items.push(self.node(event, depth - 1)?),
                    }
                }
                Ok(Node::Seq(tag, items))
            }
            Ev::MapStart(tag) => {
                if depth == 0 {
                    return Err(());
                }
                let mut pairs = Vec::new();
                loop {
                    match self.take()? {
                        Ev::MapEnd => break,
                        Ev::EmptyScalar => return Err(()),
                        event => {
                            let key = self.node(event, depth - 1)?;
                            let first = self.take()?;
                            let value = self.node(first, depth - 1)?;
                            pairs.push((key, value));
                        }
                    }
                }
                Ok(Node::Map(tag, pairs))
            }
            _ => Err(()),
        }
    }
}

/// serde's `parse_null`: the plain scalars that mean no value.
fn null(text: &str) -> bool {
    matches!(text, "null" | "Null" | "NULL" | "~")
}
/// serde's `parse_bool`: the booleans serde_yaml_ng accepts.
fn boolean(text: &str) -> Option<bool> {
    match text {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}
/// serde's `digits_but_not_number`: leading zeroes keep a scalar a string.
fn digits_but_not_number(scalar: &str) -> bool {
    let scalar = scalar.strip_prefix(['-', '+']).unwrap_or(scalar);
    scalar.len() > 1 && scalar.starts_with('0') && scalar[1..].bytes().all(|b| b.is_ascii_digit())
}
/// serde's `parse_unsigned_int`: decimal, `0x`, `0o` and `0b`, `+` allowed.
/// `u128` is width enough — serde tries the narrower widths first, but any
/// one of them parsing makes the scalar an integer.
fn unsigned_int(scalar: &str) -> Option<u128> {
    let unpositive = scalar.strip_prefix('+').unwrap_or(scalar);
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = unpositive.strip_prefix(prefix)
            && !rest.starts_with(['+', '-'])
            && let Ok(int) = u128::from_str_radix(rest, radix)
        {
            return Some(int);
        }
    }
    if unpositive.starts_with(['+', '-']) || digits_but_not_number(scalar) {
        return None;
    }
    unpositive.parse::<u128>().ok()
}
/// serde's `parse_negative_int`.
fn negative_int(scalar: &str) -> Option<i128> {
    for (prefix, radix) in [("-0x", 16), ("-0o", 8), ("-0b", 2)] {
        if let Some(rest) = scalar.strip_prefix(prefix)
            && let Ok(int) = i128::from_str_radix(&format!("-{rest}"), radix)
        {
            return Some(int);
        }
    }
    if digits_but_not_number(scalar) {
        return None;
    }
    scalar.parse::<i128>().ok()
}
/// A `String` field's node: `deserialize_str` reads any scalar's text,
/// tags included.
fn string_value(node: &Node) -> Result<String, ()> {
    match node {
        Node::Scalar(text, _, _) => Ok(text.clone()),
        _ => Err(()),
    }
}

/// A resolved `serde_yaml_ng::Value`, reduced to what the derive reads: the
/// `as_str`, `as_sequence` and `get` semantics for the metadata it keeps,
/// and enough of the resolved form to compare keys for duplicates the way
/// `serde_yaml_ng::Mapping` does.
enum Yaml {
    Null,
    Bool(bool),
    PosInt(u64),
    NegInt(i64),
    Float(f64),
    Str(String),
    Tagged(String, Box<Yaml>),
    Seq(Vec<Yaml>),
    Map(Vec<(Yaml, Yaml)>),
}
impl Yaml {
    fn untag(&self) -> &Self {
        let mut node = self;
        while let Yaml::Tagged(_, inner) = node {
            node = inner;
        }
        node
    }
    fn as_str(&self) -> Option<&str> {
        match self.untag() {
            Yaml::Str(text) => Some(text),
            _ => None,
        }
    }
    fn as_sequence(&self) -> Option<&[Yaml]> {
        match self.untag() {
            Yaml::Seq(items) => Some(items),
            _ => None,
        }
    }
    /// serde's `Value::get`: the last pair whose plain-string key matches.
    fn get<'a>(&'a self, key: &str) -> Option<&'a Yaml> {
        let Yaml::Map(pairs) = self.untag() else {
            return None;
        };
        pairs.iter().rev().find_map(|(name, value)| {
            matches!(name, Yaml::Str(text) if text == key).then_some(value)
        })
    }
}
/// `serde_yaml_ng::Value`'s equality, for the duplicate key check its
/// `Mapping` deserialization makes: null keys, booleans, the number
/// representations — `.nan` equals itself there — tagged values by tag and
/// contents, and collections structurally, mappings order-free.
fn values_equal(left: &Yaml, right: &Yaml) -> bool {
    match (left, right) {
        (Yaml::Null, Yaml::Null) => true,
        (Yaml::Bool(a), Yaml::Bool(b)) => a == b,
        (Yaml::PosInt(a), Yaml::PosInt(b)) => a == b,
        (Yaml::NegInt(a), Yaml::NegInt(b)) => a == b,
        (Yaml::Float(a), Yaml::Float(b)) => a == b || (a.is_nan() && b.is_nan()),
        (Yaml::Str(a), Yaml::Str(b)) => a == b,
        (Yaml::Tagged(at, a), Yaml::Tagged(bt, b)) => at == bt && values_equal(a, b),
        (Yaml::Seq(a), Yaml::Seq(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| values_equal(a, b))
        }
        (Yaml::Map(a), Yaml::Map(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .any(|(k, v)| values_equal(key, k) && values_equal(value, v))
                })
        }
        _ => false,
    }
}

/// serde's `visit_int` on a `Value`: u64 and i64 become numbers; the wider
/// u128/i128 visits have no `Value` to receive them and fail the document.
fn value_int(scalar: &str) -> Result<Yaml, ()> {
    if let Some(int) = unsigned_int64(scalar) {
        return Ok(Yaml::PosInt(int));
    }
    if let Some(int) = negative_int64(scalar) {
        // serde's `i64` to `Number`: only an actual negative is a `NegInt`,
        // so `-0` equals `0`.
        return Ok(if int < 0 {
            Yaml::NegInt(int)
        } else {
            Yaml::PosInt(int as u64)
        });
    }
    Err(())
}
/// serde's `parse_unsigned_int` against u64 only.
fn unsigned_int64(scalar: &str) -> Option<u64> {
    let unpositive = scalar.strip_prefix('+').unwrap_or(scalar);
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = unpositive.strip_prefix(prefix)
            && !rest.starts_with(['+', '-'])
            && let Ok(int) = u64::from_str_radix(rest, radix)
        {
            return Some(int);
        }
    }
    if unpositive.starts_with(['+', '-']) || digits_but_not_number(scalar) {
        return None;
    }
    unpositive.parse::<u64>().ok()
}
/// serde's `parse_negative_int` against i64 only.
fn negative_int64(scalar: &str) -> Option<i64> {
    for (prefix, radix) in [("-0x", 16), ("-0o", 8), ("-0b", 2)] {
        if let Some(rest) = scalar.strip_prefix(prefix)
            && let Ok(int) = i64::from_str_radix(&format!("-{rest}"), radix)
        {
            return Some(int);
        }
    }
    if digits_but_not_number(scalar) {
        return None;
    }
    scalar.parse::<i64>().ok()
}
/// serde's `parse_f64`, returning the value.
fn float_value(scalar: &str) -> Option<f64> {
    let unpositive = match scalar.strip_prefix('+') {
        Some(unpositive) if !unpositive.starts_with(['+', '-']) => unpositive,
        Some(_) => return None,
        None => scalar,
    };
    match unpositive {
        ".inf" | ".Inf" | ".INF" => Some(f64::INFINITY),
        _ => match scalar {
            "-.inf" | "-.Inf" | "-.INF" => Some(f64::NEG_INFINITY),
            ".nan" | ".NaN" | ".NAN" => Some(f64::NAN),
            _ => unpositive.parse::<f64>().ok().filter(|f| f.is_finite()),
        },
    }
}

/// A scalar resolved the way `deserialize_any` — a `Value` — resolves it:
/// core tags type or fail the document, a local `!` tag wraps the resolved
/// node, other tags make a string, and an untagged scalar follows the
/// schema's resolution.
fn scalar_value(text: String, style: TScalarStyle, tag: Option<String>) -> Result<Yaml, ()> {
    if let Some(tag) = tag {
        if tag.starts_with('!') {
            let inner = scalar_value(text, style, None)?;
            return Ok(Yaml::Tagged(tag, Box::new(inner)));
        }
        return match tag.as_str() {
            "tag:yaml.org,2002:bool" => boolean(&text).map(Yaml::Bool).ok_or(()),
            "tag:yaml.org,2002:int" => value_int(&text),
            "tag:yaml.org,2002:null" => {
                if null(&text) {
                    Ok(Yaml::Null)
                } else {
                    Err(())
                }
            }
            "tag:yaml.org,2002:float" => float_value(&text).map(Yaml::Float).ok_or(()),
            _ => Ok(Yaml::Str(text)),
        };
    }
    if style != TScalarStyle::Plain {
        return Ok(Yaml::Str(text));
    }
    if text.is_empty() || null(&text) {
        return Ok(Yaml::Null);
    }
    if let Some(b) = boolean(&text) {
        return Ok(Yaml::Bool(b));
    }
    if let Ok(value) = value_int(&text) {
        return Ok(value);
    }
    if unsigned_int(&text).is_some() || negative_int(&text).is_some() {
        // Bigger than i64/u64 but an integer: serde's u128/i128 visit has no
        // `Value` to land in.
        return Err(());
    }
    if !digits_but_not_number(&text)
        && let Some(float) = float_value(&text)
    {
        return Ok(Yaml::Float(float));
    }
    Ok(Yaml::Str(text))
}

/// A node as a `Value`: serde's tag and schema resolution applied, and its
/// `Mapping` deserialization's duplicate key check — a second pair whose
/// key equals an earlier one, resolved values compared — fails the
/// document.
fn yaml(node: &Node) -> Result<Yaml, ()> {
    Ok(match node {
        Node::Scalar(text, style, tag) => scalar_value(text.clone(), *style, tag.clone())?,
        Node::Alias(_) => return Err(()),
        Node::Seq(tag, items) => {
            let seq = Yaml::Seq(items.iter().map(yaml).collect::<Result<_, _>>()?);
            match tag {
                Some(tag) if tag.starts_with('!') => Yaml::Tagged(tag.clone(), Box::new(seq)),
                _ => seq,
            }
        }
        Node::Map(tag, pairs) => {
            let mut resolved: Vec<(Yaml, Yaml)> = Vec::with_capacity(pairs.len());
            for (key, value) in pairs {
                let key = yaml(key)?;
                if resolved
                    .iter()
                    .any(|(earlier, _)| values_equal(earlier, &key))
                {
                    return Err(());
                }
                resolved.push((key, yaml(value)?));
            }
            let map = Yaml::Map(resolved);
            match tag {
                Some(tag) if tag.starts_with('!') => Yaml::Tagged(tag.clone(), Box::new(map)),
                _ => map,
            }
        }
    })
}

/// The node `Option<String>` reads: a plain null-looking scalar is None —
/// and `!!null` on anything but a null literal is an error — any other
/// scalar is its text, anything else fails the document.
fn optional_string(node: &Node) -> Result<Option<String>, ()> {
    match node {
        Node::Scalar(text, style, tag) => {
            if *style == TScalarStyle::Plain {
                match tag.as_deref() {
                    Some("tag:yaml.org,2002:null") => {
                        return if null(text) { Ok(None) } else { Err(()) };
                    }
                    Some(_) => {}
                    None if text.is_empty() || null(text) => return Ok(None),
                    None => {}
                }
            }
            Ok(Some(text.clone()))
        }
        _ => Err(()),
    }
}
/// The node `Option<serde_yaml_ng::Value>` reads, resolved like a `Value`.
fn optional_value(node: &Node) -> Result<Option<Yaml>, ()> {
    if let Node::Scalar(text, style, tag) = node
        && *style == TScalarStyle::Plain
    {
        match tag.as_deref() {
            Some("tag:yaml.org,2002:null") => {
                return if null(text) { Ok(None) } else { Err(()) };
            }
            Some(_) => return yaml(node).map(Some),
            None if text.is_empty() || null(text) => return Ok(None),
            None => {}
        }
    }
    yaml(node).map(Some)
}

/// One chart entry's fields, the names the derive keeps.
#[derive(Default)]
struct ChartFields {
    version: Option<String>,
    created: Option<String>,
    digest: Option<String>,
    deprecated: bool,
    sources: Option<Yaml>,
    annotations: Option<Yaml>,
}
/// The pairs a `Node` holds as a mapping, or as a sequence, where serde's
/// empty plain scalar reads as an empty collection.
fn pairs_of(node: &Node) -> Result<&[(Node, Node)], ()> {
    match node {
        Node::Map(_, pairs) => Ok(pairs),
        Node::Scalar(text, TScalarStyle::Plain, _) if text.is_empty() => Ok(&[]),
        _ => Err(()),
    }
}
fn items_of(node: &Node) -> Result<&[Node], ()> {
    match node {
        Node::Seq(_, items) => Ok(items),
        Node::Scalar(text, TScalarStyle::Plain, _) if text.is_empty() => Ok(&[]),
        _ => Err(()),
    }
}
/// The node a `String` map key reads: any scalar's text — an empty plain
/// scalar here can only have come from a `?`, where `""` is a real key.
fn string_key(node: &Node) -> Result<&str, ()> {
    match node {
        Node::Scalar(name, _, _) => Ok(name),
        _ => Err(()),
    }
}

/// The fields of one chart item, read from its events: known field names
/// deserialize their values, the rest are ignored the way serde's
/// `IgnoredAny` steps over them.
fn chart_item_fields(walk: &mut Walk, fields: &mut ChartFields) -> Result<(), ()> {
    let mut seen = 0u8;
    loop {
        match walk.take()? {
            Ev::MapEnd => break,
            Ev::EmptyScalar => return Err(()),
            event => {
                let key = walk.node(event, 124)?;
                let Node::Scalar(name, ..) = &key else {
                    return Err(());
                };
                let name = name.as_str();
                let value = walk.take()?;
                match name {
                    "version" => {
                        fields.version = Some(string_value(&walk.node(value, 124)?)?);
                        if seen & 1 != 0 {
                            return Err(());
                        }
                        seen |= 1;
                    }
                    "created" => {
                        fields.created = optional_string(&walk.node(value, 124)?)?;
                        if seen & 2 != 0 {
                            return Err(());
                        }
                        seen |= 2;
                    }
                    "digest" => {
                        fields.digest = optional_string(&walk.node(value, 124)?)?;
                        if seen & 4 != 0 {
                            return Err(());
                        }
                        seen |= 4;
                    }
                    "deprecated" => {
                        let Node::Scalar(text, style, tag) = walk.node(value, 124)? else {
                            return Err(());
                        };
                        if style != TScalarStyle::Plain
                            && !(style == TScalarStyle::Literal
                                && tag.as_deref() == Some("tag:yaml.org,2002:bool"))
                        {
                            return Err(());
                        }
                        fields.deprecated = boolean(&text).ok_or(())?;
                        if seen & 8 != 0 {
                            return Err(());
                        }
                        seen |= 8;
                    }
                    "sources" => {
                        fields.sources = optional_value(&walk.node(value, 124)?)?;
                        if seen & 16 != 0 {
                            return Err(());
                        }
                        seen |= 16;
                    }
                    "annotations" => {
                        fields.annotations = optional_value(&walk.node(value, 124)?)?;
                        if seen & 32 != 0 {
                            return Err(());
                        }
                        seen |= 32;
                    }
                    _ => walk.skip(value)?,
                }
            }
        }
    }
    Ok(())
}

/// The fields a chart item's map yields; serde's `Chart` struct rejects a
/// second occurrence of any field it knows and a missing `version`.
fn chart_item(walk: &mut Walk, first: Ev, depth: usize) -> Result<ChartFields, ()> {
    let mut fields = ChartFields::default();
    match first {
        Ev::MapStart(_) => chart_item_fields(walk, &mut fields)?,
        // A non-mapping item — an alias, a scalar, a sequence — is what
        // `Chart`'s `deserialize_map` makes of it.
        _ => fields = chart_item_nodes(&walk.node(first, depth)?)?,
    }
    if fields.version.is_none() {
        return Err(());
    }
    Ok(fields)
}

/// A chart name's version list as serde's `Vec<Chart>` reads it: a
/// sequence streams its items, anything else is what a sequence
/// deserializer makes of it — an empty plain scalar is an empty list.
fn chart_versions(walk: &mut Walk, first: Ev, depth: usize) -> Result<Vec<ChartFields>, ()> {
    let mut versions = Vec::new();
    match first {
        Ev::SeqStart(_) => loop {
            match walk.take()? {
                Ev::SeqEnd => break,
                item => versions.push(chart_item(walk, item, depth - 1)?),
            }
        },
        _ => {
            for item in items_of(&walk.node(first, depth)?)? {
                versions.push(chart_item_nodes(item)?);
            }
        }
    }
    Ok(versions)
}
/// The fields a whole already-read item node yields, for a sequence the
/// walk read as one node.
fn chart_item_nodes(node: &Node) -> Result<ChartFields, ()> {
    let mut fields = ChartFields::default();
    let mut seen = 0u8;
    for (key, value) in pairs_of(node)? {
        let Node::Scalar(name, ..) = key else {
            return Err(());
        };
        let bit = match name.as_str() {
            "version" => {
                fields.version = Some(string_value(value)?);
                1
            }
            "created" => {
                fields.created = optional_string(value)?;
                2
            }
            "digest" => {
                fields.digest = optional_string(value)?;
                4
            }
            "deprecated" => {
                let Node::Scalar(text, style, tag) = value else {
                    return Err(());
                };
                if *style != TScalarStyle::Plain
                    && !(*style == TScalarStyle::Literal
                        && tag.as_deref() == Some("tag:yaml.org,2002:bool"))
                {
                    return Err(());
                }
                fields.deprecated = boolean(text).ok_or(())?;
                8
            }
            "sources" => {
                fields.sources = optional_value(value)?;
                16
            }
            "annotations" => {
                fields.annotations = optional_value(value)?;
                32
            }
            _ => continue,
        };
        if seen & bit != 0 {
            return Err(());
        }
        seen |= bit;
    }
    if fields.version.is_none() {
        return Err(());
    }
    Ok(fields)
}

/// Reads the index's `entries` map as it streams: each chart name's version
/// list is consumed an entry at a time.
fn helm_index(walk: &mut Walk) -> Result<Vec<(String, Vec<ChartFields>)>, ()> {
    let mut entries = Vec::new();
    let mut seen_entries = false;
    loop {
        match walk.take()? {
            Ev::MapEnd => break,
            Ev::EmptyScalar => return Err(()),
            event => {
                let key = walk.node(event, 127)?;
                if string_key(&key)? == "entries" {
                    if seen_entries {
                        return Err(());
                    }
                    seen_entries = true;
                    match walk.take()? {
                        Ev::MapStart(_) => loop {
                            match walk.take()? {
                                Ev::MapEnd => break,
                                Ev::EmptyScalar => return Err(()),
                                event => {
                                    let key = walk.node(event, 126)?;
                                    let chart = string_key(&key)?.to_owned();
                                    let first = walk.take()?;
                                    let versions = chart_versions(walk, first, 126)?;
                                    entries.push((chart, versions));
                                }
                            }
                        },
                        event => {
                            for (key, value) in pairs_of(&walk.node(event, 127)?)? {
                                let chart = string_key(key)?.to_owned();
                                let mut versions = Vec::new();
                                for item in items_of(value)? {
                                    versions.push(chart_item_nodes(item)?);
                                }
                                entries.push((chart, versions));
                            }
                        }
                    }
                } else {
                    let first = walk.take()?;
                    walk.skip(first)?;
                }
            }
        }
    }
    if !seen_entries {
        return Err(());
    }
    Ok(entries)
}

fn helm(body: &mut dyn Read) -> std::result::Result<Vec<u8>, CacheError> {
    let mut walk = Walk::new(body);
    let entries = (|| -> Result<Vec<(String, Vec<ChartFields>)>, ()> {
        match walk.take()? {
            Ev::StreamStart => {}
            _ => return Err(()),
        }
        match walk.take()? {
            Ev::DocumentStart => {}
            _ => return Err(()),
        }
        let first = walk.take()?;
        let entries = match walk.resolve(first)? {
            Ev::MapStart(_) => helm_index(&mut walk)?,
            // A root that is not a mapping can never hold `entries`.
            _ => return Err(()),
        };
        match walk.take()? {
            Ev::DocumentEnd => {}
            _ => return Err(()),
        }
        // serde_yaml_ng rejects a stream that holds a second document.
        match walk.take()? {
            Ev::StreamEnd => {}
            _ => return Err(()),
        }
        Ok(entries)
    })()
    .map_err(|_| CacheError::Document)?;
    if walk.failed.get() {
        return Err(CacheError::Document);
    }
    let charts: BTreeMap<_, _> = entries
        .into_iter()
        .map(|(name, entries)| {
            let mut chart = HelmChart::default();
            for v in entries {
                let sources = v
                    .sources
                    .as_ref()
                    .and_then(Yaml::as_sequence)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Yaml::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let notes = upstream::ChartNotes::new(
                    sources.iter().map(String::as_str),
                    v.annotations
                        .as_ref()
                        .and_then(|a| a.get(upstream::CHANGES))
                        .and_then(Yaml::as_str),
                );
                if !notes.is_empty() {
                    chart
                        .notes
                        .insert(v.version.clone().unwrap_or_default(), notes);
                }
                chart.catalog.versions.push(Version {
                    version: v.version.unwrap_or_default(),
                    published_at: v.created,
                    digest: v.digest,
                    deprecated: v.deprecated,
                    yanked: false,
                });
            }
            sort(&mut chart.catalog.versions, Ecosystem::Npm);
            (name, chart)
        })
        .collect();
    serde_json::to_vec(&charts).map_err(|_| CacheError::Document)
}
pub(crate) const HELM_INDEX: Derived<CacheError> = Derived::new("helm-index", 3, helm);

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// origin/main's derive, verbatim: serde_yaml_ng over the buffered body.
    /// Every input the walk accepts must produce these same bytes — and
    /// anything it fails must fail identically.
    fn helm_buffered(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, CacheError> {
        #[derive(Deserialize)]
        struct Index {
            entries: BTreeMap<String, Vec<Chart>>,
        }
        #[derive(Deserialize)]
        struct Chart {
            version: String,
            created: Option<String>,
            digest: Option<String>,
            #[serde(default)]
            deprecated: bool,
            sources: Option<serde_yaml_ng::Value>,
            annotations: Option<serde_yaml_ng::Value>,
        }
        let index: Index = serde_yaml_ng::from_reader(body).map_err(|_| CacheError::Document)?;
        let charts: BTreeMap<_, _> = index
            .entries
            .into_iter()
            .map(|(name, entries)| {
                let mut chart = HelmChart::default();
                for v in entries {
                    let sources = v
                        .sources
                        .as_ref()
                        .and_then(serde_yaml_ng::Value::as_sequence);
                    let notes = upstream::ChartNotes::new(
                        sources
                            .into_iter()
                            .flatten()
                            .filter_map(serde_yaml_ng::Value::as_str),
                        v.annotations
                            .as_ref()
                            .and_then(|a| a.get(upstream::CHANGES))
                            .and_then(serde_yaml_ng::Value::as_str),
                    );
                    if !notes.is_empty() {
                        chart.notes.insert(v.version.clone(), notes);
                    }
                    chart.catalog.versions.push(Version {
                        version: v.version,
                        published_at: v.created,
                        digest: v.digest,
                        deprecated: v.deprecated,
                        yanked: false,
                    });
                }
                sort(&mut chart.catalog.versions, Ecosystem::Npm);
                (name, chart)
            })
            .collect();
        serde_json::to_vec(&charts).map_err(|_| CacheError::Document)
    }

    /// A body arriving `size` bytes at a time.
    struct Chunks<'a> {
        body: &'a [u8],
        size: usize,
        at: usize,
    }
    impl Read for Chunks<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = (self.body.len() - self.at).min(self.size).min(buffer.len());
            buffer[..read].copy_from_slice(&self.body[self.at..self.at + read]);
            self.at += read;
            Ok(read)
        }
    }

    /// The walk must answer exactly what the buffered parse does: the same
    /// bytes, or the same failure — however the body is chunked. Diverging
    /// cases are collected and reported together.
    fn check(failures: &mut Vec<String>, body: &[u8], case: &str) {
        let oracle = helm_buffered(&mut &body[..]);
        for size in [usize::MAX, 1, 3, 7, 64] {
            let mut chunks = Chunks { body, size, at: 0 };
            let walked = helm(&mut chunks);
            if oracle.as_ref().ok() != walked.as_ref().ok() {
                failures.push(format!(
                    "{case} at chunk {size}: oracle {}, walk {}\n  body: {:?}",
                    result(&oracle),
                    result(&walked),
                    String::from_utf8_lossy(body)
                ));
            }
        }
    }
    fn result(out: &Result<Vec<u8>, impl std::fmt::Display>) -> String {
        match out {
            Ok(bytes) => format!("Ok({:?})", String::from_utf8_lossy(bytes)),
            Err(error) => format!("Err({error})"),
        }
    }

    /// Every class the review found diverging, plus the verified-clean
    /// classes, as documents.
    #[test]
    fn the_walk_derives_what_the_buffered_parse_does() {
        let cases: &[&[u8]] = &[
            // A plain index, and one carrying everything the derive reads.
            b"entries: {}",
            b"apiVersion: v1\nentries:\n  chart:\n    - version: 1.2.3\n      created: '2020-01-01'\n      digest: abc\n      deprecated: true\n      sources: [https://x.test/1]\n      annotations:\n        artifacthub.io/changes: 'a change'\n    - version: 1.2.4\n      urls: [deep, ignored, values]\n",
            // Anchors and aliases at every position.
            b"base: &v 1.0\nentries:\n  c:\n    - version: *v\n      digest: *missing\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: &s [a, b]\n  d:\n    - version: 2.0\n      sources: *s\n",
            b"proto: &item\n  version: 1.0\nentries:\n  c: [*item, {version: 2.0}]\n",
            b"entries: &e\n  c:\n    - version: 1.0\nother: *e\n",
            b"entries:\n  c:\n    - &i {version: 1.0}\n    - *i\n",
            b"x: &m\n  version: 1.0\n  sources: [s]\nentries:\n  c:\n    - <<: *m\n      version: 2.0\n",
            b"entries:\n  c:\n    - <<: [a]\n      version: 2.0\n",
            b"entries:\n  c:\n    - version: 1.0\n  c:\n    - version: 2.0\n",
            // An alias into its own unfinished anchor, and one across a
            // redefined anchor.
            b"entries:\n  c: &a [*a]\n",
            b"one: &a [1]\ntwo: &a [*a]\nentries: {}\n",
            b"one: &a 1\ntwo: &a 2\nentries:\n  c:\n    - version: *a\n",
            // Duplicate keys at every depth.
            b"entries: {}\nentries: {}\n",
            b"entries:\n  c:\n    - version: 1.0\n      version: 1.1\n",
            b"entries:\n  c:\n    - version: 1.0\n      urls: [a]\n      urls: [b]\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {a: 1, a: 2}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations:\n        a:\n          b: 1\n          b: 2\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: [[{x: 1, x: 2}]]\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {1: a, 0x1: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {~: a, '': b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {true: a, !!bool true: b}\n",
            // Tags on scalars, sequences and mappings.
            b"entries:\n  c:\n    - version: !!str 1.0\n",
            b"entries:\n  c:\n    - version: !!str\n",
            b"entries:\n  c:\n    - version: !x 1.0\n",
            b"entries:\n  c:\n    - version: ! 1.0\n",
            b"entries:\n  c:\n    - version: !!int 5\n",
            b"entries:\n  c:\n    - digest: !x\n      version: 1.0\n",
            b"entries:\n  c:\n    - created: !!null x\n      version: 1.0\n",
            b"entries:\n  c:\n    - deprecated: !!bool true\n      version: 1.0\n",
            b"entries:\n  c:\n    - deprecated: !!bool |\n        true\n      version: 1.0\n",
            b"entries:\n  c:\n    - deprecated: |\n        true\n      version: 1.0\n",
            b"entries:\n  c:\n    - deprecated: !x false\n      version: 1.0\n",
            b"entries: !!map\n  c: [!x {version: 1.0}]\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: !x [a]\n      annotations: !x {a: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: !!seq [a]\n      annotations: !!map {a: b}\n",
            b"%TAG !e! tag:example.com,2020:\n---\nentries:\n  c:\n    - version: 1.0\n      sources: !e!s [a]\n      annotations: {k: !e!t v}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {k: !<tag:verbatim> v}\n",
            // Block scalars, in block and in flow context.
            b"entries:\n  c:\n    - version: |\n        1.0\n",
            b"entries:\n  c:\n    - version: >\n        1.0\n",
            b"entries:\n  c: [- version: 1.0, >\n  folded\n]\n",
            b"entries:\n  c: [{version: 1.0, annotations: {a: |\n  x\n}}]\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {a: >\n  x}\n",
            // Empty scalars as keys and values.
            b": ignored\nentries: {}\n",
            b"entries:\n  : [{version: 1.0}]\n",
            b"entries:\n  c:\n    - : 1\n      version: 2.0\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {: x}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {? : x}\n",
            b"entries:\n  c:\n    - version:\n",
            b"entries:\n  c:\n    - version: ''\n",
            // Control characters and BOMs.
            b"entries: {}\n\x00",
            b"\x00entries: {}\n",
            b"entries:\n  c:\x00\n    - version: 1.0\n",
            b"entries:\n  c:\n    - version: 1.\x000\n",
            b"\xef\xbb\xbfentries: {}\n",
            b"entries: {}\n\xef\xbb\xbf",
            b"entries:\n  c:\n    - version: 1\xef\xbb\xbf.0\n",
            b"entries: {}\n\xef\xbb\xbf\n",
            // Multi-document streams.
            b"entries: {}\n---\nentries: {}\n",
            b"entries: {}\n---\n",
            b"entries: {}\n...\n",
            b"entries: {}\n...\n---\nx: 1\n",
            b"entries: {}\n---\nx: 1\n",
            // Kept and ignored values at and past the recursion limit.
            b"entries:\n  c:\n    - version: 1.0\n      urls: [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[x]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[x]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]\n",
            // Numbers at the edges of serde's resolution.
            b"entries:\n  c:\n    - version: 1.0\n      sources: [99999999999999999999]\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: [9999999999999999999999999999999999999999]\n",
            b"entries:\n  c:\n    - version: 1.0\n      sources: [.nan, .inf, -.inf, 0x10, 0o17, 0b11, 007, -0, +5, 1e3]\n",
            // Degenerate shapes at every level.
            b"",
            b"~\n",
            b"[]\n",
            b"foo\n",
            b"entries: ~\n",
            b"entries: [a]\n",
            b"entries:\n  c: ~\n",
            b"entries:\n  c: x\n",
            b"entries:\n  c:\n    -\n",
            b"entries:\n  c:\n    - x\n",
            b"entries:\n  c:\n    - [a]\n",
            b"entries:\n  ~: [{version: 1.0}]\n",
            b"entries:\n  null: [{version: 1.0}]\n",
            b"entries:\n  1.5: [{version: 1.0}]\n",
            b"entries:\n  [a]: [{version: 1.0}]\n",
            b"entries:\n  c: *undefined\n",
            // Explicit and implicit empty nodes, and `?`-marked keys.
            b"entries:\n  c:\n    - ? : 1\n      version: 2.0\n",
            b"entries: ? : []\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {?: x}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {? x: 1}\n",
            b"entries: {a: ?x}\n",
            b"entries:\n  c: ?x\n",
            b"entries:\n  c:\n    - version: ?x\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {\"\": a, : b}\n",
            // `:` next to a flow indicator inside a plain scalar.
            b"entries: {}\nurls: {a: {k: x}}\n",
            b"entries: {}\nurls: {a:{k: x}}\n",
            b"entries: {}\nurls: {a:[x]}\n",
            b"entries: {}\nurls: {a:}\n",
            b"entries: {}\nurls: {a:, b: 1}\n",
            b"entries: {}\nurls: {a:?x}\n",
            b"entries: {}\nurls: {a:1}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {a:{b: c}}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {a: {b: c}}\n",
            // Resolved-key duplicates the string comparison misses.
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {!!str 1: a, 1: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {!t x: a, !t x: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {[x]: a, [x]: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {.nan: a, .nan: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {0: a, -0: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {~: a, null: b}\n",
            // Field values past u64 and i64, and odd field content.
            b"entries:\n  c:\n    - version: 1.0\n      sources: [340282366920938463463374607431768211456]\n",
            b"entries:\n  c:\n    - version: !!str ''\n",
            b"entries:\n  c:\n    - version: ~\n",
            b"entries:\n  c:\n    - version: !t\n",
            b"entries:\n  c:\n    - version: !!str '1.0'\n      version: 2.0\n",
            b"entries:\n  c:\n    - version: 1.0\n      unknown: *undefined\n",
            b"entries:\n  c:\n    - version: 1.0\n      unknown: &u [1]\n        extra: *u\n",
            b"entries:\n  c:\n    - version: 1.0\n      deprecated: !!bool 'true'\n",
            b"entries:\n  c:\n    - version: 1.0\n      deprecated: 'true'\n",
            b"entries:\n  c:\n    - version: 1.0\n      deprecated: !!str true\n",
            b"entries:\n  c:\n    - version: 1.0\n      created: 2020-01-01\n",
            b"entries:\n  c:\n    - version: 1.0\n      created: !!str 2020-01-01\n",
            b"entries:\n  c:\n    - version: 1.0\n      created: !!int 3\n",
            // A second document, and content after the document.
            b"entries: {}\nfoo\n",
            b"entries: {}\nfoo: bar\n",
            b"---\nentries: {}\n",
            b"%YAML 1.1\n---\nentries: {}\n",
            b"entries: {}\n\n\n",
            b"entries: {}\n# comment\n",
            b"entries: &x {}\n",
            b"&e\nentries:\n  c: []\n",
            // A folded block scalar that runs into the next key, and the
            // same behind a leading BOM — libyaml counts the BOM's column.
            b"x: >\nentries: {}\n",
            b"generated: >\nentries: {}\n",
            b"x: >\nentries: {}\nx: 2\n",
            b"\xef\xbb\xbfgenerated: >\nentries: {}\n",
            b"\xef\xbb\xbfgenerated: >\nentries: {}\ngenerated: &a0 {deprecated: !x \"n\\n\"}\n",
            b"\xef\xbb\xbfx: >\nentries: {}\n",
            b"entries:\n \xef\xbb\xbf c: []\n",
            b"entries:\n  c: []\n\xef\xbb\xbf",
            b"entries: {}\xef\xbb\xbf\n",
            b"generated: &a0 {d: !x \"n\\n\"}\nentries: {}\n",
            // Shapes that change what a map, seq or scalar resolves to.
            b"entries: {}\n\xef\xbb\xbf",
            b"entries:\n  c:\n    - version: 1.0\n      sources: [99999999999999999999999999999]\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: [x]:\n",
            b"entries:\n  c: {x: 1}\n  c: {x: 2}\n",
            b"entries: 1\n",
            b"entries: \n",
            b"entries:\n- version: 1.0\n",
        ];
        let mut failures = Vec::new();
        for (index, body) in cases.iter().enumerate() {
            check(&mut failures, body, &format!("table case {index}"));
        }
        report(failures);
    }

    /// A deterministic PRNG, so a failing case replays from its seed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
        fn pick<'a, T>(&mut self, choices: &'a [T]) -> &'a T {
            &choices[self.below(choices.len())]
        }
    }

    /// Builds the feature soup the review probed: every document shape is a
    /// plausible Helm index, but anchors, aliases, merge keys, duplicate
    /// keys, tags, block and flow scalars, empty keys, odd encodings and
    /// deep nesting can land anywhere.
    struct Gen {
        rng: Rng,
        anchors: Vec<String>,
        next_anchor: u32,
    }

    impl Gen {
        fn tag(&mut self) -> Option<&'static str> {
            self.rng.chance(18).then(|| {
                *self.rng.pick(&[
                    "!!str", "!!bool", "!!int", "!!float", "!!null", "!!seq", "!!map", "!x", "!",
                    "!e!app",
                ])
            })
        }
        fn anchor(&mut self) -> Option<String> {
            if self.rng.chance(12) {
                let name = format!("a{}", self.next_anchor);
                self.next_anchor += 1;
                self.anchors.push(name.clone());
                Some(name)
            } else {
                None
            }
        }
        fn prefix(&mut self) -> String {
            let mut out = String::new();
            if let Some(anchor) = self.anchor() {
                out.push('&');
                out.push_str(&anchor);
                out.push(' ');
            }
            if let Some(tag) = self.tag() {
                out.push_str(tag);
                out.push(' ');
            }
            out
        }
        fn scalar(&mut self, flow: bool) -> String {
            const PLAIN: &[&str] = &[
                "1.0",
                "2.0-alpha",
                "true",
                "false",
                "True",
                "null",
                "~",
                "0x10",
                "0o17",
                "0b11",
                "007",
                "1e3",
                ".5",
                "5.",
                ".nan",
                ".inf",
                "-.inf",
                "y",
                "n",
                "on",
                "off",
                "x",
                "main",
                "1_000",
                "+5",
                "-0",
                "99999999999999999999",
                "18446744073709551616",
                "2020-01-01",
            ];
            const QUOTED: &[&str] = &[
                "\"v 1.0\"",
                "'sq'",
                "\"a>b\"",
                "\"|\"",
                "\">\"",
                "\"\"",
                "''",
                "\"é🎉\"",
                "\"t\\t\"",
                "\"n\\n\"",
                "\"a,b:c\"",
                "\"#\"",
                "\"a\\uFEFFb\"",
            ];
            let body = if flow && self.rng.chance(60) {
                self.rng.pick(QUOTED)
            } else {
                self.rng.pick(if flow { QUOTED } else { PLAIN })
            };
            let mut out = self.prefix();
            out.push_str(body);
            out
        }
        /// An inline node: a scalar, flow collection, anchor or alias.
        fn inline(&mut self, depth: usize) -> String {
            match self.rng.below(100) {
                _ if depth > 4 => self.scalar(true),
                0..=44 => self.scalar(true),
                45..=59 => {
                    let items: Vec<_> = (0..self.rng.below(4))
                        .map(|_| self.inline(depth + 1))
                        .collect();
                    format!("[{}]", items.join(", "))
                }
                60..=74 => {
                    let pairs: Vec<_> = (0..self.rng.below(3))
                        .map(|_| format!("{}: {}", self.key(), self.inline(depth + 1)))
                        .collect();
                    format!("{{{}}}", pairs.join(", "))
                }
                75..=84 => {
                    let anchor = format!("&a{}", self.next_anchor);
                    self.next_anchor += 1;
                    self.anchors.push(anchor.clone());
                    format!("{} {}", anchor, self.inline(depth + 1))
                }
                85..=94 => {
                    if self.rng.chance(85) && !self.anchors.is_empty() {
                        format!(
                            "*{}",
                            self.anchors[self.rng.below(self.anchors.len())].clone()
                        )
                    } else {
                        format!("*u{}", self.rng.below(4))
                    }
                }
                _ => (*self
                    .rng
                    .pick(&[">", "|", ">", "|", "&a9 *u1", "", " ", "?x", "\u{FEFF}"]))
                .to_owned(),
            }
        }
        fn key(&mut self) -> String {
            if self.rng.chance(3) {
                return String::new();
            }
            if self.rng.chance(6) {
                return format!("[{}]", self.inline(5));
            }
            if self.rng.chance(5) {
                let mut out = self.prefix();
                out.push_str(self.rng.pick(KEYS));
                return out;
            }
            (*self.rng.pick(KEYS)).to_owned()
        }
        /// A node rendered at a block position: usually inline after the
        /// `:`/`-`, sometimes a nested block or a block scalar.
        fn value(&mut self, out: &mut String, indent: usize, depth: usize) {
            match self.rng.below(100) {
                _ if depth > 5 => {
                    out.push(' ');
                    out.push_str(&self.scalar(false));
                }
                0..=58 => {
                    out.push(' ');
                    out.push_str(&self.inline(depth));
                }
                59..=68 => {
                    let pad = "  ".repeat(indent + 1);
                    for _ in 0..self.rng.below(3) {
                        out.push('\n');
                        out.push_str(&pad);
                        out.push_str("- ");
                        out.push_str(&self.inline(depth + 1));
                    }
                }
                69..=78 => {
                    let pad = "  ".repeat(indent + 1);
                    for _ in 0..self.rng.below(3) {
                        out.push('\n');
                        out.push_str(&pad);
                        out.push_str(&self.key());
                        out.push(':');
                        self.value(out, indent + 1, depth + 1);
                    }
                }
                79..=84 => {
                    let style = *self.rng.pick(&["|", ">", "|-", ">-", "|+", ">+"]);
                    let pad = "  ".repeat(indent + 1);
                    out.push(' ');
                    out.push_str(&self.prefix());
                    out.push_str(style);
                    for line in ["x", "two lines", ""] {
                        out.push('\n');
                        out.push_str(&pad);
                        out.push_str(line);
                    }
                }
                85..=90 => {
                    out.push('\n');
                    out.push_str(&"  ".repeat(indent + 1));
                    out.push_str(self.rng.pick(&["- ", "-  ", "# c\n"]));
                    let pad = "  ".repeat(indent + 1);
                    out.push_str(&pad);
                    out.push_str(&self.inline(depth + 1));
                }
                _ => {
                    if self.rng.chance(50) {
                        out.push(' ');
                        out.push_str(&self.deep(indent));
                    }
                }
            }
        }
        /// A deeply nested collection, at a chosen depth. Flow collections
        /// stay under 255 levels — yaml-rust2's scanner caps its flow level
        /// there where libyaml's effectively does not — and block entries
        /// carry the deeper cases, which neither parser caps.
        fn deep(&mut self, _indent: usize) -> String {
            let depth = *self
                .rng
                .pick(&[100, 118, 122, 124, 125, 126, 128, 200, 400, 600]);
            let map = self.rng.chance(50);
            let mut out = String::new();
            if depth > 250 || (!map && self.rng.chance(50)) {
                for _ in 0..depth {
                    out.push_str("- ");
                }
                out.push('x');
            } else {
                for _ in 0..depth {
                    out.push_str(if map { "{k:" } else { "[" });
                }
                out.push('x');
                for _ in 0..depth {
                    out.push_str(if map { "}" } else { "]" });
                }
            }
            out
        }
        fn chart_item(&mut self, out: &mut String, indent: usize) {
            let pad = "  ".repeat(indent);
            let fields = self.rng.below(4) + 1;
            for index in 0..fields {
                if index == 0 {
                    out.push(' ');
                } else {
                    out.push('\n');
                    out.push_str(&pad);
                }
                out.push_str(&self.key());
                out.push(':');
                self.value(out, indent, 0);
            }
        }
        fn versions(&mut self, out: &mut String, indent: usize) {
            match self.rng.below(100) {
                0..=64 => {
                    let pad = "  ".repeat(indent + 1);
                    for _ in 0..self.rng.below(3) + 1 {
                        out.push('\n');
                        out.push_str(&pad);
                        out.push('-');
                        if self.rng.chance(75) {
                            self.chart_item(out, indent + 1);
                        } else {
                            self.value(out, indent + 1, 0);
                        }
                    }
                }
                65..=84 => {
                    out.push_str(" [");
                    let items: Vec<String> = (0..self.rng.below(3))
                        .map(|_| {
                            if self.rng.chance(70) {
                                let pairs: Vec<String> = (0..self.rng.below(3) + 1)
                                    .map(|_| format!("{}: {}", self.key(), self.inline(0)))
                                    .collect();
                                format!("{{{}}}", pairs.join(", "))
                            } else {
                                self.inline(0)
                            }
                        })
                        .collect();
                    out.push_str(&items.join(", "));
                    out.push(']');
                }
                _ => self.value(out, indent, 0),
            }
        }
        fn entries(&mut self, out: &mut String) {
            match self.rng.below(100) {
                0..=69 => {
                    for _ in 0..self.rng.below(3) + 1 {
                        out.push('\n');
                        out.push_str("  ");
                        out.push_str(&self.key());
                        out.push(':');
                        self.versions(out, 1);
                    }
                }
                70..=84 => {
                    out.push_str(" {");
                    let pairs: Vec<String> = (0..self.rng.below(3))
                        .map(|_| {
                            let mut v = String::new();
                            v.push_str(&self.key());
                            v.push_str(": [");
                            v.push_str(&self.inline(0));
                            v.push(']');
                            v
                        })
                        .collect();
                    out.push_str(&pairs.join(", "));
                    out.push('}');
                }
                _ => self.value(out, 0, 0),
            }
        }
        fn document(&mut self) -> Vec<u8> {
            let mut out = String::new();
            if self.rng.chance(8) {
                out.push_str("%YAML 1.1\n");
            }
            if self.rng.chance(6) {
                out.push_str("%TAG !e! tag:example.com,2020:\n");
            }
            if self.rng.chance(10) {
                out.push_str("---\n");
            }
            let roots = self.rng.below(3) + 1;
            let mut has_entries = false;
            for _ in 0..roots {
                let (key, entries) = match self.rng.below(100) {
                    0..=54 if !has_entries => {
                        has_entries = true;
                        ("entries".to_owned(), true)
                    }
                    0..=59 => ("entries".to_owned(), true),
                    60..=69 if self.rng.chance(30) => (self.key(), false),
                    _ => (
                        (*self.rng.pick(&["apiVersion", "generated", "other", "urls"])).to_owned(),
                        false,
                    ),
                };
                out.push_str(&key);
                out.push(':');
                if entries {
                    self.entries(&mut out);
                } else {
                    self.value(&mut out, 0, 0);
                }
                out.push('\n');
            }
            match self.rng.below(100) {
                0..=4 => out.push_str("...\n"),
                5..=9 => out.push_str("---\n"),
                10..=14 => out.push_str("---\nx: 1\n"),
                15..=19 => out.push_str("...\n---\n"),
                20..=24 => {
                    out.push('\n');
                    out.push_str(&self.inline(5));
                    out.push('\n');
                }
                _ => {}
            }
            let mut bytes = out.into_bytes();
            match self.rng.below(100) {
                0..=3 => {
                    bytes.splice(..0, [0xEF, 0xBB, 0xBF]);
                }
                4..=6 => {
                    bytes = String::from_utf8_lossy(&bytes)
                        .encode_utf16()
                        .flat_map(|u| u.to_le_bytes())
                        .collect();
                    bytes.splice(..0, [0xFF, 0xFE]);
                }
                7..=9 => {
                    bytes = String::from_utf8_lossy(&bytes)
                        .encode_utf16()
                        .flat_map(|u| u.to_be_bytes())
                        .collect();
                    bytes.splice(..0, [0xFE, 0xFF]);
                }
                10..=12 => {
                    bytes = String::from_utf8_lossy(&bytes)
                        .encode_utf16()
                        .flat_map(|u| u.to_le_bytes())
                        .collect();
                }
                13..=17 if !bytes.is_empty() => {
                    bytes.insert(self.rng.below(bytes.len()), 0);
                }
                18..=22 if !bytes.is_empty() => {
                    let at = self.rng.below(bytes.len());
                    bytes.splice(at..at, [0xEF, 0xBB, 0xBF]);
                }
                _ => {}
            }
            bytes
        }
    }

    const KEYS: &[&str] = &[
        "version",
        "created",
        "digest",
        "deprecated",
        "sources",
        "annotations",
        "urls",
        "keywords",
        "icon",
        "entries",
        "apiVersion",
        "name",
        "chart",
        "~",
        "null",
        "1.5",
        "true",
        "é",
        "a-b",
    ];

    /// The seeded corpus: thousands of composed documents at a fixed seed,
    /// each checked whole and at every chunking.
    #[test]
    fn generated_documents_derive_what_the_buffered_parse_does() {
        const SEED: u64 = 0x9E3779B97F4A7C15;
        let mut rng = Rng(SEED);
        let mut failures = Vec::new();
        for index in 0..3000 {
            let seed = rng.next();
            let mut document = Gen {
                rng: Rng(seed),
                anchors: Vec::new(),
                next_anchor: 0,
            };
            let body = document.document();
            check(
                &mut failures,
                &body,
                &format!("generated seed {seed:#x} (case {index})"),
            );
        }
        report(failures);
    }

    /// Every collected divergence, or a pass.
    fn report(failures: Vec<String>) {
        assert!(
            failures.is_empty(),
            "{} divergences\n{}",
            failures.len(),
            failures
                .iter()
                .take(120)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
