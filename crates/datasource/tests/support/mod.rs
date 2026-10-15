use serde_json::{Value, json};

/// Simulated GraphQL projection of independent or previously recorded REST
/// release evidence. This does not claim to be a captured GraphQL response.
pub fn releases(request: &Value, lookup: impl Fn(&str, &str) -> Option<Value>) -> Value {
    let query = request["query"].as_str().unwrap();
    assert!(query.starts_with("query ReleaseNotes("));
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
        let value = match lookup(&repo, tag) {
            Some(body) if body["status"].as_u64().is_none_or(|status| status < 400) => {
                let mut value = json!({"updatedAt":body.get("updated_at").or(body.get("etag")).cloned().unwrap_or(json!("2026-10-15T00:00:00Z"))});
                if query.contains(&format!("r{i}:release(tagName:$t{i}){{updatedAt body:")) {
                    value["body"] = body.get("body").cloned().unwrap_or(Value::Null);
                    value["draft"] = body.get("draft").cloned().unwrap_or(json!(false));
                    value["prerelease"] = body.get("prerelease").cloned().unwrap_or(json!(false));
                }
                value
            }
            Some(body) if body["status"] != 404 => {
                return json!({"errors":[{"message":"simulated release failure"}]});
            }
            _ => Value::Null,
        };
        repository.insert(format!("r{i}"), value);
    }
    json!({"data":{"repository":repository}})
}
