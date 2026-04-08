use super::Client;
use crate::provider::OperationResult;

pub fn create(
    client: &Client,
    properties: serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let service_uuid = properties
        .get("service_uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing property: service_uuid")?;
    let name = properties
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing property: name")?;

    let body = serde_json::json!({ "name": name });

    let url = format!(
        "{}/1.3/object-storage-2/{service_uuid}/buckets",
        client.base_url
    );
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

    let mut outputs: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse create response: {e}"))?;
    outputs["service_uuid"] = serde_json::Value::String(service_uuid.to_string());

    Ok(OperationResult::Complete { outputs })
}

pub fn read(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let service_uuid = outputs
        .get("service_uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing output: service_uuid")?;
    let name = outputs
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing output: name")?;

    let url = format!(
        "{}/1.3/object-storage-2/{service_uuid}/buckets?limit=100",
        client.base_url
    );
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

    let buckets: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse list response: {e}"))?;

    match buckets.iter().find(|b| b.get("name").and_then(|v| v.as_str()) == Some(name)) {
        Some(bucket) => {
            let mut result = bucket.clone();
            result["service_uuid"] = serde_json::Value::String(service_uuid.to_string());
            Ok(OperationResult::Complete { outputs: result })
        }
        None => Ok(OperationResult::Failed {
            error: format!("bucket '{name}' not found in service '{service_uuid}'"),
        }),
    }
}

pub fn delete(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let service_uuid = outputs
        .get("service_uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing output: service_uuid")?;
    let name = outputs
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing output: name")?;

    let url = format!(
        "{}/1.3/object-storage-2/{service_uuid}/buckets/{name}",
        client.base_url
    );
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
