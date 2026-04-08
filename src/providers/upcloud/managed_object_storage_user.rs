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
    let username = properties
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("missing property: username")?;

    let body = serde_json::json!({ "username": username });

    let url = format!(
        "{}/1.3/object-storage-2/{service_uuid}/users",
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
    let username = outputs
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("missing output: username")?;

    let url = format!(
        "{}/1.3/object-storage-2/{service_uuid}/users/{username}",
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

    let mut result: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse read response: {e}"))?;
    result["service_uuid"] = serde_json::Value::String(service_uuid.to_string());

    Ok(OperationResult::Complete { outputs: result })
}

pub fn delete(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let service_uuid = outputs
        .get("service_uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing output: service_uuid")?;
    let username = outputs
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("missing output: username")?;

    let url = format!(
        "{}/1.3/object-storage-2/{service_uuid}/users/{username}",
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
