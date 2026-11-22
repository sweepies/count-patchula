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
        "{}\n> <img src=\"{IMAGE}\" alt=\"🦷 rebasing…\" width=\"24\" height=\"24\"> **The Count is sinking his teeth into this branch:** {doing}…\n{}\n\n",
        markers::REBASING,
        markers::REBASING_END,
    )
}

/// Whether his section of `body` carries a banner.
pub(crate) fn shown(body: &str) -> bool {
    markers::section(body).is_some_and(|section| section.contains(markers::REBASING))
}

/// `body` with `banner` at the top of his section, right after the start
/// marker, and everything else byte for byte as it was. `None` when the
/// body has no section of his, already shows a banner, or would pass
/// GitHub's limit with it.
pub(crate) fn with_banner(body: &str, banner: &str) -> Option<String> {
    let (before, section, after) = markers::regions(body)?;
    if section.contains(markers::REBASING) || body.len() + banner.len() > BANNER_LIMIT {
        return None;
    }
    let head = section.strip_prefix(markers::START).map_or(0, |rest| {
        markers::START.len() + usize::from(rest.starts_with('\n'))
    });
    let (head, rest) = section.split_at(head);
    Some(format!("{before}{head}{banner}{rest}{after}"))
}

/// `body` without the banner in his section; unchanged when it has none.
/// A banner whose closing line was edited away loses its opening line, so
/// it no longer counts as shown.
pub(crate) fn without_banner(body: &str) -> Cow<'_, str> {
    let Some((before, section, after)) = markers::regions(body) else {
        return Cow::Borrowed(body);
    };
    let Some(start) = section.find(markers::REBASING) else {
        return Cow::Borrowed(body);
    };
    let end = section[start..]
        .find(markers::REBASING_END)
        .map(|at| {
            let end = start + at + markers::REBASING_END.len();
            let rest = &section[end..];
            end + if rest.starts_with("\n\n") {
                2
            } else {
                usize::from(rest.starts_with('\n'))
            }
        })
        .unwrap_or_else(|| {
            let end = start + markers::REBASING.len();
            end + usize::from(section[end..].starts_with('\n'))
        });
    Cow::Owned(format!(
        "{before}{}{}{after}",
        &section[..start],
        &section[end..]
    ))
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
    fn wording_follows_the_trigger_and_the_image_is_pinned_to_this_release() {
        let recheck = banner(Rebuild::Recheck, "main");
        assert!(recheck.contains(
            "**The Count is sinking his teeth into this branch:** re-checking it against `main`…"
        ));
        let lockfiles = banner(Rebuild::Lockfiles, "trunk");
        assert!(lockfiles.contains("rebuilding it on `trunk` and regenerating its lockfiles…"));
        for text in [&recheck, &lockfiles] {
            assert!(text.contains(&format!(
                "<img src=\"https://raw.githubusercontent.com/sweepies/count-patchula/v{}/assets/rebasing.svg\" alt=\"🦷 rebasing…\" width=\"24\" height=\"24\">",
                env!("CARGO_PKG_VERSION")
            )));
            assert!(!text.contains(markers::RECHECK) && !text.contains("- [x]"));
        }
        // A branch name cannot end the comment or break out of its span.
        assert!(banner(Rebuild::Recheck, "a`b-->").contains("``a`b--\u{200b}>``"));
    }
}
