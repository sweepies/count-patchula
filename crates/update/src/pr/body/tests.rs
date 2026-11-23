//! Unit tests for the pure half of body writing: the sections, the
//! closures, body reuse and the room `Draft` leaves the fetches.
use super::*;
use crate::markers;
use crate::plan::testing::{self, action, change};
use crate::{Change, Plan};
use patchula_github::model;
use patchula_words::Recheck;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// A plan on `branch` with the marker identity the parser insists on.
fn update_plan(branch: &str, changes: Vec<Change>) -> Plan {
    testing::Draft::new(action(branch))
        .update()
        .identity("a".repeat(64))
        .changes(changes)
        .build()
}
/// `Bodies` for a run: re-check on the next run, no server, no permits.
fn bodies<'a>(permits: &'a BTreeMap<String, BTreeSet<String>>) -> Bodies<'a> {
    Bodies {
        recheck: Recheck::NextRun,
        server: None,
        branch_permits: permits,
        maintenance_group: "maintenance",
    }
}
/// A REST pull request carrying `text` as its body.
fn pr(text: &str) -> model::PullRequest {
    serde_json::from_value(json!({
        "number": 1, "node_id": "PR_node1",
        "html_url": "https://github.com/o/r/pull/1",
        "title": "update", "state": "open", "body": text,
        "labels": [], "user": {"login": "patchula[bot]"},
        "head": {"ref": "patchula/a-1.x", "sha": "head", "repo": {"full_name": "o/r"}},
        "base": {"ref": "main", "repo": {"full_name": "o/r"}}
    }))
    .expect("a complete REST pull request")
}

/// The refs a relock advanced join the content hash, so a new moved ref
/// rewrites the body.
#[test]
fn moved_refs_join_the_content_hash() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let file = patchula_report::FileAction {
        path: ".github/workflows/actions.lock".into(),
        blob_sha: String::new(),
        diff: String::new(),
    };
    let plan = |moved: Vec<patchula_lockfiles::MovedRef>| {
        testing::Draft::new(action("patchula/lock-file-maintenance"))
            .maintenance()
            .identity("a".repeat(64))
            .file(file.clone())
            .moved_refs(moved)
            .build()
    };
    let (_, quiet_marker) = bodies.intent(&plan(Vec::new()));
    let (_, marker) = bodies.intent(&plan(vec![patchula_lockfiles::MovedRef {
        workflow: ".github/workflows/ci.yml".into(),
        dependency: "actions/checkout@v7".into(),
        from: "a".repeat(40),
        to: "b".repeat(40),
    }]));
    assert_ne!(marker, quiet_marker);
}

/// A relock that removes `actions.lock` changes the content hash, so the
/// body is rewritten to say so.
#[test]
fn a_removed_actions_lock_joins_the_content_hash() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let file = patchula_report::FileAction {
        path: ".github/workflows/actions.lock".into(),
        blob_sha: String::new(),
        diff: String::new(),
    };
    let draft = || {
        testing::Draft::new(action("patchula/lock-file-maintenance"))
            .maintenance()
            .identity("a".repeat(64))
            .file(file.clone())
    };
    let (_, kept_marker) = bodies.intent(&draft().build());
    let (_, removed_marker) =
        bodies.intent(&draft().delete(".github/workflows/actions.lock").build());
    assert_ne!(removed_marker, kept_marker);
}

/// The closure texts: what the PR said, cut to the marker's budget, then
/// the marker that says why it closed.
#[test]
fn closures_keep_the_body_within_its_budget_and_end_with_their_marker() {
    for (why, marker) in [
        (Closure::Obsolete, markers::OBSOLETE),
        (Closure::Batched, markers::BATCHED),
    ] {
        assert_eq!(closure("a body", why), format!("a body\n\n{marker}"));
        let text = closure(&"x".repeat(FOREIGN_LIMIT), why);
        assert!(text.ends_with(marker), "{text}");
        assert_eq!(
            text.len(),
            closure_budget(marker) + "\n\n".len() + marker.len()
        );
        // A rebasing banner still up does not outlive the closure.
        let permits = BTreeMap::new();
        let finished = bodies(&permits)
            .draft(&update_plan("patchula/a-1.x", vec![change("a")]))
            .finish();
        let shown = crate::pr::banner::with_banner(
            &finished,
            &crate::pr::banner::banner(crate::pr::banner::Rebuild::Recheck, "main"),
        )
        .unwrap();
        assert_eq!(closure(&shown, why), closure(&finished, why));
    }
}

