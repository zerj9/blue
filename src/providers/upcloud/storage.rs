use std::collections::HashMap;

use super::Client;

fn list(client: &Client) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let url = format!("{}/1.3/storage", client.base_url);
    let resp = client
        .http
        .get(&url)
        .bearer_auth(&client.token)
        .send()?
        .error_for_status()?;
    let text = resp.text()?;
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse storage response: {e}"))?;

    body.get("storages")
        .and_then(|s| s.get("storage"))
        .and_then(|s| s.as_array())
        .cloned()
        .ok_or_else(|| "unexpected storage response: expected storages.storage array".into())
}

fn matches(item: &serde_json::Value, filters: &HashMap<String, String>) -> bool {
    let obj = match item.as_object() {
        Some(o) => o,
        None => return false,
    };
    for (key, value) in filters {
        let field_value = match obj.get(key.as_str()).and_then(json_as_str) {
            Some(fv) => fv,
            None => return false,
        };
        // title uses substring match; all others use exact match
        if key == "title" {
            if !field_value.contains(value.as_str()) {
                return false;
            }
        } else if field_value != *value {
            return false;
        }
    }
    true
}

fn json_as_str(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn resolve(
    client: &Client,
    filters: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let filter_map: HashMap<String, String> = serde_json::from_value(filters)?;
    let storages = list(client)?;
    let matched: Vec<&serde_json::Value> = storages
        .iter()
        .filter(|s| matches(s, &filter_map))
        .collect();

    match matched.len() {
        0 => Err(format!("no storage matched filters: {:?}", filter_map).into()),
        1 => Ok(matched[0].clone()),
        n => {
            let mut msg = format!("{n} storages matched, expected exactly 1\n");
            for s in &matched {
                let title = s.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let uuid = s.get("uuid").and_then(|v| v.as_str()).unwrap_or("?");
                let stype = s.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                msg.push_str(&format!("  - {title} (uuid: {uuid}, type: {stype})\n"));
            }
            Err(msg.into())
        }
    }
}
