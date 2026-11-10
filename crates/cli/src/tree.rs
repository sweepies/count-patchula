//! What `git status` reports about the working tree: the one dirty-tree
//! check every caller shares.

/// The `git status` invocation every dirty-tree check uses: porcelain v1,
/// NUL-separated so paths need no quoting, every untracked file listed.
pub const STATUS_ARGS: [&str; 4] = ["status", "--porcelain=v1", "-z", "--untracked-files=all"];

/// The paths a `STATUS_ARGS` listing names, relative to the Git root. A
/// rename or copy entry is followed by its old path, which is skipped; the
/// result is sorted and deduplicated.
pub fn changed(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = text.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..) else {
            continue;
        };
        paths.push(path.to_owned());
        // A rename is followed by its old path.
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_lists_every_path_once_sorted() {
        let text = " M src/a.rs\0?? new/file.txt\0M  src/b.rs\0";
        assert_eq!(changed(text), ["new/file.txt", "src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn renames_and_copies_skip_their_old_path() {
        let text = "R  src/new.rs\0src/old.rs\0C  dst.txt\0src.txt\0MM src/new.rs\0?? other.txt\0";
        assert_eq!(changed(text), ["dst.txt", "other.txt", "src/new.rs"]);
    }

    #[test]
    fn entries_too_short_to_be_a_status_are_skipped() {
        assert_eq!(changed("ab\0 M src/x\0"), ["src/x"]);
    }
}
