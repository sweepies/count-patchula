//! The hidden markers the Count writes into GitHub bodies and issues, with
//! one typed reader and writer per kind. What he writes is frozen: released
//! versions persist these bytes, so writers reproduce them exactly and
//! readers accept every form a release has written. Each kind's reading rule
//! comes from where he writes it, so text others wrote — which he preserves
//! around his own sections — can invalidate a read but never be taken for
//! his marker.
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// `<!-- patchula:` followed by his ownership JSON and ` -->`, closing his
/// section of a pull request body.
pub(crate) const OWNERSHIP: &str = "<!-- patchula:";
/// `<!-- patchula-begin -->`, opening his section. Bodies written before it
/// existed have none; his section then runs from the first byte.
pub(crate) const START: &str = "<!-- patchula-begin -->";
/// `<!-- patchula-recheck -->`, ending the tick box that asks for a re-check.
pub(crate) const RECHECK: &str = "<!-- patchula-recheck -->";
/// `<!-- patchula-closed:obsolete -->`, the last line of a body he closed
/// because the update was superseded.
pub(crate) const OBSOLETE: &str = "<!-- patchula-closed:obsolete -->";
/// `<!-- patchula-closed:batched -->`, the last line of a body he closed
/// because the update moved into a batch PR.
pub(crate) const BATCHED: &str = "<!-- patchula-closed:batched -->";
/// `<!-- patchula-batch-add -->`, ending the tick box that adds an update's
/// PR to the batch.
pub(crate) const ADD: &str = "<!-- patchula-batch-add -->";
/// `<!-- patchula-batch-remove:`, followed by an item origin and ` -->`,
/// ending a batch item's tick box.
pub(crate) const REMOVE: &str = "<!-- patchula-batch-remove:";
/// `<!-- patchula-targets:`, followed by a JSON list of changes and ` -->`,
/// inside his section.
pub(crate) const TARGETS: &str = "<!-- patchula-targets:";
/// `<!-- patchula-batch:`, followed by a JSON list of items and ` -->`,
/// inside his section of the batch PR's body.
pub(crate) const ITEMS: &str = "<!-- patchula-batch:";
/// `<!-- patchula-content:`, followed by a hash and ` -->`, inside his
/// section; while it matches, his rendering of the same inputs is kept.
pub(crate) const CONTENT: &str = "<!-- patchula-content:";
/// `<!-- patchula-age-permits:v1 `, followed by a JSON set of target hashes
/// and ` -->`, alone on its line inside his section.
pub(crate) const PERMITS: &str = "<!-- patchula-age-permits:v1 ";
/// `<!-- patchula-action:`, followed by a JSON action and ` -->`, at the end
/// of a tick box on the dashboard issue.
pub(crate) const ACTION: &str = "<!-- patchula-action:";
/// `<!-- patchula-dashboard:v1 -->`, a whole line of the dashboard issue.
pub const DASHBOARD: &str = "<!-- patchula-dashboard:v1 -->";
/// `<!-- patchula-server:`, followed by a server URL and ` -->`, alone on
/// its line.
pub(crate) const SERVER: &str = "<!-- patchula-server:";
/// The byte length of a content marker: its prefix, a 64-digit hash and
/// ` -->`.
pub(crate) const CONTENT_MARKER_LEN: usize = CONTENT.len() + 64 + " -->".len();
/// The largest JSON payload a hidden marker may carry: anything bigger
/// could not have come from a body he wrote, which GitHub caps far below.
pub(crate) const PAYLOAD: usize = 65_536;

/// Who a pull request belongs to and which update it carries, as his
/// ownership marker records it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Marker {
    pub version: u32,
    pub identity: String,
    pub update: String,
}

impl Marker {
    /// His marker for `identity` and `update`.
    pub(crate) fn new(identity: String, update: String) -> Self {
        Self {
            version: 1,
            identity,
            update,
        }
    }
}

/// His ownership marker for `marker`, exactly the bytes releases write.
pub(crate) fn ownership(marker: &Marker) -> String {
    format!(
        "{OWNERSHIP}{} -->",
        serde_json::to_string(marker).expect("marker")
    )
}

