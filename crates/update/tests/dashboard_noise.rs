//! The dashboard built from the data of the Count's first live run on his own
//! repository: grouped, readable, and never mentioning anyone.
use patchula_report::{ActionStatus, BranchAction, Repository};
use patchula_update::{Change, Plan, Recheck, dashboard};
use serde_json::{Value, json};

const ZSTD_SAFE: [(&str, &str); 12] = [
    (
        "RUSTSEC-2026-0395",
        "Use-after-free with `CParameter::StableInBuffer`",
    ),
    (
        "RUSTSEC-2026-0396",
        "`Seekable` reads a NULL or out-of-bounds seek table",
    ),
    (
        "RUSTSEC-2026-0397",
        "`OutBuffer::around_pos` exposes uninitialized memory",
    ),
    (
        "RUSTSEC-2026-0398",
        "`Seekable::decompress` sets a `Vec` length beyond its capacity for offsets past the end",
    ),
    (
        "RUSTSEC-2026-0399",
        "Use-after-free through `init_using_cdict` and `init_using_ddict`",
    ),
    (
        "RUSTSEC-2026-0400",
        "Use-after-free of a dictionary after a one-shot call on a context with an open stream",
    ),
    (
        "RUSTSEC-2026-0401",
        "Out-of-bounds read in `train_from_buffer` when sample sizes overflow",
    ),
    (
        "RUSTSEC-2026-0402",
        "Heap buffer overflow in `CCtx::try_clone` on a multithreaded context",
    ),
    (
        "RUSTSEC-2026-0403",
        "Uninitialized memory exposed when streaming decompression of a legacy frame fails",
    ),
    ("RUSTSEC-2026-0404", "Double free in `DCtx::try_clone`"),
    (
        "RUSTSEC-2026-0405",
        "Out-of-bounds read with an out-of-range `InBuffer::pos`",
    ),
    (
        "RUSTSEC-2026-0406",
        "Use-after-free when changing a multithreaded context with an unfinished frame",
    ),
];

fn advisory(id: &str, summary: &str, severity: Option<&str>, fixed_in: Option<&str>) -> Value {
    json!({"id":id,"aliases":[],"severity":severity,"summary":summary,
        "url":format!("https://osv.dev/vulnerability/{id}"),"fixedIn":fixed_in})
}
fn dependency(
    manager: &str,
    name: &str,
    version: &str,
    source: &str,
    advisories: Vec<Value>,
    updates: Value,
    extra: Value,
) -> Value {
    let mut metadata = json!({"source":source,"kinds":[],"locations":[],"advisories":advisories});
    for (key, value) in extra.as_object().into_iter().flatten() {
        metadata[key] = value.clone();
    }
    json!({"manager":manager,"name":name,"package":name,
        "current":{"value":version,"version":version,"digest":null},
        "updates":updates,"metadata":metadata})
}
fn update(version: &str, fixes: &[&str]) -> Value {
    json!({"newValue":version,"newVersion":version,"newDigest":null,"updateType":"minor",
        "group":null,"fixes":fixes})
}

/// The lockfile job's output as the Count's tool runner captured it.
fn lockfile_output() -> String {
    include_str!("fixtures/lockfile-failure.txt").to_owned()
}

