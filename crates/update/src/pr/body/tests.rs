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

/// A maintenance body lists the refs a relock advanced, one plain line
/// each under its workflow, and only when there are some; the list joins
/// the content hash, so a new moved ref rewrites the body.
#[test]
fn the_moved_ref_list_is_rendered_only_when_refs_moved() {
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
    let (quiet, quiet_marker) = bodies.intent(&plan(Vec::new()));
    assert!(!quiet.contains("moved upstream"), "{quiet}");
    let moved =
        |workflow: &str, dependency: &str, from: char, to: char| patchula_lockfiles::MovedRef {
            workflow: workflow.into(),
            dependency: dependency.into(),
            from: from.to_string().repeat(40),
            to: to.to_string().repeat(40),
        };
    let (text, marker) = bodies.intent(&plan(vec![
        moved(".github/workflows/ci.yml", "actions/checkout@v7", 'a', 'b'),
        moved(".github/workflows/ci.yml", "actions/setup-go@v5", 'c', 'd'),
        moved(
            ".github/workflows/release.yml",
            "actions/checkout@v7",
            'a',
            'b',
        ),
    ]));
    assert!(
        text.contains(concat!(
            "\n- .github/workflows/actions.lock\n\n",
            "Refs that moved upstream, now locked at their new commits:\n",
            "\n- `.github/workflows/ci.yml`",
            "\n  - `actions/checkout@v7` now resolves to `bbbbbbb`; the lock had `aaaaaaa`",
            "\n  - `actions/setup-go@v5` now resolves to `ddddddd`; the lock had `ccccccc`",
            "\n- `.github/workflows/release.yml`",
            "\n  - `actions/checkout@v7` now resolves to `bbbbbbb`; the lock had `aaaaaaa`\n",
        )),
        "{text}"
    );
    assert_ne!(marker, quiet_marker);
}

/// A relock that pruned the last listed workflow removes `actions.lock`,
/// and the maintenance body says so; a relock that kept it says nothing.
#[test]
fn a_removed_actions_lock_is_named_in_the_maintenance_body() {
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
    let line = "Every workflow `actions.lock` listed is gone, so the lockfile is removed.";
    let (kept, kept_marker) = bodies.intent(&draft().build());
    assert!(!kept.contains(line), "{kept}");
    let (removed, removed_marker) =
        bodies.intent(&draft().delete(".github/workflows/actions.lock").build());
    assert!(
        removed.contains(&format!("\n- .github/workflows/actions.lock\n\n{line}\n")),
        "{removed}"
    );
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

/// `reserved` counts the tail the way `finish` writes it: the start marker
/// line plus the tail's bytes, so a tail that cannot fit is refused before
/// any request builds the body.
#[test]
fn the_reserved_tail_is_what_finish_appends() {
    let permits = BTreeMap::new();
    let plan = update_plan("patchula/a-1.x", vec![change("a")]);
    let base = bodies(&permits).reserved(&plan);
    let expected = markers::START.len()
        + 1
        + bodies(&permits)
            .tail(
                &plan,
                &" ".repeat(markers::CONTENT_MARKER_LEN),
                &controls(&plan),
                None,
            )
            .len();
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
            .contains("Out of room for more release notes.")
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
            .contains("Out of room for more release notes.")
    );
}

/// A failed release-notes fetch says so in its section, never "could not
/// be resolved", and the body it leaves is not reused: the next run
/// fetches again.
#[test]
fn a_failed_notes_fetch_is_named_and_never_reused() {
    let permits = BTreeMap::new();
    let bodies = bodies(&permits);
    let plan = update_plan("patchula/a-1.x", vec![change("a")]);
    let (_, marker) = bodies.intent(&plan);
    let mut draft = bodies.draft(&plan);
    assert!(draft.next().is_some());
    draft.push(&Fetched::Transient);
    let failed = draft.finish();
    assert!(failed.contains("### a\n\nThe Count could not read these release notes this run"));
    assert!(!failed.contains("could not be resolved"));
    assert!(!bodies.reusable(&plan, Some(&pr(&failed)), &marker));
    // A lasting failure with no source says so, and settles.
    let mut draft = bodies.draft(&plan);
    assert!(draft.next().is_some());
    draft.push(&Fetched::Lasting(None));
    let unresolved = draft.finish();
    assert!(unresolved.contains("source repository could not be resolved"));
    assert!(bodies.reusable(&plan, Some(&pr(&unresolved)), &marker));
}

