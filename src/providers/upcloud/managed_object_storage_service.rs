use super::Client;
use crate::provider::OperationResult;

pub fn create(
    client: &Client,
    properties: serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let name = properties
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing property: name")?;
    let region = properties
        .get("region")
        .and_then(|v| v.as_str())
        .ok_or("missing property: region")?;
    let configured_status = properties
        .get("configured_status")
        .and_then(|v| v.as_str())
        .ok_or("missing property: configured_status")?;

    let mut body = serde_json::json!({
        "name": name,
        "region": region,
        "configured_status": configured_status
    });

    if let Some(tp) = properties.get("termination_protection") {
        let val = match tp {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::String(s) => s == "true",
            _ => false,
        };
        body["termination_protection"] = serde_json::Value::Bool(val);
    }

    if let Some(networks) = properties.get("networks").and_then(|v| v.as_array()) {
        let api_networks: Vec<serde_json::Value> = networks
            .iter()
            .filter_map(|n| {
                let name = n.get("name").and_then(|v| v.as_str())?;
                let net_type = n.get("type").and_then(|v| v.as_str())?;
                let family = n.get("family").and_then(|v| v.as_str())?;
                let mut net = serde_json::json!({
                    "name": name,
                    "type": net_type,
                    "family": family
                });
                if let Some(uuid) = n.get("uuid").and_then(|v| v.as_str()) {
                    net["uuid"] = serde_json::Value::String(uuid.to_string());
                }
                Some(net)
            })
            .collect();
        body["networks"] = serde_json::Value::Array(api_networks);
    }

    let url = format!("{}/1.3/object-storage-2", client.base_url);
    let resp = client
        .http
        .post(&url)
        .bearer_auth(&client.token)
        .json(&body)
        .send()?;

    let status = resp.status();
    let text = resp.text()?;

    if !status.is_success() {
        return Ok(OperationResult::Failed {
            error: format!("UpCloud API error {status}: {text}"),
        });
    }

    let resp_body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse create response: {e}"))?;

    Ok(OperationResult::InProgress {
        outputs: resp_body,
    })
}

pub fn read(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let uuid = outputs
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing output: uuid")?;

    let url = format!("{}/1.3/object-storage-2/{uuid}", client.base_url);
    let resp = client
        .http
        .get(&url)
        .bearer_auth(&client.token)
        .send()?;

    let status = resp.status();
    let text = resp.text()?;

    if !status.is_success() {
        return Ok(OperationResult::Failed {
            error: format!("UpCloud API error {status}: {text}"),
        });
    }

    let resp_body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse read response: {e}"))?;

    let state = resp_body
        .get("operational_state")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match state {
        "running" | "stopped" => Ok(OperationResult::Complete {
            outputs: resp_body,
        }),
        _ => Ok(OperationResult::InProgress {
            outputs: resp_body,
        }),
    }
}

pub fn update(
    client: &Client,
    old_outputs: &serde_json::Value,
    properties: serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let uuid = old_outputs
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing output: uuid")?;

    let mut body = serde_json::json!({});

    if let Some(name) = properties.get("name").and_then(|v| v.as_str()) {
        body["name"] = serde_json::Value::String(name.to_string());
    }
    if let Some(status) = properties.get("configured_status").and_then(|v| v.as_str()) {
        body["configured_status"] = serde_json::Value::String(status.to_string());
    }
    if let Some(tp) = properties.get("termination_protection") {
        let val = match tp {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::String(s) => s == "true",
            _ => false,
        };
        body["termination_protection"] = serde_json::Value::Bool(val);
    }
    if let Some(networks) = properties.get("networks").and_then(|v| v.as_array()) {
        let api_networks: Vec<serde_json::Value> = networks
            .iter()
            .filter_map(|n| {
                let name = n.get("name").and_then(|v| v.as_str())?;
                let net_type = n.get("type").and_then(|v| v.as_str())?;
                let family = n.get("family").and_then(|v| v.as_str())?;
                let mut net = serde_json::json!({
                    "name": name,
                    "type": net_type,
                    "family": family
                });
                if let Some(uuid) = n.get("uuid").and_then(|v| v.as_str()) {
                    net["uuid"] = serde_json::Value::String(uuid.to_string());
                }
                Some(net)
            })
            .collect();
        body["networks"] = serde_json::Value::Array(api_networks);
    }

    let url = format!("{}/1.3/object-storage-2/{uuid}", client.base_url);
    let resp = client
        .http
        .patch(&url)
        .bearer_auth(&client.token)
        .json(&body)
        .send()?;

    let status = resp.status();
    let text = resp.text()?;

    if !status.is_success() {
        return Ok(OperationResult::Failed {
            error: format!("UpCloud API error {status}: {text}"),
        });
    }

    let resp_body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse update response: {e}"))?;

    Ok(OperationResult::Complete {
        outputs: resp_body,
    })
}

pub fn delete(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let uuid = outputs
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing output: uuid")?;

    let url = format!("{}/1.3/object-storage-2/{uuid}", client.base_url);
    let resp = client
        .http
        .delete(&url)
        .bearer_auth(&client.token)
        .send()?;

    let status = resp.status();
    if status.is_success() || status.as_u16() == 204 {
        Ok(OperationResult::Complete {
            outputs: serde_json::json!({}),
        })
    } else {
        let text = resp.text()?;
        Ok(OperationResult::Failed {
            error: format!("UpCloud API error {status}: {text}"),
        })
    }
}
