//! A Helm repository's `index.yaml`, read as it streams: libyaml's own
//! events, from `libyaml-safer` — the safe-Rust port of the unsafe-libyaml
//! `serde_yaml_ng` runs — come out of the body, and a walk keeps only the
//! fields each chart entry uses, never the whole index. The walk mirrors
//! how `serde_yaml_ng` would deserialize the same structure: plain scalars
//! are resolved under its schema, aliases replay their anchored nodes
//! where serde would follow them, and anything it would reject — bad
//! UTF-8, NUL, too much nesting in a kept value — is a document error.
//! The parser is told UTF-8, the encoding serde's parser forces, so
//! UTF-16 and other encodings fail as undecodable bytes.
use crate::{Ecosystem, HelmChart, Version, sort, upstream};
use libyaml_safer::{Encoding, EventData, Parser, ScalarStyle};
use patchula_http::{CacheError, Derived};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufReader, Read},
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// A document's node as the walk consumes it: the structure serde would
/// see, resolved scalars excepted.
enum Node {
    Scalar(String, ScalarStyle, Option<String>),
    Seq(Option<String>, Vec<Node>),
    Map(Option<String>, Vec<(Node, Node)>),
}

/// What an event is to the walk: aliases stay aliases until a position
/// that deserializes them asks for their node — serde's `ignore_any` just
/// steps over them. The alias carries the anchor's binding id, resolved
/// where the event is read, the way serde's loader stores each alias's
/// position.
#[derive(Clone)]
enum Ev {
    Scalar(String, ScalarStyle, Option<String>),
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

/// A recording's open-collection budget: serde's recursion limit, 128
/// collections under the root. An anchor nested past it can never
/// deserialize anywhere, so the recording is abandoned — nothing deeper
/// needs keeping, and an alias to it fails where serde's remaining-depth
/// check would.
const MAX_NODE_DEPTH: usize = 128;

/// An anchor still recording. Every content event until the end event
/// balancing its root is appended once to the buffer all recordings
/// share; the anchor's node is its span, `start` to that end. `depth`
/// counts the collections open inside it — inner anchors' starts deepen
/// it like any collection's — and past `MAX_NODE_DEPTH` the recording is
/// dropped rather than bound.
struct Recording {
    anchor: usize,
    start: usize,
    depth: usize,
}

/// The pull side of the walk: libyaml's events in order, with anchored
/// nodes recorded as they close and aliases resolved where serde's
/// deserializer would follow them.
struct Walk<'a> {
    parser: Parser<BufReader<&'a mut dyn Read>>,
    /// Replayed alias work as a stack of frames: serde's `jump` gives each
    /// alias its own cursor into the anchored events, so a nested alias's
    /// replay runs ahead of the frame below it, and an exhausted frame
    /// pops back to the caller's events.
    pending: Vec<VecDeque<Ev>>,
    /// Anchors still recording, shallowest first.
    recording: Vec<Recording>,
    /// Every recorded content event, stored once however many anchors'
    /// spans cover it: a nested anchor's span covers the inner anchors'
    /// events without holding them again, so anchored memory tracks the
    /// anchored events, not the nesting.
    buffer: Vec<Ev>,
    /// Anchor name to its latest binding — serde's loader `anchors` map,
    /// where a redefined name shadows the earlier node — and each
    /// binding's recorded span in `buffer`, serde's `aliases`
    /// id-to-position map. A scalar is its one event's span; an abandoned
    /// recording is never bound, so an alias to it fails like an alias
    /// serde's remaining-depth check would fail.
    bindings: BTreeMap<String, usize>,
    anchors: BTreeMap<usize, Range<usize>>,
    next_binding: usize,
    /// Content events pulled so far: what serde's `document.events` would
    /// hold (scalars, aliases and the four collection events; the stream
    /// and document markers are not in it). serde's alias bound is a
    /// hundred jumps per document event — checked when the count is
    /// final.
    events: usize,
    /// Alias resolutions — serde's `jumpcount`.
    jumps: usize,
    /// Events pushed through alias replays. serde's own bound on that
    /// work scales with the whole document's event count — a bound a
    /// streaming walk can only check at stream end — so replay work is
    /// capped at a fixed limit instead, `REPLAY_LIMIT`: a document serde
    /// accepts can still exceed it (a small anchor replayed past it stays
    /// under serde's hundred-jumps-per-event bound), and there the walk
    /// fails where serde would not. Fail closed; only an alias bomb
    /// reaches it — an index never aliases.
    replayed: usize,
}