/// The span of his ownership marker in `body`: the marker, its start and
/// its end. The rule is the body's only `<!-- patchula:` occurrence must
/// parse as version 1 with two 64-digit hashes, payload at most 1024 bytes.
/// A second occurrence — his own marker quoted into preserved foreign text
/// — invalidates the read rather than being taken for his: a copied marker
/// can never pose as the real one.
fn ownership_span(body: &str) -> Option<(Marker, usize, usize)> {
    let start = body.find(OWNERSHIP)?;
    let rest = &body[start + OWNERSHIP.len()..];
    if rest.contains(OWNERSHIP) {
        return None;
    }
    let (json, _) = rest.split_once(" -->")?;
    if json.len() > 1024 {
        return None;
    }
    let m: Marker = serde_json::from_str(json).ok()?;
    let hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if m.version != 1 || !hash(&m.identity) || !hash(&m.update) {
        return None;
    }
    Some((
        m,
        start,
        start + OWNERSHIP.len() + json.len() + " -->".len(),
    ))
}

/// The update key his ownership marker records.
pub(crate) fn ownership_marker(body: &str) -> Option<Marker> {
    ownership_span(body).map(|(m, _, _)| m)
}

/// A body split by the Count's markers: the text before his section, the
/// section itself, and the text after it. His section starts at the last
/// start marker before his ownership marker: text others wrote above it may
/// quote the marker, but his own section cannot hold one, because rendered
/// upstream text never keeps an HTML comment. A body with no start marker
/// (one written before it existed) is his from its first byte.
pub(crate) fn regions(body: &str) -> Option<(&str, &str, &str)> {
    let (_, marker_start, end) = ownership_span(body)?;
    let start = body[..marker_start].rfind(START).unwrap_or(0);
    Some((&body[..start], &body[start..end], &body[end..]))
}

/// His section of a pull request body, when the body carries his markers.
pub(crate) fn section(body: &str) -> Option<&str> {
    regions(body).map(|(_, section, _)| section)
}

/// JSON in an HTML comment, `<` and `>` escaped so a value can neither end
/// the comment nor forge a marker. Payloads that cannot contain them
/// (hashes, versions) serialize identically with or without it, so every
/// writer shares this one rule.
pub(crate) fn hidden<T: Serialize>(prefix: &str, value: &T) -> String {
    let json = serde_json::to_string(value)
        .expect("serializable marker")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e");
    format!("{prefix}{json} -->")
}

/// The first `prefix` payload in `text` as JSON: between `prefix` and the
/// next ` -->`, at most `cap` bytes, and rejected when a second `prefix`
/// follows — a quoted copy invalidates the read, it never impersonates.
pub(crate) fn hidden_json<T: DeserializeOwned>(text: &str, prefix: &str, cap: usize) -> Option<T> {
    let (_, rest) = text.split_once(prefix)?;
    if rest.contains(prefix) {
        return None;
    }
    let (json, _) = rest.split_once(" -->")?;
    if json.len() > cap {
        return None;
    }
    serde_json::from_str(json).ok()
}

/// [`hidden_json`] scoped to his section of a pull request body: markers he
/// writes inside it cannot be quoted from the foreign text around it.
pub(crate) fn section_json<T: DeserializeOwned>(body: &str, prefix: &str, cap: usize) -> Option<T> {
    hidden_json(section(body)?, prefix, cap)
}

/// The payload of the one line in his section that starts with `prefix`
/// and ends with ` -->`, at most `cap` bytes — how the age-permits marker
/// is read: alone on its line, inside his section, never quoted.
pub(crate) fn section_line<'a>(body: &'a str, prefix: &str, cap: usize) -> Option<&'a str> {
    let mut lines = section(body)?
        .lines()
        .filter_map(|l| l.strip_prefix(prefix).and_then(|l| l.strip_suffix(" -->")));
    let json = lines.next()?;
    (lines.next().is_none() && json.len() <= cap).then_some(json)
}

/// The payload between `prefix` and the ` -->` ending `line`, at most `cap`
/// bytes — the action marker's place at the end of a tick box.
pub(crate) fn line_end<'a>(line: &'a str, prefix: &str, cap: usize) -> Option<&'a str> {
    let (_, rest) = line.split_once(prefix)?;
    rest.strip_suffix(" -->").filter(|p| p.len() <= cap)
}

/// Whether `body` carries his dashboard marker: exactly one whole line
/// equal to it.
pub fn dashboard(body: &str) -> bool {
    body.lines().filter(|l| *l == DASHBOARD).count() == 1
}

