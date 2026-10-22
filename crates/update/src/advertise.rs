//! A server's URL, hidden in the PRs and dashboard the Count writes, so
//! `patchula` in a checkout can offer to run there.
const PREFIX: &str = "<!-- patchula-server:";
const SUFFIX: &str = " -->";

/// The marker line for `url`, which the server has already validated.
pub fn marker(url: &str) -> String {
    format!("{PREFIX}{url}{SUFFIX}")
}

/// The URL in `body`'s marker. The CLI still validates it as a server URL.
pub fn find(body: &str) -> Option<&str> {
    let (_, rest) = body.split_once(PREFIX)?;
    let (url, _) = rest.split_once(SUFFIX)?;
    (!url.is_empty() && !url.contains(char::is_whitespace)).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_marker_round_trips_and_junk_is_ignored() {
        let body = format!("text\n{}\nmore", marker("https://patchula.acme.dev"));
        assert_eq!(find(&body), Some("https://patchula.acme.dev"));
        assert_eq!(find("<!-- patchula-server: -->"), None);
        assert_eq!(find("<!-- patchula-server:a b -->"), None);
        assert_eq!(find("<!-- patchula-server:https://x"), None);
        assert_eq!(find("no marker"), None);
    }
}