/// A lasting failure after the source resolved keeps the source's link and
/// says the notes could not be read, not that the source was not found;
/// the body settles.
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
    assert!(body.contains(
        "### a\n\nThe Count could not read these release notes; [view upstream](https://github.com/o/r)."
    ));
    assert!(!body.contains("could not be resolved"));
    assert!(bodies.reusable(&plan, Some(&pr(&body)), &marker));
}

/// `unticked` clears the re-check box and leaves the rest of the body.
#[test]
fn unticking_clears_only_the_recheck_box() {
    let line = format!(
        "- [x] Tick to re-check this PR on the next run. {}\n",
        markers::RECHECK
    );
    let body = format!("before\n{line}- [x] Tick the batch.\nafter\n");
    let unticked = unticked(&body);
    assert!(unticked.contains("- [ ] Tick to re-check this PR"));
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

    #[test]
    fn fixed_advisories_lead_the_body_once_each() {
        let advisory: patchula_report::Advisory = serde_json::from_value(serde_json::json!({
            "id":"GHSA-jf85-cpcp-j695","aliases":["CVE-2019-10744","SNYK-JS-LODASH-450202"],
            "severity":"critical","summary":"Prototype | pollution",
            "url":"https://osv.dev/vulnerability/GHSA-jf85-cpcp-j695"
        }))
        .unwrap();
        let change = crate::Change {
            registry: None,
            source: patchula_words::Source::Npm,
            package: "lodash".into(),
            file: "package.json".into(),
            current: "4.17.11".into(),
            current_version: None,
            value: Some("4.17.21".into()),
            version: Some("4.17.21".into()),
            digest: None,
            kind: patchula_words::Update::Patch,
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: vec![advisory],
            image: None,
        };
        let mut other = change.clone();
        other.file = "web/package.json".into();
        let text = security_section(&[change.clone(), other]);
        assert!(text.starts_with("> [!IMPORTANT]\n> This update fixes 1 known vulnerability."));
        assert!(text.contains("| [GHSA-jf85-cpcp-j695](https://osv.dev/vulnerability/GHSA-jf85-cpcp-j695) (CVE-2019-10744) | critical | lodash | Prototype \\| pollution |\n"), "{text}");
        let mut plain = change;
        plain.advisories.clear();
        assert!(security_section(&[plain]).is_empty());
    }

    #[test]
    fn advisory_text_and_package_names_never_mention_anyone() {
        let advisory: patchula_report::Advisory = serde_json::from_value(serde_json::json!({
            "id":"GHSA-x","severity":"high",
            "summary":"Reported by @mallory, see #12 and https://github.com/a/b/issues/3",
            "url":"https://osv.dev/vulnerability/GHSA-x"
        }))
        .unwrap();
        let change = crate::Change {
            registry: None,
            source: patchula_words::Source::Npm,
            package: "@scope/pkg".into(),
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
            body_notes: vec!["Held for @mallory".into()],
            advisories: vec![advisory],
            image: None,
        };
        let text = security_section(&[change]);
        for forged in ["@mallory", " #12", "//github.com/a/b/issues/3"] {
            assert!(!text.contains(forged), "{forged}: {text}");
        }
        assert!(text.contains("@\u{200d}mallory"), "{text}");
        // Package names are data, not prose.
        assert!(text.contains("| @scope/pkg |"), "{text}");
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
    use super::image_section;
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

    #[test]
    fn a_fixing_rebuild_lists_its_advisories_with_old_and_new_versions() {
        let mut check = state("checked");
        check.fixed = vec![
            ImageAdvisory {
                id: "DSA-1".into(),
                package: "openssl".into(),
                old: "3.0.1".into(),
                new: Some("3.0.2".into()),
                platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
            },
            ImageAdvisory {
                id: "DSA-2".into(),
                package: "perl".into(),
                old: "5.36".into(),
                new: None,
                platforms: vec!["linux/amd64".into()],
            },
        ];
        check.introduced = vec!["DSA-3".into()];
        let mut fixing = change("debian", check);
        fixing.advisories = vec![
            serde_json::from_value(serde_json::json!({
                "id": "DSA-1", "severity": "high", "url": "https://osv.dev/vulnerability/DSA-1"
            }))
            .unwrap(),
        ];
        let text = image_section(&[fixing]);
        assert!(text.starts_with("> [!IMPORTANT]\n> This rebuild fixes known vulnerabilities in the image's OS packages. The Count read OSV.dev's record for the 1 below, and OSV.dev lists 1 more that the new image no longer has."), "{text}");
        assert!(text.contains("| [DSA-1](https://osv.dev/vulnerability/DSA-1) | high | openssl | 3.0.1 | 3.0.2 | linux/amd64, linux/arm64 |\n"), "{text}");
        assert!(
            text.contains(
                "\nAnd 1 more listed by OSV: [DSA-2](https://osv.dev/vulnerability/DSA-2).\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("The new image also brings in 1 advisory listed by OSV: [DSA-3](https://osv.dev/vulnerability/DSA-3).\n"),
            "{text}"
        );
        assert!(
            !text.contains("OSV.dev knows of no vulnerability"),
            "{text}"
        );
    }

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
        check.introduced = vec!["USN-9-1".into(), "UBUNTU-CVE-2025-7".into()];
        check.introduced_details = vec![advisory("USN-9-1", "moderate")];
        let mut fixing = change("debian", check);
        fixing.advisories = (4..=8)
            .map(|n| advisory(&format!("DEBIAN-CVE-2025-{n}"), "high"))
            .collect();
        let text = image_section(&[fixing]);
        assert!(
            text.contains(
                "The Count read OSV.dev's record for the 5 below, and OSV.dev lists 3 more"
            ),
            "{text}"
        );
        assert_eq!(text.matches("| high | openssl |").count(), 5, "{text}");
        assert!(
            text.find("[DEBIAN-CVE-2025-8]").unwrap() < text.find("[DEBIAN-CVE-2025-4]").unwrap(),
            "{text}"
        );
        assert!(text.contains("\nAnd 3 more listed by OSV: [DEBIAN-CVE-2025-3](https://osv.dev/vulnerability/DEBIAN-CVE-2025-3), [DEBIAN-CVE-2025-2](https://osv.dev/vulnerability/DEBIAN-CVE-2025-2), [DEBIAN-CVE-2025-1](https://osv.dev/vulnerability/DEBIAN-CVE-2025-1).\n"), "{text}");
        assert!(text.contains("The new image also brings in [USN-9-1](https://osv.dev/vulnerability/USN-9-1) (moderate); and 1 more listed by OSV: [UBUNTU-CVE-2025-7](https://osv.dev/vulnerability/UBUNTU-CVE-2025-7).\n"), "{text}");
    }

    #[test]
    fn a_rebuild_that_only_brings_advisories_in_still_says_so() {
        let mut check = state("checked");
        check.introduced = vec!["USN-9-1".into()];
        let text = image_section(&[change("debian", check)]);
        assert!(!text.contains("[!IMPORTANT]"), "{text}");
        assert!(
            text.contains("The new image brings in 1 advisory listed by OSV: [USN-9-1](https://osv.dev/vulnerability/USN-9-1).\n"),
            "{text}"
        );
    }

    #[test]
    fn a_generator_name_cannot_close_its_code_span() {
        let mut ko = state("sbom-without-os");
        ko.detail = Some("ko`\n> [!NOTE] `x".into());
        let text = image_section(&[change("ghcr.io/acme/api", ko)]);
        assert!(
            text.contains("made by ``ko` > [!NOTE] `x``, which doesn't look for them"),
            "{text}"
        );
    }

    #[test]
    fn a_rebuild_with_no_verified_record_claims_no_fix() {
        let mut check = state("checked");
        check.fixed = vec![ImageAdvisory {
            id: "DSA-7".into(),
            package: "openssl".into(),
            old: "3.0.1".into(),
            new: Some("3.0.2".into()),
            platforms: vec!["linux/amd64".into()],
        }];
        let text = image_section(&[change("debian", check)]);
        assert!(!text.contains("[!IMPORTANT]"), "{text}");
        assert!(text.contains("OSV.dev lists 1 advisory that the new image no longer has, but the Count read no record that confirms one, so he doesn't count this rebuild as a security fix: [DSA-7](https://osv.dev/vulnerability/DSA-7)."), "{text}");
    }

    #[test]
    fn each_image_gets_its_own_line_and_silenced_warnings_are_left_off() {
        let mut ko = state("sbom-without-os");
        ko.detail = Some("ko".into());
        let mut silenced = state("no-sbom");
        silenced.silenced = Some("scanned in CI".into());
        let mut partial = state("partial-sources");
        partial.packages = 131;
        let mut unreadable = state("unreadable");
        unreadable.detail = Some("registry rate limit reached; retry after 60 seconds".into());
        let mut oracle = state("os-not-covered");
        oracle.os = Some("ol 9".into());
        let text = image_section(&[
            change("quay.io/argoproj/argocd", state("no-sbom")),
            change("ghcr.io/acme/api", ko),
            change("ghcr.io/acme/internal", silenced),
            change("debian", partial),
            change("mysql", oracle),
            change("ghcr.io/astral-sh/uv", state("no-os-packages")),
            change("quay.io/old", state("previous-unavailable")),
            change("alpine", unreadable),
        ]);
        assert_eq!(text.matches("[!WARNING]").count(), 1, "{text}");
        assert!(
            text.contains("> `quay.io/argoproj/argocd` publishes no SBOM for this image"),
            "{text}"
        );
        assert!(
            text.contains("made by `ko`, which doesn't look for them"),
            "{text}"
        );
        assert!(
            text.contains("> `mysql` is built on ol 9, and OSV.dev has no advisories for it."),
            "{text}"
        );
        assert!(
            text.contains(
                "The previous `quay.io/old` image (`sha256:4c49fc0…`) is no longer available"
            ),
            "{text}"
        );
        assert!(!text.contains("internal"), "{text}");
        assert!(
            text.contains("The Count compared `debian`'s old and new package lists."),
            "{text}"
        );
        assert!(text.contains("its 131 OS packages come from"), "{text}");
        assert!(
            text.contains("According to its SBOM, `ghcr.io/astral-sh/uv` has no OS packages"),
            "{text}"
        );
        assert!(text.contains("The Count couldn't read `alpine`'s SBOM on this run (registry rate limit reached; retry after 60 seconds)."), "{text}");
    }
}

mod tooling_tests {
    use super::tooling_section;
    use crate::tooling::*;
    use patchula_words::{ToolingDetail, ToolingRule};

    fn callout(rule: ToolingRule, detail: Option<ToolingDetail>) -> Callout {
        Callout {
            rule,
            detail,
            branch: "main".into(),
        }
    }

    #[test]
    fn each_tooling_warning_is_a_caution_in_the_design_wording() {
        let text = tooling_section(&[
            callout(ToolingRule::MergeWithoutCi, None),
            callout(ToolingRule::MergeWithoutCi, Some(ToolingDetail::Native)),
            callout(ToolingRule::MergeWithoutRequiredChecks, None),
            callout(ToolingRule::SecurityMergeWithoutDelivery, None),
        ]);
        assert_eq!(text.matches("> [!CAUTION]\n").count(), 4, "{text}");
        for line in [
            "> **This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits, so nothing will have tested it when GitHub merges it.\n",
            "> **This PR is set to merge itself, and the Count can't find any CI.** No workflow or CI file runs on pull requests, and no checks have reported on recent commits. He waits for at least one passing check before merging, so this PR will stay open.\n",
            "> **Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `main` requires no status checks, so a red PR merges as readily as a green one. Add a required check in a ruleset.\n",
            "> **Merging this fix ships nothing.** This PR fixes a known vulnerability and will merge on its own, but nothing in this repository builds, releases or deploys when `main` changes. Whatever runs the old code stays bitten. If something outside this repository delivers it, say so in `warnings.quiet`.\n",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        let narrower = tooling_section(&[
            callout(
                ToolingRule::MergeWithoutRequiredChecks,
                Some(ToolingDetail::ReviewsOnly),
            ),
            callout(
                ToolingRule::SecurityMergeWithoutDelivery,
                Some(ToolingDetail::TaggedRelease),
            ),
        ]);
        assert!(
            narrower.contains("`main` requires a review but no status checks"),
            "{narrower}"
        );
        assert!(narrower.contains("> **Merging this fix doesn't ship it.** It will merge on its own, and nothing runs when `main` changes. The only workflow here that could deliver it runs on tags or releases, so it ships only if you cut one.\n"), "{narrower}");
        assert!(tooling_section(&[]).is_empty());
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