/// The most events alias replays may push for one document: generous
/// against any honest index, short of what an alias bomb can do to the
/// buffered parse.
const REPLAY_LIMIT: usize = 1 << 20;

impl<'a> Walk<'a> {
    fn new(body: &'a mut dyn Read) -> Self {
        let mut parser = Parser::new();
        parser.set_input(BufReader::new(body));
        // serde_yaml_ng's parser forces UTF-8 over whatever the bytes
        // claim, so UTF-16 — BOM'd or not — fails as undecodable input.
        parser.set_encoding(Encoding::Utf8);
        Self {
            parser,
            pending: Vec::new(),
            recording: Vec::new(),
            buffer: Vec::new(),
            bindings: BTreeMap::new(),
            anchors: BTreeMap::new(),
            next_binding: 0,
            events: 0,
            jumps: 0,
            replayed: 0,
        }
    }
    /// Binds an anchor name to a fresh binding id, the way serde's loader
    /// registers each anchored node's position — before its content is
    /// read, so an alias inside it names the node still being recorded.
    fn bind_anchor(&mut self, name: String) -> usize {
        let id = self.next_binding;
        self.next_binding += 1;
        self.bindings.insert(name, id);
        id
    }
    /// Records the event in every open anchored span: appended once to
    /// the shared buffer, then counted in each recording's depth. A
    /// start that pushes a recording past serde's recursion budget
    /// abandons it — its remaining events still belong to the spans
    /// above it — and an end that closes a recording binds its anchor's
    /// span. The event's buffer position, when it was kept: a stream or
    /// document marker, or an event outside any anchor, keeps nothing.
    fn record(&mut self, event: &Ev) -> Option<usize> {
        let content = matches!(
            event,
            Ev::Scalar(..)
                | Ev::SeqStart(_)
                | Ev::SeqEnd
                | Ev::MapStart(_)
                | Ev::MapEnd
                | Ev::Alias(_)
        );
        if !content || self.recording.is_empty() {
            return None;
        }
        let at = self.store(event);
        match event {
            Ev::SeqStart(_) | Ev::MapStart(_) => {
                self.recording.retain_mut(|recording| {
                    recording.depth += 1;
                    recording.depth <= MAX_NODE_DEPTH
                });
            }
            Ev::SeqEnd | Ev::MapEnd => {
                let end = at + 1;
                let mut done = Vec::new();
                self.recording.retain_mut(|recording| {
                    recording.depth -= 1;
                    if recording.depth == 0 {
                        done.push((recording.anchor, recording.start..end));
                        return false;
                    }
                    true
                });
                for (anchor, span) in done {
                    self.anchors.insert(anchor, span);
                }
            }
            _ => {}
        }
        Some(at)
    }
    /// The event into the shared buffer every open span indexes. Its
    /// position.
    fn store(&mut self, event: &Ev) -> usize {
        let at = self.buffer.len();
        self.buffer.push(event.clone());
        at
    }
    /// The next event, or a document error: a parse failure, an alias to
    /// an anchor never bound — serde's loader fails those no matter where
    /// they sit — or serde's repetition bound, checked at stream end when
    /// the document's event count is final.
    fn take(&mut self) -> Result<Ev, ()> {
        while let Some(frame) = self.pending.last_mut() {
            if let Some(event) = frame.pop_front() {
                return Ok(event);
            }
            self.pending.pop();
        }
        let event = match self.parser.parse() {
            Ok(e) => e,
            Err(_e) => return Err(()),
        };
        let event = match event.data {
            EventData::StreamStart { .. } => Ev::StreamStart,
            EventData::StreamEnd => {
                if self.jumps > self.events * 100 {
                    return Err(());
                }
                Ev::StreamEnd
            }
            EventData::DocumentStart { .. } => Ev::DocumentStart,
            EventData::DocumentEnd { .. } => Ev::DocumentEnd,
            EventData::Alias { anchor } => {
                self.events += 1;
                let Some(&id) = self.bindings.get(&anchor) else {
                    return Err(());
                };
                Ev::Alias(id)
            }
            EventData::Scalar {
                anchor,
                tag,
                value,
                style,
                ..
            } => {
                self.events += 1;
                let event = Ev::Scalar(value, style, tag);
                if let Some(name) = anchor {
                    let id = self.bind_anchor(name);
                    // A scalar is its one event; its span is where the
                    // spans already open — or the buffer itself — kept
                    // it.
                    let start = self.record(&event).unwrap_or_else(|| self.store(&event));
                    self.anchors.insert(id, start..start + 1);
                    return Ok(event);
                }
                event
            }
            EventData::SequenceStart { anchor, tag, .. } => {
                self.events += 1;
                let event = Ev::SeqStart(tag);
                if let Some(name) = anchor {
                    let id = self.bind_anchor(name);
                    // The start belongs to the anchored spans still open
                    // before the anchor's own recording begins at it.
                    let start = self.record(&event).unwrap_or_else(|| self.store(&event));
                    self.recording.push(Recording {
                        anchor: id,
                        start,
                        depth: 1,
                    });
                    return Ok(event);
                }
                event
            }
            EventData::MappingStart { anchor, tag, .. } => {
                self.events += 1;
                let event = Ev::MapStart(tag);
                if let Some(name) = anchor {
                    let id = self.bind_anchor(name);
                    let start = self.record(&event).unwrap_or_else(|| self.store(&event));
                    self.recording.push(Recording {
                        anchor: id,
                        start,
                        depth: 1,
                    });
                    return Ok(event);
                }
                event
            }
            EventData::SequenceEnd => {
                self.events += 1;
                Ev::SeqEnd
            }
            EventData::MappingEnd => {
                self.events += 1;
                Ev::MapEnd
            }
        };
        self.record(&event);
        Ok(event)
    }
    /// The span an alias event names, replayed into `pending`: serde's
    /// jump to the anchor's events. An anchor still recording — an alias
    /// to the node being read — or one abandoned past serde's recursion
    /// budget is not bound: the first can only turn into serde's
    /// repetition limit, the second fails where serde's remaining-depth
    /// check would, and an unbound anchor never reaches here.
    fn resolve(&mut self, mut event: Ev) -> Result<Ev, ()> {
        while let Ev::Alias(id) = event {
            self.jumps += 1;
            let Some(span) = self.anchors.get(&id).cloned() else {
                return Err(());
            };
            self.replay(span)?;
            event = self.take()?;
        }
        Ok(event)
    }
    /// One span's worth of recorded events, pushed as a fresh frame:
    /// serde's `jump` hands each alias its own cursor, so a nested alias
    /// replays ahead of the outer frame's tail rather than behind it.
    /// The count is bounded: serde's own cap on this work scales with
    /// the document's size and is checked at stream end; `REPLAY_LIMIT`
    /// is the fixed ceiling beneath it that only an alias bomb reaches.
    fn replay(&mut self, span: Range<usize>) -> Result<(), ()> {
        self.replayed += span.len();
        if self.replayed > REPLAY_LIMIT {
            return Err(());
        }
        self.pending
            .push(self.buffer[span].iter().cloned().collect());
        Ok(())
    }
    /// One whole value, consumed without deserializing it: serde's
    /// `ignore_any`, which steps over aliases unresolved and never counts
    /// depth.
    fn skip(&mut self, first: Ev) -> Result<(), ()> {
        let mut open = match first {
            Ev::SeqStart(_) | Ev::MapStart(_) => 1usize,
            Ev::Scalar(..) | Ev::Alias(_) => return Ok(()),
            _ => return Err(()),
        };
        while open > 0 {
            match self.take()? {
                Ev::SeqStart(_) | Ev::MapStart(_) => open += 1,
                Ev::SeqEnd | Ev::MapEnd => open -= 1,
                Ev::Scalar(..) | Ev::Alias(_) => {}
                _ => return Err(()),
            }
        }
        Ok(())
    }
    /// The node starting with `first`, read to its end. What serde reads of
    /// a field or item; `depth` is serde's remaining recursion allowance.
    fn node(&mut self, first: Ev, depth: usize) -> Result<Node, ()> {
        match self.resolve(first)? {
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
fn scalar_value(text: String, style: ScalarStyle, tag: Option<String>) -> Result<Yaml, ()> {
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
    if style != ScalarStyle::Plain {
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
            if *style == ScalarStyle::Plain {
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
        && *style == ScalarStyle::Plain
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
        Node::Scalar(text, ScalarStyle::Plain, _) if text.is_empty() => Ok(&[]),
        _ => Err(()),
    }
}
fn items_of(node: &Node) -> Result<&[Node], ()> {
    match node {
        Node::Seq(_, items) => Ok(items),
        Node::Scalar(text, ScalarStyle::Plain, _) if text.is_empty() => Ok(&[]),
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
                        if style != ScalarStyle::Plain
                            && !(style == ScalarStyle::Literal
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
                if *style != ScalarStyle::Plain
                    && !(*style == ScalarStyle::Literal
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
    // A malformed document must fail this derive, not the worker: the
    // parser — like serde's own deserializer — has panic sites on inputs
    // its invariants never expected, and any panic inside becomes a
    // document error here rather than a `JoinError` upstream.
    catch_unwind(AssertUnwindSafe(|| helm_walk(body))).unwrap_or(Err(CacheError::Document))
}
fn helm_walk(body: &mut dyn Read) -> std::result::Result<Vec<u8>, CacheError> {
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

    impl std::fmt::Debug for Ev {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Ev::Scalar(t, s, _) => write!(f, "Scalar({t:?},{s:?})"),
                Ev::SeqStart(_) => write!(f, "SeqStart"),
                Ev::SeqEnd => write!(f, "SeqEnd"),
                Ev::MapStart(_) => write!(f, "MapStart"),
                Ev::MapEnd => write!(f, "MapEnd"),
                Ev::Alias(id) => write!(f, "Alias({id})"),
                Ev::DocumentStart => write!(f, "DocStart"),
                Ev::DocumentEnd => write!(f, "DocEnd"),
                Ev::StreamStart => write!(f, "StreamStart"),
                Ev::StreamEnd => write!(f, "StreamEnd"),
            }
        }
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
            // Tabs: after a `:`, as indentation, and inside scalars.
            b"entries:\n  c:\n    - version:\t1.0\n",
            b"entries:\n\tc:\n\t  - version: 1.0\n",
            b"entries:\n  c:\n    - version: 1\t0\n",
            b"entries: {c: {version:\t1.0}}\n",
            b"entries:\n  c:\n    - version: '1\t0'\n",
            b"entries:\n  c:\n    - version: \"1\t0\"\n",
            // Version directives, unknown directives, and %TAG.
            b"%YAML 1.1\n---\nentries: {}\n",
            b"%YAML 1.2\n---\nentries: {}\n",
            b"%YAML 2.0\n---\nentries: {}\n",
            b"%YAML 1.9\n---\nentries: {}\n",
            b"%YAML 1.1\n%YAML 1.1\n---\nentries: {}\n",
            b"%FOO directive\n---\nentries: {}\n",
            b"%FOO\n---\nentries: {}\n",
            b"%TAG ! tag:example.com,2020:\n---\nentries:\n  c:\n    - version: !x 1.0\n",
            b"%TAG !! tag:example.com,2020:\n---\nentries:\n  c:\n    - version: !!x 1.0\n",
            b"%TAG !e! tag:example.com,2020:\nentries: {}\n",
            b"%TAG !a! tag:a:\n%TAG !a! tag:b:\n---\nentries: {}\n",
            // U+2028 and U+2029 as separators and inside scalars.
            b"entries: {}\xe2\x80\xa8",
            b"entries: {}\xe2\x80\xa9",
            b"entries:\xe2\x80\xa8  c: []\n",
            b"entries:\n  c:\xe2\x80\xa9    - version: 1.0\n",
            b"entries:\n  c:\n    - version: \"a\xe2\x80\xa8b\"\n",
            b"entries:\n  c:\n    - version: 'a\xe2\x80\xa9b'\n",
            b"entries:\xe2\x80\xa8\xc2\x85  c: []\n",
            // NEL as a break and mid-scalar.
            b"entries: {}\xc2\x85",
            b"entries:\n  c:\n    - version: \"a\xc2\x85b\"\n",
            // CR, CRLF and LF line endings.
            b"entries:\r  c: []\r",
            b"entries:\r\n  c: []\r\n",
            b"entries:\n  c: []\r\n",
            b"entries:\r  c:\r\n    - version: 1.0\n",
            b"entries:\n\rc: []\n",
            // More control characters and noncharacters.
            b"entries: {}\x01",
            b"entries: {}\x1f",
            b"entries: {}\x7f",
            b"entries:\n  c:\n    - version: 1\x01.0\n",
            b"entries:\n  c:\n    - version: 'a\x7fb'\n",
            b"entries:\n  c:\n    - version: a\x0bb\n",
            b"entries:\xef\xbf\xbe {}\n",
            b"entries:\n  c:\n    - version: \xef\xbf\xbf\n",
            // UTF-16, both endians, with and without BOMs.
            b"\xff\xfee\x00n\x00t\x00r\x00i\x00e\x00s\x00:\x00 \x00{\x00}\x00\n\x00",
            b"\xfe\xff\x00e\x00n\x00t\x00r\x00i\x00e\x00s\x00:\x00 \x00{\x00}\x00\n",
            b"e\x00n\x00t\x00r\x00i\x00e\x00s\x00:\x00 \x00{\x00}\x00\n\x00",
            b"\x00e\x00n\x00t\x00r\x00i\x00e\x00s\x00:\x00 \x00{\x00}\x00\n",
            b"\xff\xfe\x00\x00e\x00n\x00t\x00r\x00i\x00e\x00s\x00:\x00 \x00{\x00}\x00",
            // Alias bombs with long tails, redefined anchors, alias of
            // alias, and merge keys behind aliases.
            b"base: &b\n  - x\n  - y\nx: &a [*b, *b]\nentries:\n  c:\n    - version: 1.0\n      sources: [*a, *a]\n",
            b"x: &a [s]\nentries:\n  c:\n    - version: 1.0\n      sources: *a\n      annotations: {k1: *a, k2: *a, k3: *a, k4: *a}\n",
            b"one: &a 1.0\ntwo: &a [1]\nthree: &a {k: v}\nentries:\n  c:\n    - version: *a\n",
            b"x: &b [*a]\ny: &a [s]\nentries:\n  c:\n    - version: 1.0\n      sources: *b\n",
            b"y: &a [s]\nx: &b [*a]\nentries:\n  c:\n    - version: 1.0\n      sources: *b\n",
            b"y: &a [s]\nx: &b *a\nentries:\n  c:\n    - version: 1.0\n      sources: *b\n",
            b"base: &m\n  version: 1.0\n  digest: d\nentries:\n  c:\n    - <<: *m\n",
            b"base: &m {version: 1.0, digest: d}\nentries:\n  c:\n    - <<: *m\n      version: 2.0\n",
            b"entries:\n  c:\n    - <<: [x, y]\n      version: 1.0\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {<<: *u}\n",
            // Duplicate keys spelled differently.
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {1: a, +1: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {'1': a, 1: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {1: a, '1': b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {null: a, NULL: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {true: a, 'true': b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {True: a, TRUE: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {0x1: a, +1: b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {'': a, '': b}\n",
            b"entries:\n  c:\n    - version: 1.0\n      annotations: {? ~: a, ? null: b}\n",
            // Anchors on empty scalars and anchored aliases.
            b"entries:\n  c:\n    - version: &e\n",
            b"x: &e\nentries:\n  c:\n    - version: *e\n",
            b"x: &a y\nz: &b *a\nentries:\n  c:\n    - version: *b\n",
        ];
        let mut failures = Vec::new();
        for (index, body) in cases.iter().enumerate() {
            check(&mut failures, body, &format!("table case {index}"));
        }
        // Flow nesting at and past yaml-rust2's old 255 cap, kept and
        // ignored, sequences and mappings.
        for depth in [249usize, 255, 256, 300] {
            let flow_seq = format!("{}x{}", "[".repeat(depth), "]".repeat(depth));
            let flow_map = format!("{}x{}", "{k: ".repeat(depth), "}".repeat(depth));
            let ignored = format!(
                "x: {flow_seq}
entries: {{}}
"
            )
            .into_bytes();
            check(
                &mut failures,
                &ignored,
                &format!("flow seq {depth} ignored"),
            );
            let kept = format!("entries:\n  c:\n    - version: 1.0\n      sources: {flow_seq}\n")
                .into_bytes();
            check(&mut failures, &kept, &format!("flow seq {depth} kept"));
            let ignored_map = format!(
                "x: {flow_map}
entries: {{}}
"
            )
            .into_bytes();
            check(
                &mut failures,
                &ignored_map,
                &format!("flow map {depth} ignored"),
            );
        }
        report(failures);

        // libyaml-safer's scanner panics where C libyaml emits the Tag
        // token: a tag immediately followed by `,` in a flow collection
        // (`scanner.rs`'s `panic!("TODO: What is expected here?")`). The
        // buffered parse accepts these; the walk's catch_unwind makes
        // them failed documents — an asserted residual, like the replay
        // cap, fail-closed and not a shape an honest index writes.
        let residuals: &[&[u8]] = &[
            b"entries: {}\nother: [!x, a]\n",
            b"entries: {}\nother: {version: !x, digest: d}\n",
        ];
        for (index, &body) in residuals.iter().enumerate() {
            assert!(
                helm_buffered(&mut &body[..]).is_ok(),
                "residual {index}: the buffered parse accepts"
            );
            for size in [usize::MAX, 1, 3, 7, 64] {
                let mut chunks = Chunks { body, size, at: 0 };
                assert!(
                    helm(&mut chunks).is_err(),
                    "residual {index} at chunk {size}: the walk fails the document"
                );
            }
        }
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
                // Sometimes redefine an earlier name: bindings shadow.
                let name = if self.rng.chance(15) && !self.anchors.is_empty() {
                    self.anchors[self.rng.below(self.anchors.len())].clone()
                } else {
                    let name = format!("a{}", self.next_anchor);
                    self.next_anchor += 1;
                    name
                };
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
                "a\tb",
                "1\t0",
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
                _ => (*self.rng.pick(&[
                    ">", "|", ">", "|", "&a9 *u1", "", " ", "?x", "\u{FEFF}", "\u{2028}",
                    "\u{2029}", "\t",
                ]))
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
                    out.push(if self.rng.chance(4) { '\t' } else { ' ' });
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
        /// A deeply nested collection, at a chosen depth: serde's kept
        /// values cap at 128, its ignored ones are unbounded, and a
        /// recorded anchor never keeps more than serde could deserialize.
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
            match self.rng.below(100) {
                0..=4 => out.push_str("%YAML 1.1\n"),
                5..=6 => out.push_str("%YAML 1.2\n"),
                7..=8 => out.push_str("%YAML 2.0\n"),
                9..=10 => out.push_str("%FOO directive\n"),
                11..=12 => out.push_str("%YAML 1.1\n%YAML 1.2\n"),
                _ => {}
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
                25..=30 if !self.anchors.is_empty() => {
                    // An alias bomb's long tail: many uses of one anchor.
                    let name = self.anchors[self.rng.below(self.anchors.len())].clone();
                    let uses = vec![format!("*{name}"); self.rng.below(80) + 2].join(", ");
                    out.push_str("tail: [");
                    out.push_str(&uses);
                    out.push_str("]\n");
                }
                _ => {}
            }
            if self.rng.chance(5) {
                out = out.replace('\n', if self.rng.chance(50) { "\r" } else { "\r\n" });
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
                13..=15 => {
                    bytes = String::from_utf8_lossy(&bytes)
                        .encode_utf16()
                        .flat_map(|u| u.to_be_bytes())
                        .collect();
                }
                16..=19 if !bytes.is_empty() => {
                    bytes.insert(self.rng.below(bytes.len()), 0);
                }
                20..=23 if !bytes.is_empty() => {
                    let at = self.rng.below(bytes.len());
                    bytes.splice(at..at, [0xEF, 0xBB, 0xBF]);
                }
                24..=28 if !bytes.is_empty() => {
                    let at = self.rng.below(bytes.len());
                    let splice: &[u8] = self.rng.pick(&[
                        &[0x01][..],
                        &[0x1f][..],
                        &[0x7f][..],
                        &[0x0b][..],
                        &[0xc2, 0x85][..],
                        &[0xe2, 0x80, 0xa8][..],
                        &[0xe2, 0x80, 0xa9][..],
                        &[0x09][..],
                        &[0x80][..],
                    ]);
                    bytes.splice(at..at, splice.iter().copied());
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
        "<<",
        "0x1",
        "+1",
        "1",
        "''",
    ];

    /// The seeded corpus: thousands of composed documents at a fixed seed,
    /// each checked whole and at every chunking.
    #[test]
    fn generated_documents_derive_what_the_buffered_parse_does() {
        const SEED: u64 = 0x9E3779B97F4A7C15;
        let mut rng = Rng(SEED);
        let mut failures = Vec::new();
        for index in 0..6000 {
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

    /// Deep anchored nodes — the case that could overflow a stack: a
    /// subtree nested ~3,000 deep recorded under an anchor, then replayed
    /// into a kept field or left alone. serde's recursion budget fails the
    /// kept uses and ignores the rest; the walk's recordings cap at the
    /// same depth, so replay, clone and drop stay shallow. Runs on a
    /// thread with the default 2 MB test stack, as in production.
    #[test]
    fn deep_anchored_nodes_agree_with_the_oracle() {
        let seq = |n: usize| format!("{}x", "- ".repeat(n));
        let map = |n: usize| format!("{}v{}", "{k:".repeat(n), "}".repeat(n));
        let chain = |n: usize| {
            let mut doc = String::from("x: &a1 [v]\n");
            for link in 2..=n {
                doc.push_str(&format!("x{link}: &a{link} [*a{}]\n", link - 1));
            }
            doc
        };
        let bodies: Vec<Vec<u8>> = [
            // The repro: a ~3,000-deep anchored sequence aliased into a
            // kept field — serde's remaining-depth check fails it.
            format!(
                "x: &a {}\nentries: {{c: [{{version: 1.0, sources: *a}}]}}\n",
                seq(3000)
            ),
            // A deep anchored mapping the same way.
            format!(
                "x: &a {}\nentries: {{c: [{{version: 1.0, sources: *a}}]}}\n",
                map(3000)
            ),
            // A deep chain of aliases through anchored sequences, used.
            format!(
                "{}entries: {{c: [{{version: 1.0, sources: *a3000}}]}}\n",
                chain(3000)
            ),
            // A chain short enough for serde's budget.
            format!(
                "{}entries: {{c: [{{version: 1.0, sources: *a100}}]}}\n",
                chain(100)
            ),
            // Ignored deep anchors nobody aliases still succeed.
            format!("x: &a {}\nentries: {{}}\n", seq(3000)),
            format!("x: &a {}\nentries: {{}}\n", map(3000)),
            // Ignored deep values, anchored or not.
            format!("x: {}\nentries: {{}}\n", seq(3000)),
            format!("entries:\n  x: &a {}\n  c: []\n", seq(3000)),
            // A kept deep value that is not anchored fails like serde.
            format!(
                "entries:\n  c:\n    - version: 1.0\n      sources: {}\n",
                seq(3000)
            ),
            format!(
                "entries:\n  c:\n    - version: 1.0\n      urls: {}\n",
                seq(3000)
            ),
            // An anchored node just inside the budget, used at depth.
            format!(
                "x: &a {}\nentries: {{c: [{{version: 1.0, sources: *a}}]}}\n",
                seq(100)
            ),
            // A deep anchored node aliased inside an ignored value.
            format!("x: &a {}\nignored: [*a]\nentries: {{}}\n", seq(3000)),
        ]
        .into_iter()
        .map(String::into_bytes)
        .collect();
        let failures = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let mut failures = Vec::new();
                for (index, body) in bodies.iter().enumerate() {
                    check(&mut failures, body, &format!("deep case {index}"));
                }
                failures
            })
            .unwrap()
            .join()
            .unwrap();
        report(failures);
    }

    /// What the walk still holds for anchors once the stream ends: the
    /// shared buffer's events and their scalars.
    fn retained(body: &[u8]) -> usize {
        let mut body = body;
        let mut walk = Walk::new(&mut body);
        while let Ok(event) = walk.take() {
            if matches!(event, Ev::StreamEnd) {
                break;
            }
        }
        walk.buffer
            .iter()
            .map(|event| match event {
                Ev::Scalar(text, _, tag) => {
                    size_of::<Ev>() + text.len() + tag.as_ref().map_or(0, String::len)
                }
                _ => size_of::<Ev>(),
            })
            .sum()
    }

    /// Anchored content is stored once however deeply the anchors nest:
    /// each recording is a span into the buffer every open recording
    /// shares, so a scalar inside 128 nested anchors — and inside an
    /// anchor holding many anchored siblings — is kept once, not once
    /// per enclosing span. Both shapes must agree with the oracle, and
    /// what the walk retains for anchors stays within a small factor of
    /// the input.
    #[test]
    fn nested_anchors_share_one_recording() {
        let scalar = "x".repeat(1 << 20);
        // 128 anchors each wrapping the next, at serde's recursion
        // budget, around a large scalar — then one used through an alias.
        let mut nested = String::from("x: &a1 [");
        for n in 2..=MAX_NODE_DEPTH {
            nested.push_str(&format!("&a{n} ["));
        }
        nested.push('"');
        nested.push_str(&scalar);
        nested.push('"');
        nested.push_str(&"]".repeat(MAX_NODE_DEPTH));
        nested.push_str("\nentries: {c: [{version: 1.0, sources: *a64}]}\n");
        // One big anchor holding many anchored siblings, each a large
        // scalar.
        let mut siblings = String::from("x: &big [");
        for n in 0..64 {
            siblings.push_str(&format!("&s{n} [\"{}\"], ", "y".repeat(1 << 14)));
        }
        siblings.push_str("null]\nentries: {c: [{version: 1.0, sources: *s32}]}\n");

        let mut failures = Vec::new();
        check(&mut failures, nested.as_bytes(), "nested anchors");
        check(&mut failures, siblings.as_bytes(), "sibling anchors");
        report(failures);
        for (case, body) in [("nested", &nested), ("siblings", &siblings)] {
            let retained = retained(body.as_bytes());
            assert!(
                retained <= body.len() * 2,
                "{case}: retained {retained} bytes for a {}-byte body",
                body.len()
            );
        }
    }

    /// Truncated and corrupted bodies still get a document answer: the
    /// parser's own panics are contained as a failed document, and the
    /// oracle agrees which side each one lands on.
    #[test]
    fn malformed_input_fails_the_document_never_the_process() {
        let body = b"apiVersion: v1\nentries:\n  c:\n    - version: 1.0\n      created: '2020-01-01'\n      digest: abc\n      deprecated: true\n      sources: [https://x.test/1]\n      annotations: {k: v}\n    - version: 2.0\n";
        let mut bodies: Vec<Vec<u8>> = (0..=body.len())
            .step_by(3)
            .map(|cut| body[..cut].to_vec())
            .collect();
        let mut rng = Rng(0x5EED_CAFE);
        for _ in 0..300 {
            let mut doc = body.to_vec();
            for _ in 0..rng.below(6) + 1 {
                let at = rng.below(doc.len() + 1);
                let splice: &[u8] = rng.pick(&[
                    &[0x00][..],
                    &[0xff][..],
                    &[0xfe][..],
                    &[0x80][..],
                    &[0x01][..],
                    &[0x7f][..],
                    &b"{"[..],
                    &b"}"[..],
                    &b"["[..],
                    &b":"[..],
                    &b"\xef\xbb\xbf"[..],
                    &b"\xe2\x80\xa8"[..],
                ]);
                doc.splice(at..at, splice.iter().copied());
            }
            bodies.push(doc);
        }
        let mut failures = Vec::new();
        for (index, doc) in bodies.iter().enumerate() {
            check(&mut failures, doc, &format!("malformed case {index}"));
        }
        report(failures);
    }

    /// serde's alias bound is a hundred jumps per document event — a bound
    /// the walk can only check once the count is final — while the walk's
    /// absolute ceiling on replayed events is `REPLAY_LIMIT`. A document
    /// serde accepts can still pass the ceiling: a small anchor used
    /// enough times stays under serde's bound yet exhausts the fixed one,
    /// and there the walk fails where serde would not. That is one of
    /// the two residual divergences, both fail-closed and not shapes an
    /// honest index writes:
    /// - the replay cap above: a document serde accepts can exhaust
    ///   `REPLAY_LIMIT` while staying under serde's per-event bound;
    /// - a tag immediately followed by `,` in a flow collection —
    ///   `other: [!x, a]`, `{version: !x, digest: d}` — where C libyaml
    ///   emits the Tag token but libyaml-safer's scanner panics, so the
    ///   buffered parse accepts and `helm()`'s catch_unwind fails the
    ///   document (asserted in the hand table's residuals).
    #[test]
    fn replay_work_past_the_cap_fails_closed() {
        // `[{k: v}]` replays six events per use; enough uses pass
        // REPLAY_LIMIT while staying under serde's hundred-jumps-per-event
        // bound on this document.
        let uses = 190_000;
        let body = format!(
            "x: &a [{{k: v}}]\nentries:\n  c:\n    - version: 1.0\n      sources: [{}]\n",
            vec!["*a"; uses].join(", ")
        )
        .into_bytes();
        assert!(helm_buffered(&mut &body[..]).is_ok());
        assert!(helm(&mut &body[..]).is_err());
        // And a long but ordinary alias chain both sides take.
        let ok = format!(
            "x: &a [{{k: v}}]\nentries:\n  c:\n    - version: 1.0\n      sources: [{}]\n",
            vec!["*a"; 1_000].join(", ")
        )
        .into_bytes();
        let mut failures = Vec::new();
        check(&mut failures, &ok, "replay under the cap");
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
