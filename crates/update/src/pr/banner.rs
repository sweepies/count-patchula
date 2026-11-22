//! The banner at the top of the Count's section while he rebuilds a branch
//! where the wait is real: a ticked re-check box, or a rebuild that
//! regenerates lockfiles. The rewrite that ends the rebuild drops it, and
//! any body still carrying one is rewritten, so a run that stopped
//! mid-rebuild never leaves it behind.
use crate::markers;
use std::borrow::Cow;

/// Why the Count is rebuilding, which picks the banner's wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rebuild {
    /// Someone ticked the PR's re-check box.
    Recheck,
    /// The rebuild regenerates lockfiles with the real tools.
    Lockfiles,
}

/// The banner image, pinned to the release tag of the running build so a
/// PR written by an older release keeps showing the image it was written
/// with.
pub(crate) const IMAGE: &str = concat!(
    "https://raw.githubusercontent.com/sweepies/count-patchula/v",
    env!("CARGO_PKG_VERSION"),
    "/assets/rebasing.svg"
);

/// The banner for `rebuild` on a branch based on `base`, ending in a blank
/// line so the section's own text follows as its own block.
pub(crate) fn banner(rebuild: Rebuild, base: &str) -> String {
    let base = crate::text::code(patchula_datasource::upstream::truncate(base, 240));
    let doing = match rebuild {
        Rebuild::Recheck => format!("re-checking it against {base}"),
        Rebuild::Lockfiles => format!("rebuilding it on {base} and regenerating its lockfiles"),
    };
    format!(
        "{}\n> <img src=\"{IMAGE}\" alt=\"🦷 rebasing…\" width=\"80\" height=\"32\"> **The Count is sinking his teeth into this branch:** {doing}…\n{}\n\n",
        markers::REBASING,
        markers::REBASING_END,
    )
}

/// Whether his section of `body` carries a banner, or a stray line of one:
/// a whole line equal to either marker, CRLF or not.
pub(crate) fn shown(body: &str) -> bool {
    markers::section(body).is_some_and(|section| section.split_inclusive('\n').any(marker_line))
}

/// Whether `line` is a banner marker on a line of its own.
fn marker_line(line: &str) -> bool {
    let line = bare(line);
    line == markers::REBASING || line == markers::REBASING_END
}

/// `line` without its line ending.
fn bare(line: &str) -> &str {
    line.strip_suffix('\n')
        .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line))
}

/// `body` with `banner` at the top of his section, right after the start
/// marker's line, and everything else byte for byte as it was. The banner
/// takes the start line's ending, so a body GitHub keeps with CRLF stays
/// CRLF. `None` when the body has no section of his, already shows a
/// banner, or would pass GitHub's limit with it.
pub(crate) fn with_banner(body: &str, banner: &str) -> Option<String> {
    let (before, section, after) = markers::regions(body)?;
    if shown(body) || body.len() + 2 * banner.len() > BANNER_LIMIT {
        return None;
    }
    let (head, banner) = match section.strip_prefix(markers::START) {
        Some(rest) if rest.starts_with("\r\n") => (
            markers::START.len() + 2,
            Cow::Owned(banner.replace('\n', "\r\n")),
        ),
        Some(rest) if rest.starts_with('\n') => (markers::START.len() + 1, Cow::Borrowed(banner)),
        // An edit took the line break after the start marker away: the
        // banner opens on its own line all the same, and the start marker
        // keeps the line break once the banner comes off.
        Some(_) => (markers::START.len(), Cow::Owned(format!("\n{banner}"))),
        None => (0, Cow::Borrowed(banner)),
    };
    let (head, rest) = section.split_at(head);
    Some(format!("{before}{head}{banner}{rest}{after}"))
}