/// `reusable` needs the run's content marker in his section, no ticked
/// box, no retry and the ownership marker naming the plan.
#[test]
fn a_body_is_reusable_only_when_nothing_it_records_changed() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let plan = update_plan("patchula/a-1.x", vec![change("a")]);
    let (_, marker) = bodies.intent(&plan);
    let finished = bodies.draft(&plan).finish();
    assert!(bodies.reusable(&plan, Some(&pr(&finished)), &marker));
    // No existing PR, a ticked box, a different update and a different
    // content marker each write the body again.
    assert!(!bodies.reusable(&plan, None, &marker));
    let ticked = pr(&finished.replacen("- [ ]", "- [x]", 1));
    assert!(!bodies.reusable(&plan, Some(&ticked), &marker));
    let other = update_plan("patchula/b-1.x", vec![change("b")]);
    assert!(!bodies.reusable(&other, Some(&pr(&finished)), &marker));
    assert!(!bodies.reusable(&plan, Some(&pr(&finished)), "<!-- patchula-content:0 -->"));
    let retry = testing::Draft::from(plan).retry(true).build();
    assert!(!bodies.reusable(&retry, Some(&pr(&finished)), &marker));
}

/// `reserved` counts what `finish` keeps beside the visible text: the start
/// marker line, the tail and the closing room, so a tail that cannot fit is refused before
/// any request builds the body.
#[test]
fn the_reserved_tail_is_what_finish_appends() {
    let permits = BTreeMap::new();
    let plan = update_plan("patchula/a-1.x", vec![change("a")]);
    let base = bodies(&permits).reserved(&plan);
    let expected = fixed(
        bodies(&permits)
            .tail(
                &plan,
                &" ".repeat(markers::CONTENT_MARKER_LEN),
                &controls(&plan),
                None,
            )
            .len(),
        0,
    );
    assert_eq!(base, expected);
    // A permit marker joins the tail and `reserved` grows by exactly its
    // bytes.
    let mut with_permits = BTreeMap::new();
    with_permits.insert(
        "patchula/a-1.x".to_owned(),
        ["pkg:1.2.3".to_owned()].into_iter().collect(),
    );
    let marker = crate::dashboard::permit_marker(with_permits.get("patchula/a-1.x").unwrap());
    assert_eq!(
        bodies(&with_permits).reserved(&plan),
        base + marker.len() + 1
    );
    // A tail bigger than the body budget is what `reserved` exists to
    // refuse before the body is written.
    let huge: BTreeSet<String> = (0..3_000).map(|n| format!("pkg-{n}:1.0.0")).collect();
    let mut bursting = BTreeMap::new();
    bursting.insert("patchula/a-1.x".to_owned(), huge);
    assert!(bodies(&bursting).reserved(&plan) > BODY_LIMIT);
}

/// `Draft::next` asks for a fetch only while the body has room for its
/// notes: eight releases, or less than a thousand bytes left.
#[test]
fn the_draft_stops_asking_when_the_body_has_no_room() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let notes = patchula_notes::Notes::default();
    // Ten eligible changes: eight fetches, then the room-count stop.
    let plan = update_plan(
        "patchula/many-1.x",
        (0..10).map(|n| change(&format!("pkg-{n}"))).collect(),
    );
    let mut draft = bodies.draft(&plan);
    let mut fetched = Vec::new();
    while let Some(key) = draft.next() {
        fetched.push(key);
        draft.push(&Fetched::Read(notes.clone()));
    }
    assert_eq!(fetched.len(), 8);
    assert!(
        draft
            .finish()
            .contains("Out of room for more release notes")
    );
    // Two eligible changes and a near-full intent: one fetch, then the
    // byte-room stop answers `None` with one fetch still wanted.
    let mut fat = change("a");
    fat.body_notes = vec!["n".repeat(21_000)];
    let plan = update_plan("patchula/a-1.x", vec![fat, change("b")]);
    let mut draft = bodies.draft(&plan);
    let big = patchula_notes::Notes {
        source: patchula_notes::Repository::from_url("https://github.com/o/r"),
        sections: vec![patchula_notes::Section {
            version: "1.1.0".into(),
            link: String::new(),
            body: "a release note line.\n".repeat(2_000),
        }],
        ..Default::default()
    };
    assert!(draft.next().is_some());
    draft.push(&Fetched::Read(big));
    assert!(draft.next().is_none());
    assert!(
        draft
            .finish()
            .contains("Out of room for more release notes")
    );
}

