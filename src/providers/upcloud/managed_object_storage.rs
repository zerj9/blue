use std::collections::HashMap;

use super::Client;
use crate::providers::matches_filters;

fn list_regions(client: &Client) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let url = format!("{}/1.3/object-storage-2/regions", client.base_url);
    let resp = client
        .http
        .get(&url)
        .bearer_auth(&client.token)
        .send()?
        .error_for_status()?;
    let text = resp.text()?;
    let body: Vec<serde_json::Value> = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse object storage regions response: {e}"))?;

    Ok(body)
}

pub fn resolve_regions(
    client: &Client,
    filters: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let filter_map: HashMap<String, String> = serde_json::from_value(filters)?;
    let regions = list_regions(client)?;
    let matched: Vec<&serde_json::Value> = regions
        .iter()
        .filter(|r| matches_filters(r, &filter_map, &["name"]))
        .collect();

    match matched.len() {
        0 => Err(format!("no regions matched filters: {:?}", filter_map).into()),
        1 => Ok(matched[0].clone()),
        n => {
            let mut msg = format!("{n} regions matched, expected exactly 1\n");
            for r in &matched {
                let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let primary_zone = r
                    .get("primary_zone")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  - {name} (primary_zone: {primary_zone})\n"));
            }
            Err(msg.into())
        }
    }
}
