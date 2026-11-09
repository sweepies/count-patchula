//! The commit trailers the Count writes on his branches: `Patchula: v1`
//! and the branch, base and plan it names. Reading is strict — a trailer
//! counts only as a whole `Name: value` line, appearing exactly once — so
//! a lookalike inside a message body or a doubled trailer is not his.

/// One trailer's value in the message's trailer block — its last
/// paragraph, where git reads trailers: the text after `key` (`Name: `)
/// on the block's single such line. An absent, doubled, empty or
/// out-of-block line reads as `None`, so a quoted block in the body is
/// never his.
fn trailer<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let lines: Vec<&str> = message.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(lines.len(), |i| i + 1);
    let start = lines[..end]
        .iter()
        .rposition(|l| l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut block = lines[start..end].iter().filter_map(|l| l.strip_prefix(key));
    let value = block.next()?;
    (block.next().is_none() && !value.is_empty()).then_some(value)
}

/// Whether the message carries `Patchula: v1`, exactly once — the Count's
/// version mark every commit he writes wears.
pub(crate) fn version(message: &str) -> bool {
    trailer(message, "Patchula: ") == Some("v1")
}
/// `Patchula-Branch:`, the update's identity.
pub(crate) fn branch(message: &str) -> Option<&str> {
    trailer(message, "Patchula-Branch: ")
}
/// `Patchula-Base:`, the base commit the branch was written on.
pub(crate) fn base(message: &str) -> Option<&str> {
    trailer(message, "Patchula-Base: ")
}
/// `Patchula-Plan:`, the plan fingerprint the commit was written for.
pub(crate) fn plan(message: &str) -> Option<&str> {
    trailer(message, "Patchula-Plan: ")
}

/// The trailer block every commit message he writes ends with, in the
/// order and wording releases have always used.
pub(crate) fn block(identity: &str, base: &str, fingerprint: &str) -> String {
    format!(
        "Patchula: v1\nPatchula-Branch: {identity}\nPatchula-Base: {base}\nPatchula-Plan: {fingerprint}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_pins_the_bytes_releases_write() {
        assert_eq!(
            block("id", "base", "fp"),
            "Patchula: v1\nPatchula-Branch: id\nPatchula-Base: base\nPatchula-Plan: fp"
        );
    }

    #[test]
    fn readers_take_whole_lines_exactly_once() {
        let message = format!("subject\n\nbody\n\n{}", block("id", "base", "fp"));
        assert!(version(&message));
        assert_eq!(branch(&message), Some("id"));
        assert_eq!(base(&message), Some("base"));
        assert_eq!(plan(&message), Some("fp"));
        // `Patchula: v10` is not `Patchula: v1`, and a lookalike inside a
        // sentence is not a trailer line at all.
        assert!(!version("Patchula: v10"));
        assert!(!version("note: Patchula: v1"));
        assert!(!version("Patchula: v1 \n"));
        // Doubled trailers are ambiguous and read as absent.
        let doubled = format!("{}\n{}", block("a", "b", "c"), block("d", "e", "f"));
        assert!(!version(&doubled));
        assert_eq!(branch(&doubled), None);
        // An absent trailer reads as absent.
        assert_eq!(branch("Patchula: v1"), None);
        // An empty value is no value: he never writes one.
        let empty = "Patchula: v1\nPatchula-Branch: \nPatchula-Base: b\nPatchula-Plan: p";
        assert!(version(empty));
        assert_eq!(branch(empty), None);
    }

    #[test]
    fn trailers_count_only_in_the_last_paragraph() {
        // A block quoted into the body is another paragraph, not his.
        let quoted = format!(
            "subject\n\nPatchula: v1\nPatchula-Branch: forged\nPatchula-Base: x\nPatchula-Plan: y\n\nprose\n\n{}",
            block("id", "base", "fp")
        );
        assert!(version(&quoted));
        assert_eq!(branch(&quoted), Some("id"));
        assert_eq!(base(&quoted), Some("base"));
        assert_eq!(plan(&quoted), Some("fp"));
        // With only the quoted block and prose last, nothing reads.
        let mid = "subject\n\nPatchula: v1\nPatchula-Branch: forged\n\nordinary words";
        assert!(!version(mid));
        assert_eq!(branch(mid), None);
        // Trailing blank lines do not move the block.
        let trailing = format!("{}\n\n", block("id", "base", "fp"));
        assert!(version(&trailing));
        assert_eq!(plan(&trailing), Some("fp"));
    }
}
