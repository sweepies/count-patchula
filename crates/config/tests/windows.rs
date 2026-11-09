use chrono::{DateTime, Utc};
use patchula_config::{Window, due, load};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().to_utc()
}
fn window(cron: &str) -> Vec<Window> {
    vec![Window {
        cron: vec![cron.into()],
        zone: "UTC".into(),
    }]
}

#[test]
fn a_daily_time_is_due_once_on_the_first_run_after_it() {
    let daily = window("0 3 * * *");
    // Hourly runs: only the one after 03:00 proposes.
    assert!(due(
        &daily,
        Some(at("2026-10-22T02:00:00Z")),
        at("2026-10-22T03:00:05Z")
    ));
    assert!(!due(
        &daily,
        Some(at("2026-10-22T03:00:05Z")),
        at("2026-10-22T04:00:00Z")
    ));
    // A run every six hours still catches it.
    assert!(due(
        &daily,
        Some(at("2026-10-22T00:00:00Z")),
        at("2026-10-22T06:00:00Z")
    ));
    // A webhook run at 03:30 takes the day's turn; the next sweep does not.
    assert!(due(
        &daily,
        Some(at("2026-10-22T02:59:00Z")),
        at("2026-10-22T03:30:00Z")
    ));
    assert!(!due(
        &daily,
        Some(at("2026-10-22T03:30:00Z")),
        at("2026-10-22T04:00:00Z")
    ));
}

#[test]
fn without_a_previous_run_only_the_current_minute_counts() {
    let daily = window("0 3 * * *");
    assert!(due(&daily, None, at("2026-10-22T03:00:30Z")));
    assert!(!due(&daily, None, at("2026-10-22T04:00:00Z")));
}

#[test]
fn ranges_stay_open_and_empty_windows_always_allow() {
    let mornings = window("* 0-6 * * 1");
    // 2026-10-19 is a Monday.
    assert!(due(
        &mornings,
        Some(at("2026-10-19T02:00:00Z")),
        at("2026-10-19T03:00:00Z")
    ));
    assert!(!due(
        &mornings,
        Some(at("2026-10-19T08:00:00Z")),
        at("2026-10-19T09:00:00Z")
    ));
    assert!(due(&[], None, at("2026-10-22T12:00:00Z")));
}

#[test]
fn the_window_zone_decides_when_a_time_is_due() {
    let tokyo = vec![Window {
        cron: vec!["0 9 * * *".into()],
        zone: "Asia/Tokyo".into(),
    }];
    // 09:00 in Tokyo is 00:00 UTC.
    assert!(due(
        &tokyo,
        Some(at("2026-10-21T23:00:00Z")),
        at("2026-10-22T00:00:30Z")
    ));
    assert!(!due(
        &tokyo,
        Some(at("2026-10-22T00:00:30Z")),
        at("2026-10-22T01:00:00Z")
    ));
}

fn fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("patchula.pkl");
    std::fs::write(&path, format!("amends \"PatchulaConfig.pkl\"\n{body}\n")).unwrap();
    (directory, path)
}

#[test]
fn maintenance_defaults_to_monday_overnight_utc() {
    let (_directory, path) = fixture("");
    let windows = load(&path).unwrap().maintenance_windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].cron, ["* 0-3 * * 1"]);
    assert_eq!(windows[0].zone, "UTC");
    // 2026-10-19 is a Monday; the window covers 00:00 to 03:59 UTC.
    assert!(due(
        &windows,
        Some(at("2026-10-19T01:00:00Z")),
        at("2026-10-19T02:00:00Z")
    ));
    assert!(!due(
        &windows,
        Some(at("2026-10-19T04:00:00Z")),
        at("2026-10-19T05:00:00Z")
    ));
}

#[test]
fn configured_windows_replace_the_maintenance_default() {
    let (_directory, path) = fixture(
        "defaults { windows = new Listing { new { cron = new Listing { \"0 2 * * *\" } zone = \"Europe/London\" } } }",
    );
    let windows = load(&path).unwrap().maintenance_windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].cron, ["0 2 * * *"]);
    assert_eq!(windows[0].zone, "Europe/London");
}
