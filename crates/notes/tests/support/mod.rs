use serde_json::{Value, json};

/// GraphQL projection of independent or previously recorded REST release
/// evidence for the in-memory test transport. It fails closed on any
/// operation but `ReleaseNotes`, and answers each alias the fields its own
/// selection set names; fields the query did not select stay absent, as a
/// real GraphQL response would leave them.
pub fn releases(request: &Value, lookup: impl Fn(&str, &str) -> Option<Value>) -> Value {
    let query = request["query"]
        .as_str()
        .expect("a GraphQL request carries its query");
    // Whitespace is not significant in GraphQL; compare the compact text.
    let compact: String = query.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        compact.starts_with("queryReleaseNotes("),
        "unexpected GraphQL operation: {query}"
    );
    let variables = &request["variables"];
    let repo = format!(
        "{}/{}",
        variables["owner"].as_str().unwrap(),
        variables["name"].as_str().unwrap()
    );
    let mut repository = serde_json::Map::new();
    for i in 0..100 {
        let Some(tag) = variables[format!("t{i}")].as_str() else {
            break;
        };
        // `r{i}:release(tagName:$t{i}){...}`: the alias's own selection set,
        // flat fields up to the first closing brace.
        let marker = format!("r{i}:release(tagName:$t{i}){{");
        let body = compact
            .find(&marker)
            .and_then(|at| {
                let rest = &compact[at + marker.len()..];
                let end = rest.find('}')?;
                rest[..end].contains("body").then_some(())
            })
            .is_some();
        let value = match lookup(&repo, tag) {
            Some(found) if found["status"].as_u64().is_none_or(|status| status < 400) => {
                let mut value = json!({
                    "updatedAt": found.get("updated_at").or(found.get("etag")).cloned().unwrap_or(json!("2026-10-15T00:00:00Z")),
                });
                if body {
                    value["body"] = found.get("body").cloned().unwrap_or(Value::Null);
                    value["draft"] = found.get("draft").cloned().unwrap_or(json!(false));
                    value["prerelease"] = found.get("prerelease").cloned().unwrap_or(json!(false));
                }
                value
            }
            Some(found) if found["status"] != 404 => {
                return json!({"errors":[{"message":"simulated release failure"}]});
            }
            _ => Value::Null,
        };
        repository.insert(format!("r{i}"), value);
    }
    json!({"data":{"repository":repository}})
}
