use crate::{Error, Result};
use rustovate_report::{Dependency, Report, Status, Update};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    RustovateMissing,
    RustovateExtra,
    FieldMismatch,
    DeliberateDeparture,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Difference {
    pub classification: Classification,
    pub original_classification: Classification,
    pub repository: String,
    pub package_file: Option<String>,
    pub manager: Option<String>,
    pub dependency: Option<String>,
    pub field: String,
    pub renovate: Value,
    pub rustovate: Value,
    pub departure: Option<Departure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Departure {
    pub rule: String,
    pub row: u32,
    pub link: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rules {
    schema_version: u32,
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rule {
    id: String,
    departure: u32,
    classification: Classification,
    managers: Vec<String>,
    field: String,
    predicate: Predicate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Predicate {
    DigestAdded,
    DigestPinOnly,
}

impl Rules {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let rules: Self = serde_json::from_slice(bytes)?;
        if rules.schema_version != 1 {
            return Err(Error::Input("unsupported rules schemaVersion".into()));
        }
        let mut ids = BTreeSet::new();
        for rule in &rules.rules {
            if rule.id.is_empty()
                || !ids.insert(&rule.id)
                || rule.managers.is_empty()
                || !include_str!("../../docs/departures.md")
                    .lines()
                    .any(|line| line.starts_with(&format!("| {} |", rule.departure)))
                || !matches!(
                    (&rule.predicate, rule.classification, rule.field.as_str()),
                    (
                        Predicate::DigestAdded,
                        Classification::FieldMismatch,
                        "updates.newDigest"
                    ) | (
                        Predicate::DigestPinOnly,
                        Classification::RustovateExtra,
                        "updates"
                    )
                )
            {
                return Err(Error::Input(
                    "invalid departure rule, duplicate ID or missing docs/departures.md row".into(),
                ));
            }
        }
        Ok(rules)
    }

    fn classify(&self, difference: &mut Difference, dependency: Option<&Dependency>) {
        for rule in &self.rules {
            if difference.classification != rule.classification
                || difference.field != rule.field
                || !difference
                    .manager
                    .as_ref()
                    .is_some_and(|m| rule.managers.contains(m))
            {
                continue;
            }
            let matches = match rule.predicate {
                Predicate::DigestAdded => {
                    difference.renovate.is_null()
                        && difference
                            .rustovate
                            .as_str()
                            .is_some_and(|digest| !digest.is_empty())
                }
                Predicate::DigestPinOnly => dependency.is_some_and(|dep| {
                    serde_json::from_value::<Update>(difference.rustovate.clone()).is_ok_and(
                        |update| {
                            update.update_type == "pin"
                                && dep.current.digest.is_none()
                                && update.new_digest.as_ref().is_some_and(|s| !s.is_empty())
                                && update
                                    .new_value
                                    .as_ref()
                                    .is_none_or(|v| Some(v) == dep.current.value.as_ref())
                                && update
                                    .new_version
                                    .as_ref()
                                    .is_none_or(|v| Some(v) == dep.current.version.as_ref())
                                && update.group.is_none()
                        },
                    )
                }),
            };
            if matches {
                difference.classification = Classification::DeliberateDeparture;
                difference.departure = Some(Departure {
                    rule: rule.id.clone(),
                    row: rule.departure,
                    link: format!("docs/departures.md#departure-{}", rule.departure),
                });
                return;
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Triage {
    pub schema_version: u32,
    pub renovate: Report,
    pub rustovate: Report,
    pub differences: Vec<Difference>,
}

fn label(class: Classification) -> &'static str {
    match class {
        Classification::RustovateMissing => "rustovate-missing",
        Classification::RustovateExtra => "rustovate-extra",
        Classification::FieldMismatch => "field-mismatch",
        Classification::DeliberateDeparture => "deliberate-departure",
    }
}

fn cell(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "&#124;")
        .replace('`', "&#96;")
        .replace('\n', "<br>")
        .replace('\r', "")
}

impl Triage {
    pub fn markdown(&self) -> String {
        let mut out = String::from("# Dependency comparison triage\n\n");
        for class in [
            Classification::RustovateMissing,
            Classification::RustovateExtra,
            Classification::FieldMismatch,
            Classification::DeliberateDeparture,
        ] {
            let count = self
                .differences
                .iter()
                .filter(|d| d.classification == class)
                .count();
            out.push_str(&format!("- {}: {count}\n", label(class)));
        }
        out.push_str("\nRepository coverage and normalized inputs are in the accompanying JSON. Departure links are relative to the repository root.\n\n");
        if self.differences.is_empty() {
            out.push_str("No differences.\n");
            return out;
        }
        out.push_str("| Classification | Repository / file / manager / dependency | Field | Renovate | rustovate | Departure |\n|---|---|---|---|---|---|\n");
        for d in &self.differences {
            let location = [
                &d.repository,
                d.package_file.as_deref().unwrap_or(""),
                d.manager.as_deref().unwrap_or(""),
                d.dependency.as_deref().unwrap_or(""),
            ]
            .join(" / ");
            let departure = d
                .departure
                .as_ref()
                .map(|p| format!("[row {}]({}) ({})", p.row, p.link, cell(&p.rule)))
                .unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                label(d.classification),
                cell(&location),
                cell(&d.field),
                cell(&d.renovate.to_string()),
                cell(&d.rustovate.to_string()),
                departure
            ));
        }
        out
    }
}

#[derive(Clone)]
struct Location<'a> {
    repository: &'a str,
    path: Option<&'a str>,
    dep: Option<&'a Dependency>,
}

fn difference(
    location: &Location<'_>,
    field: &str,
    a: Value,
    b: Value,
    class: Classification,
) -> Difference {
    Difference {
        classification: class,
        original_classification: class,
        repository: location.repository.into(),
        package_file: location.path.map(str::to_owned),
        manager: location.dep.map(|d| d.manager.clone()),
        dependency: location.dep.map(|d| d.name.clone()),
        field: field.into(),
        renovate: a,
        rustovate: b,
        departure: None,
    }
}

fn mismatch(out: &mut Vec<Difference>, location: &Location<'_>, field: &str, a: Value, b: Value) {
    if a != b {
        out.push(difference(
            location,
            field,
            a,
            b,
            Classification::FieldMismatch,
        ));
    }
}

fn updates(out: &mut Vec<Difference>, location: &Location<'_>, a: &[Update], b: &[Update]) {
    let a_set: BTreeSet<_> = a.iter().collect();
    let b_set: BTreeSet<_> = b.iter().collect();
    let mut a: Vec<_> = a_set.difference(&b_set).copied().collect();
    let mut b: Vec<_> = b_set.difference(&a_set).copied().collect();
    let same_target = |left: &Update, right: &Update| {
        left.new_value == right.new_value
            && left.new_version == right.new_version
            && left.new_digest == right.new_digest
    };
    // A unique identical target can expose a taxonomy mismatch independently
    // of the update type used to pair changed targets below.
    for left in a.clone() {
        if a.iter().filter(|u| same_target(left, u)).count() != 1 {
            continue;
        }
        let candidates: Vec<_> = b.iter().filter(|u| same_target(left, u)).copied().collect();
        if let [right] = candidates.as_slice() {
            mismatch(
                out,
                location,
                "updates.updateType",
                json!(left.update_type),
                json!(right.update_type),
            );
            mismatch(
                out,
                location,
                "updates.group",
                json!(left.group),
                json!(right.group),
            );
            a.retain(|u| u != &left);
            b.retain(|u| u != right);
        }
    }
    // Pair only when a type identifies one remaining proposal on each side.
    // Multiple proposals of one type are extra/missing, never position-paired.
    let types: BTreeSet<_> = a.iter().chain(&b).map(|u| u.update_type.clone()).collect();
    for kind in types {
        let left: Vec<_> = a
            .iter()
            .filter(|u| u.update_type == kind)
            .copied()
            .collect();
        let right: Vec<_> = b
            .iter()
            .filter(|u| u.update_type == kind)
            .copied()
            .collect();
        if let ([left], [right]) = (left.as_slice(), right.as_slice()) {
            for (field, av, bv) in [
                (
                    "updates.newValue",
                    json!(left.new_value),
                    json!(right.new_value),
                ),
                (
                    "updates.newVersion",
                    json!(left.new_version),
                    json!(right.new_version),
                ),
                (
                    "updates.newDigest",
                    json!(left.new_digest),
                    json!(right.new_digest),
                ),
                ("updates.group", json!(left.group), json!(right.group)),
            ] {
                mismatch(out, location, field, av, bv);
            }
            a.retain(|u| u != left);
            b.retain(|u| u != right);
        }
    }
    for u in a {
        out.push(difference(
            location,
            "updates",
            json!(u),
            Value::Null,
            Classification::RustovateMissing,
        ));
    }
    for u in b {
        out.push(difference(
            location,
            "updates",
            Value::Null,
            json!(u),
            Classification::RustovateExtra,
        ));
    }
}

pub fn compare(renovate: &Report, rustovate: &Report, rules: &Rules) -> Result<Triage> {
    renovate.validate()?;
    rustovate.validate()?;
    for repo in renovate.repositories.iter().chain(&rustovate.repositories) {
        if repo.status != Status::Complete {
            return Err(Error::Input(format!(
                "{} is {:?}; only complete repositories can be compared",
                repo.repository, repo.status
            )));
        }
    }
    let a: BTreeMap<_, _> = renovate
        .repositories
        .iter()
        .map(|r| (&r.repository, r))
        .collect();
    let b: BTreeMap<_, _> = rustovate
        .repositories
        .iter()
        .map(|r| (&r.repository, r))
        .collect();
    let mut differences = Vec::new();
    let repos: BTreeSet<_> = a.keys().chain(b.keys()).copied().collect();
    for repo in repos {
        let location = Location {
            repository: repo,
            path: None,
            dep: None,
        };
        let (Some(left), Some(right)) = (a.get(repo), b.get(repo)) else {
            let class = if a.contains_key(repo) {
                Classification::RustovateMissing
            } else {
                Classification::RustovateExtra
            };
            differences.push(difference(
                &location,
                "repository",
                json!(a.get(repo)),
                json!(b.get(repo)),
                class,
            ));
            continue;
        };
        let a: BTreeMap<_, _> = left.package_files.iter().map(|f| (&f.path, f)).collect();
        let b: BTreeMap<_, _> = right.package_files.iter().map(|f| (&f.path, f)).collect();
        for path in a.keys().chain(b.keys()).copied().collect::<BTreeSet<_>>() {
            let location = Location {
                path: Some(path),
                ..location.clone()
            };
            let (Some(left), Some(right)) = (a.get(path), b.get(path)) else {
                let class = if a.contains_key(path) {
                    Classification::RustovateMissing
                } else {
                    Classification::RustovateExtra
                };
                differences.push(difference(
                    &location,
                    "packageFile",
                    json!(a.get(path)),
                    json!(b.get(path)),
                    class,
                ));
                continue;
            };
            let a: BTreeMap<_, _> = left
                .dependencies
                .iter()
                .map(|d| ((&d.manager, &d.name), d))
                .collect();
            let b: BTreeMap<_, _> = right
                .dependencies
                .iter()
                .map(|d| ((&d.manager, &d.name), d))
                .collect();
            for key in a.keys().chain(b.keys()).copied().collect::<BTreeSet<_>>() {
                let dep = a
                    .get(&key)
                    .or_else(|| b.get(&key))
                    .copied()
                    .expect("key exists");
                let location = Location {
                    dep: Some(dep),
                    ..location.clone()
                };
                let (Some(left), Some(right)) = (a.get(&key), b.get(&key)) else {
                    let class = if a.contains_key(&key) {
                        Classification::RustovateMissing
                    } else {
                        Classification::RustovateExtra
                    };
                    differences.push(difference(
                        &location,
                        "dependency",
                        json!(a.get(&key)),
                        json!(b.get(&key)),
                        class,
                    ));
                    continue;
                };
                let start = differences.len();
                for (field, av, bv) in [
                    ("package", json!(left.package), json!(right.package)),
                    (
                        "current.value",
                        json!(left.current.value),
                        json!(right.current.value),
                    ),
                    (
                        "current.version",
                        json!(left.current.version),
                        json!(right.current.version),
                    ),
                    (
                        "current.digest",
                        json!(left.current.digest),
                        json!(right.current.digest),
                    ),
                ] {
                    mismatch(&mut differences, &location, field, av, bv);
                }
                updates(&mut differences, &location, &left.updates, &right.updates);
                for d in &mut differences[start..] {
                    rules.classify(d, Some(left));
                }
            }
        }
    }
    Ok(Triage {
        schema_version: 1,
        renovate: renovate.clone(),
        rustovate: rustovate.clone(),
        differences,
    })
}