fn repository() -> Repository {
    let zstd_safe: Vec<_> = ZSTD_SAFE
        .iter()
        .map(|(id, summary)| advisory(id, summary, None, Some("8.2.0")))
        .collect();
    let zstd_fixes: Vec<_> = ZSTD_SAFE.iter().map(|(id, _)| *id).collect();
    let rsa = |path: &str| {
        json!({"path":path,"dependencies":[dependency(
            "cargo","rsa","0.9.10","crates",
            vec![json!({"id":"RUSTSEC-2023-0071","aliases":["CVE-2023-49092"],"severity":null,
                "summary":"Marvin Attack: potential key recovery through timing sidechannels",
                "url":"https://osv.dev/vulnerability/RUSTSEC-2023-0071"})],
            json!([]), json!({}))]})
    };
    let missing = |path: &str, name: &str| {
        json!({"path":path,"dependencies":[dependency(
            "cargo",name,"0.6.0","crates",vec![],json!([]),json!({}))]})
    };
    serde_json::from_value(json!({
        "repository":"sweepies/count-patchula","status":"complete","reason":null,
        "warnings":[],
        "lookupFailures":[
            {"manager":"cargo","path":"crates/cli/Cargo.toml","name":"patchula-cache","package":"patchula-cache","datasource":"crates","kind":"not-found","message":"registry HTTP 404"},
            {"manager":"cargo","path":"crates/datasource/Cargo.toml","name":"patchula-cache","package":"patchula-cache","datasource":"crates","kind":"not-found","message":"registry HTTP 404"},
            {"manager":"cargo","path":"crates/http/Cargo.toml","name":"patchula-cache","package":"patchula-cache","datasource":"crates","kind":"not-found","message":"registry HTTP 404"}
        ],
        "packageFiles":[
            {"path":"Cargo.lock","dependencies":[
                dependency("cargo","crypto-bigint","0.5.5","crates",
                    vec![advisory("RUSTSEC-2026-0387","Multiple timing variability issues in operations intended to be constant-time",None,Some("0.7.6"))],
                    json!([]), json!({})),
                dependency("cargo","ctutils","0.4.3","crates",
                    vec![advisory("RUSTSEC-2026-0385","Non-constant-time mask generation and `u128` comparisons",None,Some("0.4.4"))],
                    json!([update("0.4.4",&["RUSTSEC-2026-0385"])]), json!({})),
                dependency("cargo","unsafe-libyaml","0.2.11","crates",
                    vec![advisory("RUSTSEC-2024-0449","unsafe-libyaml is unmaintained",None,None)],
                    json!([]), json!({})),
                dependency("cargo","zstd-safe","8.1.0","crates", zstd_safe,
                    json!([update("8.2.0",&zstd_fixes)]), json!({})),
            ]},
            rsa("crates/github/Cargo.toml"),
            rsa("crates/server/Cargo.toml"),
            missing("crates/cli/Cargo.toml","patchula-cache"),
            missing("crates/datasource/Cargo.toml","patchula-cache"),
            missing("crates/http/Cargo.toml","patchula-cache"),
            {"path":"docs/package.json","dependencies":[dependency(
                "npm","@astrojs/starlight","0.42.6","npm",vec![],json!([]),
                json!({"heldBack":[{"version":"0.42.8","updateType":"patch",
                    "publishedAt":"2026-10-24T08:29:16.914Z","eligibleAt":"2026-10-31T08:29:16.914Z"}],
                  "lookup":{"evaluatedAt":"2026-10-30T22:49:36.573Z","disabledUpdates":[],"rejected":[],
                    "catalog":{"fetchedAt":"2026-10-30T22:49:36.573Z","reused":false}}}))]}
        ]
    }))
    .unwrap()
}

fn plan(
    branch: &str,
    message: &str,
    package: &str,
    file: &str,
    version: &str,
    reason: Option<&str>,
) -> Plan {
    Plan {
        action: BranchAction {
            repository: "sweepies/count-patchula".into(),
            branch: branch.into(),
            base_commit: "base".into(),
            status: if reason.is_some() {
                ActionStatus::Error
            } else {
                ActionStatus::Planned
            },
            reason: reason.map(str::to_owned),
            message: message.into(),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        },
        contents: Default::default(),
        identity: branch.into(),
        fingerprint: String::new(),
        changes: vec![Change {
            source: "crates".into(),
            package: package.into(),
            file: file.into(),
            current: "1".into(),
            current_version: Some("1".into()),
            value: Some(version.into()),
            version: Some(version.into()),
            digest: None,
            kind: "patch".into(),
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        }],
        retry: false,
        tree: None,
        cached_pr: None,
        input_fingerprint: String::new(),
        lock_jobs: Vec::new(),
        lockfiles_ready: false,
        deleted: Default::default(),
        maintenance: false,
        batch: None,
        tooling: Vec::new(),
    }
}

fn render() -> String {
    render_with(&repository())
}
fn render_with(repo: &Repository) -> String {
    let output = lockfile_output();
    let plans = [
        plan(
            "patchula/zstd-safe-8.x",
            "chore(deps): update zstd-safe to 8.2.0 [security]",
            "zstd-safe",
            "Cargo.lock",
            "8.2.0",
            None,
        ),
        plan(
            "patchula/ctutils-0.x",
            "chore(deps): update ctutils to 0.4.4 [security]",
            "ctutils",
            "Cargo.lock",
            "0.4.4",
            None,
        ),
        plan(
            "patchula/astrojs-starlight-0.x",
            "chore(deps): update @astrojs/starlight to 0.42.7",
            "@astrojs/starlight",
            "docs/package.json",
            "0.42.7",
            None,
        ),
        plan(
            "patchula/base64-0.x",
            "chore(deps): update base64 to 0.23.1",
            "base64",
            "Cargo.lock",
            "0.23.1",
            Some(&format!("lockfile generation failed: {output}")),
        ),
        plan(
            "patchula/node-26.x",
            "chore(deps): update node to 26.13.0",
            "node",
            "mise.toml",
            "26.13.0",
            Some("lockfile generation failed: mise lock did not resolve node to 26.13.0"),
        ),
    ];
    let prs: Vec<_> = [
        (93, "patchula/zstd-safe-8.x", "chore(deps): update zstd-safe to 8.2.0 [security]"),
        (85, "patchula/ctutils-0.x", "chore(deps): update ctutils to 0.4.4 [security]"),
        (103, "patchula/astrojs-starlight-0.x", "chore(deps): update @astrojs/starlight to 0.42.7"),
    ]
    .into_iter()
    .map(|(number, branch, title)| json!({"number":number,"state":"open","title":title,"head":{"ref":branch}}))
    .collect();
    dashboard::render(repo, &plans, &prs, Recheck::NextRun, None)
}

