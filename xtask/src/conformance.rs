//! Ad hoc views of Renovate comparison output, for triage by hand.

use crate::{Args, Result, text};
use serde_json::Value;
use std::collections::BTreeMap;

/// Prints an object's fields in the given order, leaving out absent ones.
fn object(fields: &[(&str, Option<&Value>)]) -> String {
    let fields: Vec<String> = fields
        .iter()
        .filter_map(|(name, value)| Some(format!("{}:{}", Value::from(*name), (*value)?)))
        .collect();
    format!("{{{}}}", fields.join(","))
}

fn each(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Summarizes triage buckets, or prints a manager's representative
/// differences, or every repository's warnings.
pub fn inspect(args: Args) -> Result {
    let args = args.positional(1, 3)?;
    let triage: Value = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let manager = args.get(1).map(String::as_str);
    let limit: usize = args.get(2).map_or(Ok(2), |l| l.parse())?;
    let differences = each(&triage["differences"]);
    match manager {
        None => {
            let mut buckets = BTreeMap::<String, usize>::new();
            for d in differences {
                let key = ["repository", "manager", "classification", "field"]
                    .map(|field| text(&d[field]))
                    .join(" / ");
                *buckets.entry(key).or_default() += 1;
            }
            for (key, count) in buckets {
                println!("{count}\t{key}");
            }
        }
        Some("warnings") => {
            for r in each(&triage["patchula"]["repositories"]) {
                if r["warnings"].as_array().is_some_and(|w| !w.is_empty()) {
                    let fields = [
                        ("repository", r.get("repository")),
                        ("warnings", r.get("warnings")),
                    ];
                    println!("{}", object(&fields));
                }
            }
        }
        Some(manager) => {
            let mut seen = BTreeMap::<String, usize>::new();
            for d in differences.iter().filter(|d| {
                d["manager"] == manager && d["classification"] != "deliberate-departure"
            }) {
                let key = ["repository", "classification", "field"]
                    .map(|field| text(&d[field]))
                    .join("/");
                let count = seen.entry(key).or_default();
                if *count >= limit {
                    continue;
                }
                *count += 1;
                let actual = each(&triage["patchula"]["repositories"])
                    .iter()
                    .find(|r| r["repository"] == d["repository"])
                    .and_then(|r| {
                        each(&r["packageFiles"])
                            .iter()
                            .find(|f| f["path"] == d["packageFile"])
                    })
                    .and_then(|f| {
                        each(&f["dependencies"])
                            .iter()
                            .find(|a| a["manager"] == d["manager"] && a["name"] == d["dependency"])
                    });
                let mut fields: Vec<_> = d
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, value)| (name.as_str(), Some(value)))
                    .collect();
                fields.push((
                    "heldBack",
                    actual.and_then(|a| a["metadata"].get("heldBack")),
                ));
                println!("{}", object(&fields));
            }
        }
    }
    Ok(())
}

/// Prints each dependency a Renovate JSON log reports for one repository, and
/// how the repository finished.
pub fn logs(args: Args) -> Result {
    let args = args.positional(2, 3)?;
    let (repository, manager) = (args[1].as_str(), args.get(2).map(String::as_str));
    for line in String::from_utf8_lossy(&std::fs::read(&args[0])?).lines() {
        if !line.starts_with('{') {
            continue;
        }
        let record: Value = serde_json::from_str(line)?;
        if record["repository"] != repository {
            continue;
        }
        if record["msg"] == "Repository finished" {
            let fields = [
                ("result", record.get("result")),
                ("exitCode", record.get("exitCode")),
            ];
            println!("{}", object(&fields));
        }
        if record["msg"] != "packageFiles with updates" {
            continue;
        }
        let managers = [&record["config"], &record["packageFiles"]]
            .into_iter()
            .find_map(Value::as_object);
        for (name, files) in managers.into_iter().flatten() {
            if manager.is_some_and(|m| m != name) {
                continue;
            }
            let name = Value::from(name.as_str());
            for file in each(files) {
                for d in each(&file["deps"]) {
                    let fields = [
                        ("manager", Some(&name)),
                        ("file", file.get("packageFile")),
                        ("name", d.get("depName")),
                        ("package", d.get("packageName")),
                        ("source", d.get("datasource")),
                        ("current", d.get("currentValue")),
                        ("version", d.get("currentVersion")),
                        ("digest", d.get("currentDigest")),
                        ("skip", d.get("skipReason")),
                        ("updates", d.get("updates")),
                        ("registry", d.get("registryUrls")),
                    ];
                    println!("{}", object(&fields));
                }
            }
        }
    }
    Ok(())
}