/// `body` without any banner in his section; unchanged when it has none.
/// Every banner block — its opening line, its one quoted line, its closing
/// line and the blank line after — comes off whole. A marker line that
/// bounds no such block, edited or quoted into his section, comes off on
/// its own: whatever stands between two stray markers is never removed.
pub(crate) fn without_banner(body: &str) -> Cow<'_, str> {
    let Some((before, section, after)) = markers::regions(body) else {
        return Cow::Borrowed(body);
    };
    if !shown(body) {
        return Cow::Borrowed(body);
    }
    let lines: Vec<&str> = section.split_inclusive('\n').collect();
    let mut kept = String::with_capacity(section.len());
    let mut at = 0;
    while at < lines.len() {
        let block = bare(lines[at]) == markers::REBASING
            && lines.get(at + 1).is_some_and(|l| bare(l).starts_with("> "))
            && lines
                .get(at + 2)
                .is_some_and(|l| bare(l) == markers::REBASING_END);
        if block {
            at += 3;
            // The blank line that separates the banner from his text.
            if lines
                .get(at)
                .is_some_and(|l| bare(l).is_empty() && l.ends_with('\n'))
            {
                at += 1;
            }
            continue;
        }
        if !marker_line(lines[at]) {
            kept.push_str(lines[at]);
        }
        at += 1;
    }
    Cow::Owned(format!("{before}{kept}{after}"))
}