/// The action targets are hashes; the snapshot keeps their place.
fn redact(text: &str) -> String {
    text.split("\"target\":\"")
        .enumerate()
        .map(|(i, part)| match (i, part.split_once('"')) {
            (0, _) | (_, None) => part.to_owned(),
            (_, Some((_, rest))) => format!("TARGET\"{rest}"),
        })
        .collect::<Vec<_>>()
        .join("\"target\":\"")
}

#[test]
fn the_first_live_dashboard_renders_grouped_and_readable() {
    let text = redact(&render());
    assert_eq!(text, include_str!("snapshots/dashboard-first-run.md"));
}

/// The first mention in `text` outside code, as GitHub would read it.
fn mention(text: &str) -> Option<String> {
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut code = false;
        let mut previous = ' ';
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '`' {
                code = !code;
            }
            if !code
                && ch == '@'
                && !previous.is_alphanumeric()
                && previous != '_'
                && chars.peek().is_some_and(|n| n.is_alphanumeric())
            {
                return Some(line.to_owned());
            }
            previous = ch;
        }
    }
    None
}

#[test]
fn nothing_third_party_mentions_anyone_or_links_back() {
    let mut repo = repository();
    repo.package_files[0].dependencies[0].metadata.as_mut().unwrap().advisories =
        serde_json::from_value(json!([
            {"id":"GHSA-evil","aliases":["CVE-2026-1"],"severity":"high",
             "summary":"Ask @mallory or &#64;eve (cc @org/team); fixes #12 and org/repo#34, see https://github.com/a/b/issues/5",
             "url":"https://osv.dev/vulnerability/GHSA-evil"},
            {"id":"GHSA-evil2","summary":"Second @mallory report","url":"https://osv.dev/vulnerability/GHSA-evil2"}
        ]))
        .unwrap();
    repo.lookup_failures = serde_json::from_value(json!([
        {"manager":"cargo","path":"crates/cli/Cargo.toml","name":"patchula-cache","package":"patchula-cache",
         "datasource":"crates","kind":"other","message":"registry said: contact @jdx, see #99"}
    ]))
    .unwrap();
    repo.warnings.push("registry note for @jdx and #7".into());
    let text = render_with(&repo);
    assert_eq!(mention(&text), None, "{text}");
    for forged in [
        "@jdx",
        "@mallory",
        "@org/team",
        "&#64;",
        "@eve",
        " #12",
        "org/repo#34",
        "//github.com/a/b/issues/5",
    ] {
        assert!(!text.contains(forged), "{forged}\n{text}");
    }
    // The tool output of the lockfile failure keeps its error and loses its mention.
    assert!(
        text.contains("@\u{200d}") || !text.contains("jdx"),
        "{text}"
    );
}

#[test]
fn the_lockfile_job_output_never_mentions_the_person_whose_tool_printed_it() {
    let text = render();
    assert!(lockfile_output().contains("@jdx"));
    assert!(!text.contains("@jdx"), "{text}");
    assert_eq!(mention(&text), None, "{text}");
}

#[test]
fn failure_output_is_a_collapsed_block_of_at_most_ten_lines_not_a_bullet() {
    let text = render();
    let start = text.find("<summary>The tool's output</summary>").unwrap();
    let block = &text[start..text[start..].find("</details>").unwrap() + start];
    let fenced: Vec<_> = block
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("```"))
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with("```"))
        .collect();
    assert!(fenced.len() <= 10 && fenced.len() >= 4, "{block}");
    assert!(
        fenced[0]
            .trim_start()
            .starts_with("error: failed to select a version"),
        "{block}"
    );
    assert!(block.len() < 1400, "{}", block.len());
    for noise in ["██", "rustup", "mise rust@", "info:", "\u{1b}", "installer"] {
        assert!(!text.contains(noise), "{noise}");
    }
    assert!(
        !text.contains("lockfile generation failed: cargo: exit exit"),
        "{text}"
    );
}