/// Whether `body`'s last line is `marker` — how he records a closed update:
/// he appends it when he closes the PR, and nothing he writes follows it.
pub(crate) fn closed(body: &str, marker: &str) -> bool {
    body.lines().next_back() == Some(marker)
}

/// The server URL his marker advertises: exactly one whole line of the
/// body equal to `<!-- patchula-server:URL -->`, the URL nonempty and free
/// of whitespace. He always writes it alone on its line; a copy quoted
/// into foreign prose is not a line and a second marker line is ambiguous,
/// so neither can select the server. The CLI still validates it as a URL.
pub(crate) fn server(body: &str) -> Option<&str> {
    let mut found = None;
    for line in body.lines() {
        let Some(url) = line
            .strip_prefix(SERVER)
            .and_then(|l| l.strip_suffix(" -->"))
        else {
            continue;
        };
        if found.is_some() || url.is_empty() || url.contains(char::is_whitespace) {
            return None;
        }
        found = Some(url);
    }
    found
}

/// Every checkbox line outside fenced code: whether it is ticked, and its
/// text after the box. The one reader of the Count's checkboxes, so the
/// dashboard, PR and batch controls agree on what is ticked and what is code.
pub(crate) fn checkboxes(text: &str) -> Vec<(bool, &str)> {
    let mut fence = None;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            let kind = &line[..3];
            if fence == Some(kind) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(kind);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let Some((state, rest)) = line.strip_prefix("- [").and_then(|s| s.split_once("] ")) else {
            continue;
        };
        match state {
            "x" | "X" => out.push((true, rest)),
            " " => out.push((false, rest)),
            _ => {}
        }
    }
    out
}
/// The text of ticked checkboxes outside fenced code.
pub(crate) fn ticked_lines(text: &str) -> Vec<&str> {
    checkboxes(text)
        .into_iter()
        .filter_map(|(ticked, rest)| ticked.then_some(rest))
        .collect()
}

