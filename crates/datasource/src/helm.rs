//! A Helm repository's `index.yaml`, read as it streams: yaml-rust2's event
//! parser pulls the document's characters out of the body, and a walk keeps
//! only the fields each chart entry uses — never the whole index. The walk
//! mirrors how `serde_yaml_ng` would deserialize the same structure: plain
//! scalars are resolved under its schema, aliases replay their anchored
//! nodes, and anything it would reject is a document error.
use crate::{Ecosystem, HelmChart, Version, sort, upstream};
use patchula_http::{CacheError, Derived};
use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    io::Read,
    rc::Rc,
};
use yaml_rust2::{
    parser::{Event, Parser, Tag},
    scanner::TScalarStyle,
};

/// The document's characters, decoded as they are read: UTF-8, or UTF-16
/// when the first bytes say so, the way libyaml decides encoding. An io or
/// decoding error poisons the rest of the stream.
struct Chars<'a> {
    body: std::io::BufReader<&'a mut dyn Read>,
    /// Bytes read while the encoding was undecided.
    ahead: VecDeque<u8>,
    /// UTF-16 endianness once decided: big when true, little when false.
    wide: Option<bool>,
    failed: Rc<Cell<bool>>,
}
impl Iterator for Chars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        if self.failed.get() {
            return None;
        }
        match self.wide {
            None => self.decide(),
            Some(true) => self.utf16(),
            Some(false) => self.utf8(),
        }
    }
}
impl Chars<'_> {
    fn byte(&mut self) -> Option<u8> {
        if let Some(byte) = self.ahead.pop_front() {
            return Some(byte);
        }
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
    /// Fills `ahead` to `n` bytes unless the stream ends first.
    fn peek(&mut self, n: usize) {
        while self.ahead.len() < n {
            let mut byte = [0];
            match self.body.read(&mut byte) {
                Ok(1) => self.ahead.push_back(byte[0]),
                Ok(_) => break,
                Err(_) => {
                    self.failed.set(true);
                    break;
                }
            }
        }
    }
    /// The first bytes decide the encoding the way libyaml does: a BOM, then
    /// the NUL half of a UTF-16 unit, else UTF-8.
    fn decide(&mut self) -> Option<char> {
        self.peek(2);
        match (self.ahead.front(), self.ahead.get(1)) {
            (Some(0xFE), Some(0xFF)) => {
                self.ahead.clear();
                self.wide = Some(true);
            }
            (Some(0xFF), Some(0xFE)) => {
                self.ahead.clear();
                self.wide = Some(false);
            }
            (Some(0x00), _) => self.wide = Some(true),
            (_, Some(0x00)) => self.wide = Some(false),
            _ => {
                self.wide = Some(false);
                if self.ahead.front() == Some(&0xEF) {
                    self.peek(3);
                    if self.ahead.iter().eq([0xEF, 0xBB, 0xBF].iter()) {
                        self.ahead.clear();
                    }
                }
            }
        }
        self.next()
    }
    fn utf8(&mut self) -> Option<char> {
        let lead = self.byte()?;
        let length = match lead {
            0x00..=0x7F => return Some(lead as char),
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => {
                self.failed.set(true);
                return None;
            }
        };
        self.peek(length - 1);
        if self.ahead.len() < length - 1 {
            // A truncated character at the end of the stream.
            if !self.failed.get() {
                self.failed.set(true);
            }
            return None;
        }
        let mut bytes = vec![lead];
        bytes.extend(self.ahead.drain(..length - 1));
        match std::str::from_utf8(&bytes) {
            Ok(text) => text.chars().next(),
            Err(_) => {
                self.failed.set(true);
                None
            }
        }
    }
    fn utf16(&mut self) -> Option<char> {
        fn unit(this: &mut Chars<'_>, be: bool) -> Option<u16> {
            let high = this.byte()?;
            let Some(low) = this.byte() else {
                // An odd byte ends a character early.
                this.failed.set(true);
                return None;
            };
            Some(if be {
                u16::from_be_bytes([high, low])
            } else {
                u16::from_le_bytes([high, low])
            })
        }
        let be = self.wide == Some(true);
        let first = unit(self, be)?;
        let scalar = if (0xD800..0xDC00).contains(&first) {
            let Some(second) = unit(self, be) else {
                self.failed.set(true);
                return None;
            };
            if !(0xDC00..0xE000).contains(&second) {
                self.failed.set(true);
                return None;
            }
            0x10000 + (((first as u32) - 0xD800) << 10) + (second as u32 - 0xDC00)
        } else if (0xDC00..0xE000).contains(&first) {
            self.failed.set(true);
            return None;
        } else {
            first as u32
        };
        match char::from_u32(scalar) {
            Some(c) => Some(c),
            None => {
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
/// alias can replay them.
#[derive(Clone)]
enum Node {
    Scalar(String, TScalarStyle, Option<String>),
    Seq(Option<String>, Vec<Node>),
    Map(Option<String>, Vec<(Node, Node)>),
}

/// What an event is to the walk: aliases arrive already replayed.
enum Ev {
    Scalar(String, TScalarStyle, Option<String>),
    SeqStart(Option<String>),
    SeqEnd,
    MapStart(Option<String>),
    MapEnd,
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
    /// anchor and node.
    fn feed(&mut self, event: &Ev) -> Option<(usize, Node)> {
        let built = match event {
            Ev::Scalar(text, style, tag) => Node::Scalar(text.clone(), *style, tag.clone()),
            Ev::SeqStart(tag) => {
                self.stack.push(Partial::Seq(tag.clone(), Vec::new()));
                return None;
            }
            Ev::MapStart(tag) => {
                self.stack.push(Partial::Map(tag.clone(), Vec::new(), None));
                return None;
            }
            Ev::SeqEnd => match self.stack.pop() {
                Some(Partial::Seq(tag, items)) => Node::Seq(tag, items),
                _ => return None,
            },
            Ev::MapEnd => match self.stack.pop() {
                Some(Partial::Map(tag, pairs, _)) => Node::Map(tag, pairs),
                _ => return None,
            },
            _ => return None,
        };
        match self.stack.last_mut() {
            Some(Partial::Seq(_, items)) => items.push(built),
            Some(Partial::Map(_, _, key @ None)) => *key = Some(built),
            Some(Partial::Map(_, pairs, key)) => {
                if let Some(key) = key.take() {
                    pairs.push((key, built));
                }
            }
            None => return Some((self.anchor, built)),
        }
        None
    }
}

/// The pull side of the walk: parser events in order, with anchored nodes
/// recorded as they close and aliases replayed in place.
struct Walk<'a> {
    parser: Parser<Chars<'a>>,
    pending: VecDeque<Ev>,
    recording: Vec<Recording>,
    anchors: BTreeMap<usize, Node>,
    failed: Rc<Cell<bool>>,
}
impl<'a> Walk<'a> {
    fn new(body: &'a mut dyn Read) -> Self {
        let failed = Rc::new(Cell::new(false));
        Self {
            parser: Parser::new(Chars {
                body: std::io::BufReader::new(body),
                ahead: VecDeque::new(),
                wide: None,
                failed: failed.clone(),
            }),
            pending: VecDeque::new(),
            recording: Vec::new(),
            anchors: BTreeMap::new(),
            failed,
        }
    }
    /// Records the event in every open anchored tree.
    fn record(&mut self, event: &Ev) {
        let mut done = Vec::new();
        for recording in &mut self.recording {
            if let Some(built) = recording.feed(event) {
                done.push(built);
            }
        }
        self.recording
            .retain(|recording| !recording.stack.is_empty());
        for (anchor, node) in done {
            self.anchors.insert(anchor, node);
        }
    }
    /// The events a kept node replays.
    fn replay(node: &Node, into: &mut VecDeque<Ev>) {
        match node {
            Node::Scalar(text, style, tag) => {
                into.push_back(Ev::Scalar(text.clone(), *style, tag.clone()))
            }
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
    /// The next event, or a document error: parse failures and unresolved
    /// aliases alike.
    fn take(&mut self) -> Result<Ev, ()> {
        if let Some(event) = self.pending.pop_front() {
            self.record(&event);
            return Ok(event);
        }
        if self.failed.get() {
            return Err(());
        }
        let (event, _) = self.parser.next_token().map_err(|_| ())?;
        if self.failed.get() {
            return Err(());
        }
        let event = match event {
            Event::Scalar(text, style, anchor, tag) => {
                let tag = tag.as_ref().map(serde_tag);
                if anchor != 0 {
                    self.anchors
                        .insert(anchor, Node::Scalar(text.clone(), style, tag.clone()));
                }
                Ev::Scalar(text, style, tag)
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 {
                    self.recording.push(Recording {
                        anchor,
                        stack: vec![Partial::Seq(tag.as_ref().map(serde_tag), Vec::new())],
                    });
                }
                Ev::SeqStart(tag.as_ref().map(serde_tag))
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 {
                    self.recording.push(Recording {
                        anchor,
                        stack: vec![Partial::Map(tag.as_ref().map(serde_tag), Vec::new(), None)],
                    });
                }
                Ev::MapStart(tag.as_ref().map(serde_tag))
            }
            Event::SequenceEnd => Ev::SeqEnd,
            Event::MappingEnd => Ev::MapEnd,
            Event::Alias(id) => {
                let Some(node) = self.anchors.get(&id).cloned() else {
                    self.failed.set(true);
                    return Err(());
                };
                Self::replay(&node, &mut self.pending);
                return self.take();
            }
            Event::DocumentStart => Ev::DocumentStart,
            Event::DocumentEnd => Ev::DocumentEnd,
            Event::StreamStart => Ev::StreamStart,
            Event::StreamEnd => Ev::StreamEnd,
            Event::Nothing => return Err(()),
        };
        self.record(&event);
        Ok(event)
    }
    /// The node starting with `first`, read to its end. What serde reads of
    /// a field or item; `depth` is serde's remaining recursion allowance.
    fn node(&mut self, first: Ev, depth: usize) -> Result<Node, ()> {
        match first {
            Ev::Scalar(text, style, tag) => Ok(Node::Scalar(text, style, tag)),
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
/// serde's `visit_int`: any width's parse resolves the scalar to a number.
fn integer(scalar: &str) -> bool {
    unsigned_int(scalar).is_some() || negative_int(scalar).is_some()
}
/// serde's `parse_f64`: infinities and NaN under their YAML spellings only.
fn float(scalar: &str) -> bool {
    let unpositive = match scalar.strip_prefix('+') {
        Some(unpositive) if !unpositive.starts_with(['+', '-']) => unpositive,
        Some(_) => return false,
        None => scalar,
    };
    if matches!(unpositive, ".inf" | ".Inf" | ".INF")
        || matches!(
            scalar,
            "-.inf" | "-.Inf" | "-.INF" | ".nan" | ".NaN" | ".NAN"
        )
    {
        return true;
    }
    unpositive
        .parse::<f64>()
        .is_ok_and(|float| float.is_finite())
}
/// Whether an untagged plain scalar resolves to something but a string, the
/// way serde_yaml_ng's `visit_untagged_scalar` resolves it.
fn non_string(scalar: &str) -> bool {
    scalar.is_empty()
        || null(scalar)
        || boolean(scalar).is_some()
        || integer(scalar)
        || (!digits_but_not_number(scalar) && float(scalar))
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
/// `as_str`, `as_sequence` and `get` semantics for the metadata it keeps.
enum Yaml {
    Str(String),
    Tagged(Box<Yaml>),
    Seq(Vec<Yaml>),
    Map(Vec<(Yaml, Yaml)>),
    /// Null, booleans and numbers: never a string, sequence or mapping.
    Other,
}
impl Yaml {
    fn untag(&self) -> &Self {
        let mut node = self;
        while let Yaml::Tagged(inner) = node {
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

/// A scalar resolved the way `deserialize_any` — a `Value` — resolves it:
/// core tags type or fail the document, a local `!` tag wraps the resolved
/// node, other tags make a string, and an untagged scalar follows the
/// schema's resolution.
fn scalar_value(text: String, style: TScalarStyle, tag: Option<String>) -> Result<Yaml, ()> {
    if let Some(tag) = tag {
        if tag.starts_with('!') {
            let inner = if style == TScalarStyle::Plain && non_string(&text) {
                Yaml::Other
            } else {
                Yaml::Str(text)
            };
            return Ok(Yaml::Tagged(Box::new(inner)));
        }
        return match tag.as_str() {
            "tag:yaml.org,2002:bool" => boolean(&text).map(|_| Yaml::Other).ok_or(()),
            "tag:yaml.org,2002:int" => {
                if integer(&text) {
                    Ok(Yaml::Other)
                } else {
                    Err(())
                }
            }
            "tag:yaml.org,2002:null" => {
                if null(&text) {
                    Ok(Yaml::Other)
                } else {
                    Err(())
                }
            }
            "tag:yaml.org,2002:float" => {
                if float(&text) {
                    Ok(Yaml::Other)
                } else {
                    Err(())
                }
            }
            _ => Ok(Yaml::Str(text)),
        };
    }
    Ok(if style == TScalarStyle::Plain && non_string(&text) {
        Yaml::Other
    } else {
        Yaml::Str(text)
    })
}

/// A node as a `Value`: serde's tag and schema resolution applied.
fn yaml(node: &Node) -> Result<Yaml, ()> {
    Ok(match node {
        Node::Scalar(text, style, tag) => scalar_value(text.clone(), *style, tag.clone())?,
        Node::Seq(tag, items) => {
            let seq = Yaml::Seq(items.iter().map(yaml).collect::<Result<_, _>>()?);
            match tag {
                Some(tag) if tag.starts_with('!') => Yaml::Tagged(Box::new(seq)),
                _ => seq,
            }
        }
        Node::Map(tag, pairs) => {
            let map = Yaml::Map(
                pairs
                    .iter()
                    .map(|(key, value)| Ok((yaml(key)?, yaml(value)?)))
                    .collect::<Result<Vec<_>, ()>>()?,
            );
            match tag {
                Some(tag) if tag.starts_with('!') => Yaml::Tagged(Box::new(map)),
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
/// The chart entries a `Node::Map` describes, as the `Chart` struct reads
/// them: fields by name, known names once each, the rest ignored.
fn chart_fields(pairs: &[(Node, Node)]) -> Result<ChartFields, ()> {
    let mut fields = ChartFields::default();
    let mut seen = 0u8;
    for (key, value) in pairs {
        let Node::Scalar(name, _, _) = key else {
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
                // serde reads a bool from a plain scalar, or a literal one
                // carrying `!!bool`.
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
/// The node a `String` map key reads: any scalar's text.
fn string_key(node: &Node) -> Result<&str, ()> {
    match node {
        Node::Scalar(name, _, _) => Ok(name),
        _ => Err(()),
    }
}

/// Reads the index's `entries` map as it streams: each chart name's version
/// list is consumed an entry at a time.
fn helm_index(walk: &mut Walk) -> Result<Vec<(String, Vec<ChartFields>)>, ()> {
    let mut entries = Vec::new();
    let mut seen_entries = false;
    loop {
        match walk.take()? {
            Ev::MapEnd => break,
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
                                event => {
                                    let key = walk.node(event, 127)?;
                                    let chart = string_key(&key)?.to_owned();
                                    let mut versions = Vec::new();
                                    match walk.take()? {
                                        Ev::SeqStart(_) => loop {
                                            match walk.take()? {
                                                Ev::SeqEnd => break,
                                                item => {
                                                    versions.push(chart_fields(pairs_of(
                                                        &walk.node(item, 126)?,
                                                    )?)?);
                                                }
                                            }
                                        },
                                        first => {
                                            for item in items_of(&walk.node(first, 127)?)? {
                                                versions.push(chart_fields(pairs_of(item)?)?);
                                            }
                                        }
                                    }
                                    entries.push((chart, versions));
                                }
                            }
                        },
                        event => {
                            for (key, value) in pairs_of(&walk.node(event, 127)?)? {
                                let chart = string_key(key)?.to_owned();
                                let mut versions = Vec::new();
                                for item in items_of(value)? {
                                    versions.push(chart_fields(pairs_of(item)?)?);
                                }
                                entries.push((chart, versions));
                            }
                        }
                    }
                } else {
                    let first = walk.take()?;
                    walk.node(first, 127)?;
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
        let entries = match walk.take()? {
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
pub(crate) const HELM_INDEX: Derived<CacheError> = Derived::new("helm-index", 2, helm);