#[test]
fn vulnerabilities_are_one_bullet_per_package_and_version_with_the_files_listed() {
    let text = render();
    let known =
        &text[text.find("## Known vulnerabilities").unwrap()..text.find("## Open PRs").unwrap()];
    assert_eq!(known.matches("\n- ").count(), 5, "{known}");
    assert_eq!(known.matches("`zstd-safe` 8.1.0").count(), 1);
    assert!(known.contains("12 advisories"));
    assert!(known.contains("Fixed by #93."), "{known}");
    assert_eq!(known.matches("Fixed by #93").count(), 1);
    assert!(
        known.contains("`rsa` 0.9.10 in `crates/github/Cargo.toml`, `crates/server/Cargo.toml`")
    );
    assert_eq!(known.matches("RUSTSEC-2023-0071](").count(), 1, "{known}");
    assert!(known.contains("Fixed in `0.7.6`, not proposed."));
    assert!(known.contains("No fixed release yet."));
    assert!(!known.contains("unrated"));
    assert!(known.contains("<summary>What they say</summary>"));
}

#[test]
fn vulnerabilities_come_most_severe_first_then_by_package() {
    let mut repo = repository();
    let deps = &mut repo.package_files[0].dependencies;
    deps[0].metadata.as_mut().unwrap().advisories[0].severity = Some("low".into());
    deps[2].metadata.as_mut().unwrap().advisories[0].severity = Some("critical".into());
    deps[3].metadata.as_mut().unwrap().advisories[0].severity = Some("high".into());
    let text = render_with(&repo);
    let order: Vec<_> = [
        "`unsafe-libyaml`",
        "`zstd-safe`",
        "`crypto-bigint`",
        "`ctutils`",
    ]
    .map(|name| {
        text.find(&format!("- **{}**: {name}", "x"))
            .or_else(|| text.find(name))
            .unwrap()
    })
    .into();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}\n{text}");
}

#[test]
fn one_retry_box_covers_every_file_that_declares_the_failing_package() {
    let text = render();
    let boxes: Vec<_> = text
        .lines()
        .filter(|l| l.contains("Try that lookup again"))
        .collect();
    assert_eq!(boxes.len(), 1, "{text}");
    for file in [
        "crates/cli/Cargo.toml",
        "crates/datasource/Cargo.toml",
        "crates/http/Cargo.toml",
    ] {
        assert!(boxes[0].contains(&format!("`{file}`")), "{}", boxes[0]);
    }
    assert!(boxes[0].contains(&dashboard::retry_key("cargo", "patchula-cache", "crates")));
}

#[test]
fn held_updates_read_as_dates_and_name_the_package_in_code() {
    let text = render();
    let line = text.lines().find(|l| l.contains("0.42.8")).unwrap();
    assert!(
        line.contains(
            "`@astrojs/starlight` → 0.42.8 (npm, `docs/package.json`; eligible 31 Oct 08:29 UTC)"
        ),
        "{line}"
    );
    for dropped in ["T08:29", ".914", "catalog", "eligible at"] {
        assert!(!line.contains(dropped), "{line}");
    }
}

#[test]
fn advisory_ids_and_aliases_cannot_mention_anyone() {
    let mut repo = repository();
    repo.package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .advisories = serde_json::from_value(json!([
        {"id":"GHSA-@evilid","aliases":["CVE-2026-1 @evilalias","&commat;evilentity"],
         "url":"https://osv.dev/vulnerability/GHSA-x"}
    ]))
    .unwrap();
    let text = render_with(&repo);
    assert_eq!(mention(&text), None, "{text}");
    for forged in ["@evilid", "@evilalias", "&commat;", "@evilentity"] {
        assert!(!text.contains(forged), "{forged}\n{text}");
    }
}

#[test]
fn equally_severe_packages_are_ordered_by_name() {
    let mut repo = repository();
    for dep in &mut repo.package_files[0].dependencies {
        for advisory in &mut dep.metadata.as_mut().unwrap().advisories {
            advisory.severity = Some("high".into());
        }
    }
    let text = render_with(&repo);
    let order: Vec<_> = [
        "`crypto-bigint`",
        "`ctutils`",
        "`unsafe-libyaml`",
        "`zstd-safe`",
    ]
    .map(|name| text.find(&format!("**high**: {name}")).unwrap())
    .into();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}\n{text}");
}