/// A failed release-notes fetch leaves a body that is not reused: the next
/// run fetches again. A lasting failure settles.
#[test]
fn a_failed_notes_fetch_is_never_reused() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let plan = update_plan("patchula/a-1.x", vec![change("a")]);
    let (_, marker) = bodies.intent(&plan);
    let mut draft = bodies.draft(&plan);
    assert!(draft.next().is_some());
    draft.push(&Fetched::Transient);
    assert!(draft.unsettled());
    let failed = draft.finish();
    assert!(!bodies.reusable(&plan, Some(&pr(&failed)), &marker));
    let mut draft = bodies.draft(&plan);
    assert!(draft.next().is_some());
    draft.push(&Fetched::Lasting(None));
    let unresolved = draft.finish();
    assert!(bodies.reusable(&plan, Some(&pr(&unresolved)), &marker));
}

/// A lasting failure after the source resolved keeps the source's link,
/// and the body settles.
#[test]
fn a_resolved_source_with_unreadable_notes_keeps_its_link() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let plan = update_plan("patchula/a-1.x", vec![change("a")]);
    let (_, marker) = bodies.intent(&plan);
    let mut draft = bodies.draft(&plan);
    assert!(draft.next().is_some());
    let source = patchula_notes::Repository::from_url("https://github.com/o/r").unwrap();
    draft.push(&Fetched::Lasting(Some(source)));
    let body = draft.finish();
    assert!(body.contains("(https://github.com/o/r)"), "{body}");
    assert!(bodies.reusable(&plan, Some(&pr(&body)), &marker));
}

/// `unticked` clears the re-check box and leaves the rest of the body.
#[test]
fn unticking_clears_only_the_recheck_box() {
    let line = format!(
        "- [x] Re-check this PR on the next run {}\n",
        markers::RECHECK
    );
    let body = format!("before\n{line}- [x] Tick the batch.\nafter\n");
    let unticked = unticked(&body);
    assert!(unticked.contains("- [ ] Re-check this PR"));
    assert!(unticked.contains("- [x] Tick the batch."));
    assert!(unticked.starts_with("before\n") && unticked.ends_with("after\n"));
}

/// `title` is the commit message on one line inside GitHub's limit;
/// `same_owner` compares the ownership markers two bodies carry.
#[test]
fn the_title_is_one_line_and_ownership_compares_whole_markers() {
    let mut long = "m".repeat(300);
    long.insert(3, '\n');
    let plan = testing::Draft::new(action("b")).message(long).build();
    let title = title(&plan);
    assert!(!title.contains('\n'));
    assert_eq!(title.len(), 250);
    let (identity, update) = ("a".repeat(64), "b".repeat(64));
    let marker = markers::ownership(&markers::Marker::new(identity, update.clone()));
    let same = markers::ownership(&markers::Marker::new("c".repeat(64), update));
    assert!(same_owner(
        &format!("text\n{marker}"),
        &format!("other\n{marker}")
    ));
    assert!(!same_owner(
        &format!("text\n{marker}"),
        &format!("other\n{same}")
    ));
    assert!(!same_owner(&format!("text\n{marker}"), "none here"));
    assert!(same_owner("no markers", "none here"));
}

mod security_tests {
    use super::security_section;

    fn change(package: &str, advisory: patchula_report::Advisory) -> crate::Change {
        crate::Change {
            registry: None,
            source: patchula_words::Source::Npm,
            package: package.into(),
            file: "package.json".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some("1.0.1".into()),
            version: Some("1.0.1".into()),
            digest: None,
            kind: patchula_words::Update::Patch,
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: vec![advisory],
            image: None,
        }
    }

    /// An advisory a package's changes share is listed once, a pipe in its
    /// summary cannot end the table cell, and the collapsed table is closed.
    #[test]
    fn fixed_advisories_are_listed_once_each() {
        let advisory: patchula_report::Advisory = serde_json::from_value(serde_json::json!({
            "id":"GHSA-jf85-cpcp-j695","aliases":["CVE-2019-10744","SNYK-JS-LODASH-450202"],
            "severity":"critical","summary":"Prototype | pollution",
            "url":"https://osv.dev/vulnerability/GHSA-jf85-cpcp-j695"
        }))
        .unwrap();
        let change = change("lodash", advisory);
        let mut other = change.clone();
        other.file = "web/package.json".into();
        let (head, details) = security_section(&[change.clone(), other]);
        assert!(!head.is_empty());
        assert_eq!(
            details.matches("[GHSA-jf85-cpcp-j695]").count(),
            1,
            "{details}"
        );
        assert!(details.contains("Prototype \\| pollution"), "{details}");
        assert!(details.ends_with("</details>\n"), "{details}");
        let mut plain = change;
        plain.advisories.clear();
        assert_eq!(security_section(&[plain]), (String::new(), String::new()));
    }

