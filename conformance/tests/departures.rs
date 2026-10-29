#![forbid(unsafe_code)]

use patchula_conformance::Rules;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const RULES: &[u8] = include_bytes!("../departures.json");
const PAGE: &str = include_str!("../../docs/src/content/docs/renovate/departures.md");

fn page_anchors() -> Vec<String> {
    PAGE.lines()
        .filter(|line| line.starts_with("### "))
        .map(|line| {
            line.rsplit_once("{#")
                .and_then(|(_, anchor)| anchor.strip_suffix('}'))
                .unwrap_or_else(|| panic!("departure heading without an anchor: {line}"))
                .to_owned()
        })
        .collect()
}

#[test]
fn every_rule_departure_is_an_entry_on_the_page() {
    let anchors: BTreeSet<_> = page_anchors().into_iter().collect();
    let rules: Value = serde_json::from_slice(RULES).unwrap();
    for section in ["rules", "normalizations"] {
        for entry in rules[section].as_array().unwrap() {
            if let Some(departure) = entry["departure"].as_str() {
                assert!(
                    anchors.contains(departure),
                    "{} names departure {departure}, which has no entry on the page",
                    entry["id"]
                );
            }
        }
    }
}

#[test]
fn every_page_entry_has_one_anchor_and_no_harness_rules() {
    let anchors = page_anchors();
    let unique: BTreeSet<_> = anchors.iter().collect();
    assert_eq!(unique.len(), anchors.len(), "duplicate departure anchors");
    assert!(
        !PAGE.contains("Comparison harness"),
        "harness rules belong in conformance/departures.json"
    );
}

#[test]
fn rules_and_normalizations_need_descriptions() {
    assert!(Rules::from_json(RULES).is_ok());
    for section in ["rules", "normalizations"] {
        let mut value: Value = serde_json::from_slice(RULES).unwrap();
        value[section][0]["description"] = json!("");
        assert!(Rules::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
