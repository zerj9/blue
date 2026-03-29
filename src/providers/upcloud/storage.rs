use std::collections::HashMap;

use super::Client;
use crate::providers::matches_filters;

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

pub fn resolve(
    client: &Client,
    filters: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let filter_map: HashMap<String, String> = serde_json::from_value(filters)?;
    let storages = list(client)?;
    let matched: Vec<&serde_json::Value> = storages
        .iter()
        .filter(|s| matches_filters(s, &filter_map, &["title"]))
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