/// GitHub's pull request body limit, with room to spare: a banner that
/// would take a body past it is left out rather than cut.
const BANNER_LIMIT: usize = 65_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(section: &str) -> String {
        format!(
            "{section}{}{} -->\n",
            markers::OWNERSHIP,
            serde_json::json!({"version":1,"identity":"a".repeat(64),"update":"b".repeat(64)})
        )
    }

    #[test]
    fn the_banner_sits_at_the_top_of_his_section_and_comes_off_without_a_trace() {
        let section = owned(&format!("{}\n| Package |\n", markers::START));
        for (before, after) in [("", ""), ("Someone's note.\n\n", "\n\nA tool's footer.")] {
            let body = format!("{before}{section}{after}");
            let banner = banner(Rebuild::Recheck, "main");
            let shown_body = with_banner(&body, &banner).unwrap();
            assert!(shown(&shown_body));
            assert!(
                shown_body.starts_with(&format!(
                    "{before}{}\n{}\n",
                    markers::START,
                    markers::REBASING
                )),
                "{shown_body}"
            );
            assert!(shown_body.ends_with(after));
            // Never twice.
            assert_eq!(with_banner(&shown_body, &banner), None);
            assert_eq!(without_banner(&shown_body), body);
            assert!(matches!(without_banner(&body), Cow::Borrowed(_)));
        }
        // A body from before the start marker existed is his from byte 0.
        let legacy = owned("| Package |\n");
        let shown_body = with_banner(&legacy, &banner(Rebuild::Lockfiles, "main")).unwrap();
        assert!(shown_body.starts_with(markers::REBASING));
        assert_eq!(without_banner(&shown_body), legacy);
        // No section of his, nothing to put it in.
        assert_eq!(with_banner("A human's PR.", "x"), None);
    }

    #[test]
    fn a_banner_quoted_outside_his_section_is_not_his() {
        let quoted = format!(
            "{}\n> quoted\n{}\n\n",
            markers::REBASING,
            markers::REBASING_END
        );
        let body = format!(
            "{quoted}{}",
            owned(&format!("{}\n| Package |\n", markers::START))
        );
        assert!(!shown(&body));
        assert_eq!(without_banner(&body), body);
        assert!(with_banner(&body, &banner(Rebuild::Recheck, "main")).is_some());
    }

    #[test]
    fn a_banner_missing_its_closing_line_still_comes_off() {
        let body = owned(&format!(
            "{}\n{}\n> edited\n| Package |\n",
            markers::START,
            markers::REBASING
        ));
        let cleared = without_banner(&body);
        assert!(!shown(&cleared));
        assert!(cleared.contains("> edited\n| Package |"));
    }

    #[test]
    fn crlf_bodies_take_and_lose_the_banner_without_a_trace() {
        for section in [
            format!("{}\r\n| Package |\r\n", markers::START),
            format!("{}\n| Package |\n", markers::START),
        ] {
            let body = format!("A note.\r\n\r\n{}\r\nA footer.", owned(&section));
            let shown_body = with_banner(&body, &banner(Rebuild::Recheck, "main")).unwrap();
            let crlf = section.contains('\r');
            assert!(
                shown_body.contains(&if crlf {
                    format!("{}\r\n{}\r\n", markers::START, markers::REBASING)
                } else {
                    format!("{}\n{}\n", markers::START, markers::REBASING)
                }),
                "{shown_body:?}"
            );
            assert!(shown(&shown_body));
            assert_eq!(without_banner(&shown_body), body);
        }
        // GitHub turning an LF banner into CRLF still lets it come off.
        let body = owned(&format!("{}\n| Package |\n", markers::START));
        let crlf = with_banner(&body, &banner(Rebuild::Lockfiles, "main"))
            .unwrap()
            .replace('\n', "\r\n");
        assert_eq!(without_banner(&crlf), body.replace('\n', "\r\n"));
    }

    #[test]
    fn the_banner_opens_its_own_line_even_when_the_start_marker_lost_its_line_break() {
        let glued = owned(&format!("{}| Package |\n", markers::START));
        let shown_body = with_banner(&glued, &banner(Rebuild::Recheck, "main")).unwrap();
        assert!(
            shown_body.starts_with(&format!("{}\n{}\n", markers::START, markers::REBASING)),
            "{shown_body}"
        );
        assert!(shown(&shown_body));
        let cleared = without_banner(&shown_body);
        assert!(!shown(&cleared) && !cleared.contains("sinking his teeth"));
        assert_eq!(
            cleared,
            owned(&format!("{}\n| Package |\n", markers::START))
        );
        // Once on its own line, the start marker round-trips exactly.
        let again = with_banner(&cleared, &banner(Rebuild::Recheck, "main")).unwrap();
        assert_eq!(without_banner(&again), cleared);
    }

    #[test]
    fn every_banner_comes_off_and_stray_markers_never_take_text_with_them() {
        let one = banner(Rebuild::Recheck, "main");
        let body = owned(&format!("{}\n{one}{one}| Package |\n", markers::START));
        let cleared = without_banner(&body);
        assert!(!shown(&cleared), "{cleared}");
        assert_eq!(
            cleared,
            owned(&format!("{}\n| Package |\n", markers::START))
        );
        // Markers quoted around his table: only the marker lines go.
        let quoted = owned(&format!(
            "{}\n{}\n| Package |\n<!-- patchula-content:x -->\n{}\n",
            markers::START,
            markers::REBASING,
            markers::REBASING_END
        ));
        let cleared = without_banner(&quoted);
        assert!(!shown(&cleared));
        assert!(cleared.contains("| Package |\n<!-- patchula-content:x -->\n"));
        // A marker inside a line of prose is no banner at all.
        let prose = owned(&format!(
            "{}\nSee `{}` in the docs.\n",
            markers::START,
            markers::REBASING
        ));
        assert!(!shown(&prose));
        assert!(matches!(without_banner(&prose), Cow::Borrowed(_)));
    }

    #[test]
    fn wording_follows_the_trigger_and_the_image_is_pinned_to_this_release() {
        let recheck = banner(Rebuild::Recheck, "main");
        assert!(recheck.contains(
            "**The Count is sinking his teeth into this branch:** re-checking it against `main`…"
        ));
        let lockfiles = banner(Rebuild::Lockfiles, "trunk");
        assert!(lockfiles.contains("rebuilding it on `trunk` and regenerating its lockfiles…"));
        for text in [&recheck, &lockfiles] {
            assert!(text.contains(&format!(
                "<img src=\"https://raw.githubusercontent.com/sweepies/count-patchula/v{}/assets/rebasing.svg\" alt=\"🦷 rebasing…\" width=\"80\" height=\"32\">",
                env!("CARGO_PKG_VERSION")
            )));
            assert!(!text.contains(markers::RECHECK) && !text.contains("- [x]"));
        }
        // A branch name cannot end the comment or break out of its span.
        assert!(banner(Rebuild::Recheck, "a`b-->").contains("``a`b--\u{200b}>``"));
    }
}