    #[test]
    fn advisory_text_and_package_names_never_mention_anyone() {
        let advisory: patchula_report::Advisory = serde_json::from_value(serde_json::json!({
            "id":"GHSA-x","severity":"high",
            "summary":"Reported by @mallory, see #12 and https://github.com/a/b/issues/3",
            "url":"https://osv.dev/vulnerability/GHSA-x"
        }))
        .unwrap();
        let mut change = change("@scope/pkg", advisory);
        change.body_notes = vec!["Held for @mallory".into()];
        let (head, details) = security_section(&[change]);
        let text = format!("{head}{details}");
        for forged in ["@mallory", " #12", "//github.com/a/b/issues/3"] {
            assert!(!text.contains(forged), "{forged}: {text}");
        }
        assert!(text.contains("@\u{200d}mallory"), "{text}");
        // Package names are data, not prose.
        assert!(text.contains("`@scope/pkg`"), "{text}");
        assert!(super::prose_cell("@mallory").contains("@\u{200d}mallory"));
        assert!(
            crate::batch::render(&[crate::batch::Item {
                origin: "a".repeat(64),
                number: Some(1),
                title: "update @mallory/pkg".into(),
                targets: vec![],
            }])
            .contains("update @\u{200d}mallory/pkg")
        );
    }
}

mod image_tests {
    use super::{BODY_LIMIT, IMAGE_CAP, IMAGE_TABLE_CAP, IMAGE_WARNING, image_section};
    use patchula_report::{ImageAdvisory, ImageCheck};

    fn change(package: &str, check: ImageCheck) -> crate::Change {
        crate::Change {
            registry: None,
            source: patchula_words::Source::Docker,
            package: package.into(),
            file: "Dockerfile".into(),
            current: format!("1.0@sha256:{}", "4c49fc0".repeat(9)),
            current_version: None,
            value: Some("1.0".into()),
            version: Some("1.0".into()),
            digest: Some(format!("sha256:{}", "b".repeat(64))),
            kind: patchula_words::Update::Digest,
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: Some(check),
        }
    }
    fn state(state: &str) -> ImageCheck {
        ImageCheck {
            state: serde_json::from_value(serde_json::json!(state)).unwrap(),
            ..ImageCheck::default()
        }
    }

    /// An image section past its budget stays within `IMAGE_CAP`, its table
    /// within `IMAGE_TABLE_CAP`, and a full table still leaves the lines
    /// after it their room. The warning callout stays whole, the headline
    /// ends on a block break so the update table starts its own block, the
    /// collapsed table is closed, and the whole body fits.
    #[test]
    fn an_oversized_image_section_keeps_its_limits() {
        let mut check = state("checked");
        check.fixed = (0..400)
            .map(|n| ImageAdvisory {
                id: format!("DSA-{n}"),
                package: "openssl".into(),
                old: "3.0.1".into(),
                new: Some("3.0.2".into()),
                platforms: vec!["linux/amd64".into()],
            })
            .collect();
        let mut fixing = change("debian", check);
        // Every even advisory has a record and gets a row; the odd ones are
        // listed by id.
        fixing.advisories = (0..400)
            .step_by(2)
            .map(|n| {
                serde_json::from_value(serde_json::json!({
                    "id": format!("DSA-{n}"), "severity": "high",
                    "url": format!("https://osv.dev/vulnerability/DSA-{n}")
                }))
                .unwrap()
            })
            .collect();
        // Enough images without an SBOM that their warnings overflow too.
        let mut changes = vec![fixing];
        changes.extend((0..80).map(|n| change(&format!("image-{n}"), state("no-sbom"))));
        let (head, details) = image_section(&changes);
        assert!(head.len() + details.len() <= IMAGE_CAP);
        assert!(head.ends_with("\n\n"), "{:?}", &head[head.len() - 200..]);
        assert!(head.contains(IMAGE_WARNING), "{head}");
        assert!(details.ends_with("</details>\n"), "{details}");
        let table = details.find("| Advisory |").unwrap();
        let listed = details.find("\nAnd 200 more listed by OSV: ").unwrap();
        assert!(listed - table <= IMAGE_TABLE_CAP, "{}", listed - table);
        assert!(listed - table > IMAGE_TABLE_CAP - 300, "{}", listed - table);
        // The listed line is whole, and the warnings start but are cut.
        assert!(details.contains(" not listed here.\n"), "{details}");
        assert!(head.matches("publishes no SBOM").count() < 80, "{head}");
        // In the whole body the update table opens a block of its own.
        let permits = std::collections::BTreeMap::new();
        let plan = super::update_plan("patchula/debian-1.x", changes);
        let (intent, _) = super::bodies(&permits).intent(&plan);
        assert!(
            intent.contains("\n\n| Package | Change | Type | File |\n"),
            "{intent}"
        );
        let body = super::bodies(&permits).draft(&plan).finish();
        assert!(body.len() <= BODY_LIMIT, "{}", body.len());
    }

