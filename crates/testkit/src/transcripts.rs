//! Request transcripts for the stateful GitHub fakes in the engine and
//! update test suites. A fake reports every request it answers to a log on
//! the calling thread; `check` hashes that log and compares it with a
//! checked-in snapshot, so moving a fake cannot silently change the
//! requests a test sees. Regenerate the snapshots with
//! `mise run transcripts:snapshot`.
//!
//! Requests are recorded in arrival order, but the snapshot hashes the
//! sorted lines: `apply_all` and the lookup fan-outs run requests through
//! `buffer_unordered`, so the interleaving of concurrent requests is
//! scheduler-dependent even when every request is identical. What the
//! snapshot pins is the multiset of requests and each request's content;
//! order is only meaningful where the code under test issues requests
//! sequentially, and there the multiset still pins the sequence's members.

use serde_json::Value;
use sha2::Digest as _;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
};

/// Set to `1` to rewrite the transcript snapshots instead of checking them.
pub const UPDATE_VAR: &str = "PATCHULA_UPDATE_TRANSCRIPTS";

struct Log(Vec<String>);

thread_local! {
    /// Requests recorded on this thread, one normalized line each, in the
    /// order the fake answered them. Tests run one per thread, so the log
    /// is the test's transcript.
    static TRANSCRIPT: RefCell<Log> = const { RefCell::new(Log(Vec::new())) };
}

/// Record one request a fake is about to answer. Called from the fake's
/// `Transport::send` on the test's own thread; current-thread runtimes keep
/// every request on it.
pub fn record(request: &reqwest::Request, body: &Value) {
    let url = request.url();
    let mut target = url.host_str().unwrap_or_default().to_owned();
    if let Some(port) = url.port() {
        target.push(':');
        target.push_str(&port.to_string());
    }
    target.push_str(url.path());
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let graphql = if url.path() == "/graphql" {
        body.get("query").and_then(Value::as_str).unwrap_or("-")
    } else {
        "-"
    };
    let body = serde_json::to_string(body).unwrap_or_default();
    let line = format!("{}\t{target}\t{graphql}\t{body}", request.method());
    TRANSCRIPT.with(|log| log.borrow_mut().0.push(normalize(&line)));
}

/// Wall-clock timestamps are the volatile value request bodies can carry:
/// RFC 3339 times (`merged_at` inputs, `eligible_at` fields) and the
/// "12 Nov 00:54 UTC" / "12 Nov 2026 00:54 UTC" renderings of
/// `update::text::when`/`when_unix` in dashboard and PR bodies. `Utc::now`
/// is real time even under tokio's paused clock, so these change every
/// run; every other byte is fixed by the test. Anything else time- or
/// randomness-derived would fail the stability runs, not slip through.
fn normalize(line: &str) -> String {
    static TIMESTAMP: OnceLock<regex::Regex> = OnceLock::new();
    TIMESTAMP
        .get_or_init(|| {
            regex::Regex::new(concat!(
                r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})",
                r"|\d{1,2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2} UTC",
                r"|\d{1,2} [A-Z][a-z]{2} \d{2}:\d{2} UTC",
            ))
            .expect("static timestamp regex")
        })
        .replace_all(line, "<timestamp>")
        .into_owned()
}

fn digest(lines: &[String]) -> String {
    let mut lines = lines.to_vec();
    lines.sort();
    let transcript = lines.join("\n");
    let hash = sha2::Sha256::digest(transcript.as_bytes());
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_snapshot(path: &Path) -> BTreeMap<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => panic!(
            "cannot read transcript snapshot {}: {error}",
            path.display()
        ),
    };
    text.lines()
        .map(|line| {
            line.split_once('\t')
                .map(|(test, hash)| (test.to_owned(), hash.to_owned()))
                .unwrap_or_else(|| {
                    panic!("malformed transcript snapshot line in {}", path.display())
                })
        })
        .collect()
}

fn verify(path: &Path, test: &str, hash: &str) {
    match read_snapshot(path).remove(test) {
        Some(expected) if expected == hash => {}
        Some(expected) => panic!(
            "request transcript for `{test}` changed\n  {} expects {expected}, got {hash}\nif the change is intended run `mise run transcripts:snapshot`",
            path.display()
        ),
        None => panic!(
            "no request transcript recorded for `{test}` in {}\nif the test is new run `mise run transcripts:snapshot`",
            path.display()
        ),
    }
}

fn update(path: &Path, test: &str, hash: &str) {
    // Tests in one binary run on parallel threads; the file is read,
    // merged and rewritten under one lock so the result is sorted and
    // complete regardless of completion order. Cargo runs test binaries
    // one at a time, so no two processes write the same snapshot.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut lines = read_snapshot(path);
    lines.insert(test.to_owned(), hash.to_owned());
    let mut text = String::new();
    for (test, hash) in &lines {
        text.push_str(test);
        text.push('\t');
        text.push_str(hash);
        text.push('\n');
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", parent.display()));
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .unwrap_or_else(|error| panic!("cannot create transcript temporary file: {error}"));
    file.write_all(text.as_bytes()).unwrap_or_else(|error| {
        panic!(
            "cannot write transcript snapshot {}: {error}",
            path.display()
        )
    });
    file.persist(path).unwrap_or_else(|error| {
        panic!(
            "cannot persist transcript snapshot {}: {}",
            path.display(),
            error.error
        )
    });
}

/// Compare this test's request transcript with its snapshot line, or
/// rewrite the snapshot when `PATCHULA_UPDATE_TRANSCRIPTS` is set. `path`
/// is the snapshot file beside the test binary, conventionally
/// `tests/transcripts.tsv` under the crate's manifest directory.
pub fn check(path: &Path) {
    let requests = TRANSCRIPT.with(|log| std::mem::take(&mut log.borrow_mut().0));
    let test = std::thread::current()
        .name()
        .expect("libtest runs each test on a thread named for it")
        .to_owned();
    let hash = digest(&requests);
    if std::env::var_os(UPDATE_VAR).is_some() {
        update(path, &test, &hash);
    } else {
        verify(path, &test, &hash);
    }
}
