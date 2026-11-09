use serde_json::{Value, json};

/// GraphQL projection of independent or previously recorded REST release
/// evidence for the in-memory test transport. It answers the aliases and
/// variables the request carries; fields the query did not select are
/// ignored by the client.
pub fn releases(request: &Value, lookup: impl Fn(&str, &str) -> Option<Value>) -> Value {
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
                json!({
                    "updatedAt": body.get("updated_at").or(body.get("etag")).cloned().unwrap_or(json!("2026-10-15T00:00:00Z")),
                    "body": body.get("body").cloned().unwrap_or(Value::Null),
                    "draft": body.get("draft").cloned().unwrap_or(json!(false)),
                    "prerelease": body.get("prerelease").cloned().unwrap_or(json!(false)),
                })
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