/// `text` cut to at most `len` bytes on a char boundary.
pub(crate) fn cut_bytes(text: &str, len: usize) -> &str {
    if text.len() <= len {
        return text;
    }
    let mut end = len;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `body` cut to `limit` bytes with his section and markers kept whole:
/// only the foreign text around them is truncated, the text before his
/// section first, then what follows it.
pub(crate) fn truncate_marked(body: &str, limit: usize) -> String {
    let Some((before, section, after)) = regions(body) else {
        return cut_bytes(body, limit).to_owned();
    };
    let room = limit.saturating_sub(section.len());
    let before = cut_bytes(before, room);
    let after = cut_bytes(after, room - before.len());
    format!("{before}{section}{after}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes() -> (String, String) {
        ("a".repeat(64), "b".repeat(64))
    }

    /// A body as a release writes it: foreign text, his section opened by
    /// the start marker and closed by the ownership marker.
    fn body(extra: &str) -> String {
        let (identity, update) = hashes();
        format!(
            "someone else's words\n{START}\nnotes{extra}\n{OWNERSHIP}{{\"version\":1,\"identity\":\"{identity}\",\"update\":\"{update}\"}} -->\n"
        )
    }

    #[test]
    fn writers_pin_the_bytes_releases_write() {
        let (identity, update) = hashes();
        assert_eq!(
            ownership(&Marker::new(identity.clone(), update.clone())),
            format!(
                "{OWNERSHIP}{{\"version\":1,\"identity\":\"{identity}\",\"update\":\"{update}\"}} -->"
            )
        );
        assert_eq!(
            hidden("<!-- x:", &vec!["<tag>".to_string()]),
            "<!-- x:[\"\\u003ctag\\u003e\"] -->"
        );
        assert_eq!(hidden("<!-- h:", &"plain"), "<!-- h:\"plain\" -->");
        assert_eq!(DASHBOARD, "<!-- patchula-dashboard:v1 -->");
        assert_eq!(
            format!("{SERVER}{} -->", "https://count.example"),
            "<!-- patchula-server:https://count.example -->"
        );
    }

    #[test]
    fn ownership_reads_the_released_form_and_nothing_else() {
        let text = body("");
        let m = ownership_marker(&text).unwrap();
        assert_eq!(m.version, 1);
        assert_eq!(m.identity, "a".repeat(64));
        assert_eq!(m.update, "b".repeat(64));
        // A second occurrence — his marker quoted into preserved text —
        // invalidates the read rather than impersonating it.
        let quoted = format!(
            "{text}\n{OWNERSHIP}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
            "c".repeat(64),
            "d".repeat(64)
        );
        assert_eq!(ownership_marker(&quoted), None);
        // Released readers accepted only version 1 with two 64-digit hashes.
        for bad in [
            format!(
                "{OWNERSHIP}{{\"version\":2,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
                "a".repeat(64),
                "b".repeat(64)
            ),
            format!(
                "{OWNERSHIP}{{\"version\":1,\"identity\":\"short\",\"update\":\"{}\"}} -->",
                "b".repeat(64)
            ),
            format!(
                "{OWNERSHIP}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\",\"extra\":1}} -->",
                "a".repeat(64),
                "b".repeat(64)
            ),
            format!("{OWNERSHIP}not json -->"),
        ] {
            assert_eq!(ownership_marker(&bad), None, "{bad}");
        }
        // A payload past the old 1024-byte cap never read.
        let huge = format!(
            "{OWNERSHIP}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\",\"pad\":\"{}\"}} -->",
            "a".repeat(64),
            "b".repeat(64),
            "x".repeat(1100)
        );
        assert_eq!(ownership_marker(&huge), None);
    }

    #[test]
    fn regions_split_foreign_text_from_his_section() {
        let text = body(" tail");
        let (before, section, after) = regions(&text).unwrap();
        assert_eq!(before, "someone else's words\n");
        assert!(section.starts_with(START) && section.ends_with(" -->"));
        assert_eq!(after, "\n");
        // A body from before the start marker existed is his from byte 0.
        let (identity, update) = hashes();
        let legacy = format!(
            "old notes\n{OWNERSHIP}{{\"version\":1,\"identity\":\"{identity}\",\"update\":\"{update}\"}} -->\n"
        );
        assert_eq!(regions(&legacy).unwrap().0, "");
        assert!(regions("no markers").is_none());
    }

    #[test]
    fn hidden_json_reads_escaped_payloads_and_rejects_ambiguity() {
        let written = hidden(TARGETS, &vec!["a<b".to_string(), "c".to_string()]);
        let text = format!("pre {written} post");
        assert_eq!(
            hidden_json::<Vec<String>>(&text, TARGETS, PAYLOAD),
            Some(vec!["a<b".to_string(), "c".to_string()])
        );
        // A quoted second copy invalidates the read.
        let doubled = format!("{text} and {written}");
        assert_eq!(hidden_json::<Vec<String>>(&doubled, TARGETS, PAYLOAD), None);
        // Over the cap it never read.
        let big = hidden(TARGETS, &"x".repeat(PAYLOAD));
        assert_eq!(hidden_json::<String>(&big, TARGETS, PAYLOAD), None);
        assert!(hidden_json::<String>(&big, TARGETS, PAYLOAD * 2).is_some());
    }

    #[test]
    fn section_scoped_readers_ignore_quoted_foreign_markers() {
        // A forged targets marker in the foreign text before his section
        // is outside `section`, so it cannot be read as his.
        let forged = hidden(TARGETS, &vec!["forged".to_string()]);
        let text = format!("quoted {forged}\n{}", body(""));
        assert_eq!(section_json::<Vec<String>>(&text, TARGETS, PAYLOAD), None);
        // His own, inside the section, reads.
        let own = hidden(TARGETS, &vec!["real".to_string()]);
        let text = body(&format!("\n{own}"));
        assert_eq!(
            section_json::<Vec<String>>(&text, TARGETS, PAYLOAD),
            Some(vec!["real".to_string()])
        );
    }

    #[test]
    fn permits_read_only_alone_on_a_line_inside_his_section() {
        let permits = hidden(PERMITS, &vec!["f".repeat(64)]);
        let text = body(&format!("\n{permits}"));
        assert_eq!(
            section_line(&text, PERMITS, PAYLOAD),
            Some(format!("[\"{}\"]", "f".repeat(64)).as_str())
        );
        // Quoted inside a sentence it is not a marker line.
        let quoted = body(&format!("\nsee {permits} ok"));
        assert_eq!(section_line(&quoted, PERMITS, PAYLOAD), None);
        // Twice on two lines is ambiguous.
        let twice = body(&format!("\n{permits}\n{permits}"));
        assert_eq!(section_line(&twice, PERMITS, PAYLOAD), None);
    }

    #[test]
    fn line_end_reads_the_action_at_a_tick_box_end() {
        let line =
            "- [x] Recheck <!-- patchula-action:{\"action\":\"recheck\",\"target\":\"abc\"} -->";
        assert_eq!(
            line_end(line, ACTION, 256),
            Some("{\"action\":\"recheck\",\"target\":\"abc\"}")
        );
        // Not at the line's end, or over the cap, it is not his action.
        assert_eq!(line_end(&format!("{line} trailing"), ACTION, 256), None);
        assert_eq!(line_end(line, ACTION, 8), None);
        assert_eq!(line_end("- [x] plain", ACTION, 256), None);
    }

    #[test]
    fn dashboard_needs_exactly_one_whole_line() {
        assert!(dashboard(&format!("intro\n{DASHBOARD}\noutro")));
        assert!(!dashboard("no marker"));
        assert!(!dashboard(&format!("{DASHBOARD}\n{DASHBOARD}")));
        assert!(!dashboard(&format!("x {DASHBOARD}")));
        assert!(!dashboard(&format!("{DASHBOARD} ")));
    }

    #[test]
    fn closed_reads_only_the_last_line() {
        assert!(closed(&format!("body\n{OBSOLETE}"), OBSOLETE));
        assert!(!closed(&format!("{OBSOLETE}\nbody"), OBSOLETE));
        assert!(!closed(&format!("body {OBSOLETE}"), OBSOLETE));
        assert!(closed(&format!("body\n{BATCHED}"), BATCHED));
    }

    #[test]
    fn server_reads_one_whole_line() {
        let text = format!("run there\n{SERVER}https://count.example -->\n");
        assert_eq!(server(&text), Some("https://count.example"));
        assert_eq!(server("none"), None);
        // Quoted inline in prose it is not a marker line.
        assert_eq!(
            server(&format!("see {SERVER}https://evil --> inline")),
            None
        );
        // Text after the marker on its line breaks the whole-line form.
        assert_eq!(server(&format!("{SERVER}https://a --> trailing")), None);
        assert_eq!(server(&format!("x {SERVER}https://a -->")), None);
        // Two marker lines are ambiguous, good or bad.
        assert_eq!(
            server(&format!("{SERVER}https://a -->\n{SERVER}https://b -->")),
            None
        );
        assert_eq!(
            server(&format!("{SERVER} -->\n{SERVER}https://a -->")),
            None
        );
        assert_eq!(server(&format!("{SERVER}has space -->")), None);
        assert_eq!(server(&format!("{SERVER} -->")), None);
    }

    #[test]
    fn checkboxes_skip_fences_and_only_count_real_boxes() {
        let text = "- [ ] open\n- [x] done <!-- patchula-action:{} -->\n- [X] caps\n\
                    - [~] not his\n* [x] not his shape\n```\n- [x] in code\n```\n~~~\n- [ ] tilde code\n~~~\n\
                    - [x] after";
        let ticked = ticked_lines(text);
        assert_eq!(ticked.len(), 3);
        assert!(ticked[0].starts_with("done "));
        assert_eq!(ticked[1], "caps");
        assert_eq!(ticked[2], "after");
        let states: Vec<_> = checkboxes(text).into_iter().collect();
        assert_eq!(states[0], (false, "open"));
        assert_eq!(states[3], (true, "after"));
    }

    #[test]
    fn cut_and_truncate_marked_never_sever_his_markers() {
        assert_eq!(cut_bytes("héllo", 3), "hé");
        assert_eq!(cut_bytes("abc", 10), "abc");
        let text = body(" foreign tail");
        // Tight enough to cut the foreign text, generous enough for his
        // section: the markers come through whole and the result fits.
        let limit = section(&text).unwrap().len() + 10;
        let truncated = truncate_marked(&text, limit);
        assert!(truncated.len() <= limit);
        assert!(ownership_marker(&truncated).is_some());
        assert!(section(&truncated).is_some());
        // A limit below his section keeps the section whole anyway.
        let truncated = truncate_marked(&text, 10);
        assert_eq!(truncated, section(&text).unwrap());
        // No markers at all: a plain cut.
        assert_eq!(truncate_marked("abcdefgh", 3), "abc");
    }
}