    /// Only advisories with a record get a row, most severe first; the rest
    /// are counted by id.
    #[test]
    fn only_advisories_with_a_record_get_a_row_and_the_rest_are_counted_ids() {
        let advisory = |id: &str, severity: &str| -> patchula_report::Advisory {
            serde_json::from_value(serde_json::json!({
                "id": id, "severity": severity, "url": format!("https://osv.dev/vulnerability/{id}")
            }))
            .unwrap()
        };
        let mut check = state("checked");
        check.fixed = (1..=8)
            .map(|n| ImageAdvisory {
                id: format!("DEBIAN-CVE-2025-{n}"),
                package: "openssl".into(),
                old: "3.0.1".into(),
                new: Some("3.0.2".into()),
                platforms: vec!["linux/amd64".into()],
            })
            .collect();
        let mut fixing = change("debian", check);
        fixing.advisories = (4..=8)
            .map(|n| advisory(&format!("DEBIAN-CVE-2025-{n}"), "high"))
            .collect();
        let (_, details) = image_section(&[fixing]);
        assert_eq!(details.matches(" | openssl | ").count(), 5, "{details}");
        assert!(
            details.find("[DEBIAN-CVE-2025-8]").unwrap()
                < details.find("[DEBIAN-CVE-2025-4]").unwrap(),
            "{details}"
        );
        assert!(
            details.contains("\nAnd 3 more listed by OSV: "),
            "{details}"
        );
    }

    /// A rebuild with no verified record claims no fix.
    #[test]
    fn a_rebuild_without_a_verified_record_claims_no_fix() {
        let mut check = state("checked");
        check.introduced = vec!["USN-9-1".into()];
        let (head, details) = image_section(&[change("debian", check)]);
        assert!(
            !head.contains("[!IMPORTANT]") && details.is_empty(),
            "{head}"
        );
        // What the rebuild brings in is still named.
        assert!(
            head.contains("[USN-9-1](https://osv.dev/vulnerability/USN-9-1)"),
            "{head}"
        );
        let mut check = state("checked");
        check.fixed = vec![ImageAdvisory {
            id: "DSA-7".into(),
            package: "openssl".into(),
            old: "3.0.1".into(),
            new: Some("3.0.2".into()),
            platforms: vec!["linux/amd64".into()],
        }];
        let (head, details) = image_section(&[change("debian", check)]);
        assert!(
            !head.contains("[!IMPORTANT]") && details.is_empty(),
            "{head}"
        );
        // The advisory OSV lists is still named.
        assert!(
            head.contains("[DSA-7](https://osv.dev/vulnerability/DSA-7)"),
            "{head}"
        );
    }

    #[test]
    fn a_generator_name_cannot_close_its_code_span() {
        let mut ko = state("sbom-without-os");
        ko.detail = Some("ko`\n> [!NOTE] `x".into());
        let (head, _) = image_section(&[change("ghcr.io/acme/api", ko)]);
        assert!(head.contains("``ko` > [!NOTE] `x``"), "{head}");
    }

    /// Warnings share one callout, and a silenced image is left off.
    #[test]
    fn silenced_warnings_are_left_off() {
        let mut silenced = state("no-sbom");
        silenced.silenced = Some("scanned in CI".into());
        let (head, _) = image_section(&[
            change("quay.io/argoproj/argocd", state("no-sbom")),
            change("ghcr.io/acme/internal", silenced),
            change("quay.io/old", state("previous-unavailable")),
        ]);
        assert_eq!(head.matches("[!WARNING]").count(), 1, "{head}");
        assert!(
            head.contains("argocd") && head.contains("quay.io/old"),
            "{head}"
        );
        assert!(!head.contains("internal"), "{head}");
    }
}

mod foreign_text_tests {
    use super::{FOREIGN_LIMIT, body_hash, with_foreign};
    use crate::markers::{OWNERSHIP as MARKER, START, regions};

    /// What the Count writes: his section, opened by the start marker and
    /// closed by the ownership marker.
    fn section(table: &str) -> String {
        format!(
            "{START}\n{table}{MARKER}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->\n",
            "a".repeat(64),
            "b".repeat(64)
        )
    }
    /// A block some other tool or person added to the description.
    const BLOCK: &str = "<!-- tool:summary:start -->\nA generated summary, with a [link](https://example.org/x) and @someone.\n<!-- tool:summary:end -->";

    #[test]
    fn the_section_is_what_the_counts_markers_enclose() {
        let body = format!("Intro by a person.\n\n{}\n\n{BLOCK}", section("table\n"));
        let (before, middle, after) = regions(&body).unwrap();
        assert_eq!(before, "Intro by a person.\n\n");
        assert_eq!(middle, section("table\n").trim_end());
        assert_eq!(after, format!("\n\n\n{BLOCK}"));
        // An older body has no start marker: all of it up to his marker is his.
        let legacy = format!(
            "table\n{MARKER}{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->\nrest",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(regions(&legacy).unwrap().0, "");
        assert!(regions("no markers at all").is_none());
    }

    #[test]
    fn text_before_and_after_the_section_survives_rewrites_exactly_once() {
        for current in [
            format!("{}{BLOCK}", section("old\n")),
            format!("{BLOCK}\n\n{}", section("old\n")),
            format!("{BLOCK}\n\n{}\n\n{BLOCK} again", section("old\n")),
        ] {
            let rewritten = with_foreign(section("new\n"), &current);
            assert!(
                rewritten.contains("new\n") && !rewritten.contains("old\n"),
                "{rewritten}"
            );
            assert_eq!(rewritten.matches(START).count(), 1);
            assert_eq!(rewritten.matches(MARKER).count(), 1);
            assert_eq!(
                rewritten.matches("tool:summary:start").count(),
                current.matches("tool:summary:start").count()
            );
            // Rendering again over the result is a fixed point.
            assert_eq!(with_foreign(section("new\n"), &rewritten), rewritten);
            // So is taking the existing body as the text, as an unchanged publish does.
            assert_eq!(with_foreign(current.clone(), &current), current);
            assert_eq!(
                body_hash(&with_foreign(current.clone(), &current)),
                body_hash(&current)
            );
        }
    }

    #[test]
    fn what_others_wrote_comes_from_the_latest_read_not_an_older_one() {
        let stale = format!("{}{BLOCK}", section("t\n"));
        // The block was rewritten in place, and another was prepended, since.
        let fresh = format!(
            "Added later.\n\n{}<!-- tool:summary:start -->\nrewritten\n<!-- tool:summary:end -->",
            section("t\n")
        );
        let merged = with_foreign(stale, &fresh);
        assert_eq!(merged, fresh);
    }

    #[test]
    fn text_above_that_quotes_the_start_marker_is_kept() {
        let quoted = format!("Someone pasted `{START}` here.\n\n");
        let current = format!("{quoted}{}{BLOCK}", section("old\n"));
        let rewritten = with_foreign(section("new\n"), &current);
        assert!(rewritten.starts_with(&quoted), "{rewritten}");
        assert!(rewritten.contains("new\n") && !rewritten.contains("old\n"));
        assert!(rewritten.ends_with(BLOCK));
        assert_eq!(with_foreign(section("new\n"), &rewritten), rewritten);
    }

    #[test]
    fn an_edit_inside_the_section_loses_to_the_counts_rewrite() {
        let edited = section("table\nsomeone typed here\n");
        let merged = with_foreign(section("table\n"), &format!("{edited}{BLOCK}"));
        assert!(!merged.contains("someone typed here"), "{merged}");
        assert!(merged.ends_with(BLOCK));
    }

    #[test]
    fn a_large_amount_of_foreign_text_fits_and_an_excessive_amount_is_dropped() {
        let big = format!("\n\n{}", "x".repeat(30_000));
        let kept = with_foreign(section("t\n"), &format!("{}{big}", section("o\n")));
        assert!(kept.ends_with(&big));
        let huge = format!("\n\n{}", "x".repeat(FOREIGN_LIMIT));
        let dropped = with_foreign(section("t\n"), &format!("{}{huge}", section("o\n")));
        assert_eq!(dropped, format!("{}\n", section("t\n").trim_end()));
    }
}
