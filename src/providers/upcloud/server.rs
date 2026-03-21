use super::Client;
use crate::provider::OperationResult;

pub fn create(
    client: &Client,
    properties: serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let hostname = properties
        .get("hostname")
        .and_then(|v| v.as_str())
        .ok_or("missing property: hostname")?;
    let zone = properties
        .get("zone")
        .and_then(|v| v.as_str())
        .ok_or("missing property: zone")?;
    let plan = properties
        .get("plan")
        .and_then(|v| v.as_str())
        .ok_or("missing property: plan")?;
    let title = properties
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or("missing property: title")?;

    let storage_devices_val = properties
        .get("storage_devices")
        .and_then(|v| v.as_array())
        .ok_or("missing property: storage_devices")?;

    let mut api_storage_devices = Vec::new();
    for dev in storage_devices_val {
        let action = dev
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or("each storage_device must have an 'action' field")?;
        let size = dev
            .get("size")
            .and_then(json_as_i64)
            .ok_or("each storage_device must have a 'size' field")?;

        let mut api_dev = serde_json::json!({
            "action": action,
            "size": size
        });

        if let Some(storage) = dev.get("storage").and_then(|v| v.as_str()) {
            api_dev["storage"] = serde_json::Value::String(storage.to_string());
        }
        if let Some(dev_title) = dev.get("title").and_then(|v| v.as_str()) {
            api_dev["title"] = serde_json::Value::String(dev_title.to_string());
        }
        if let Some(tier) = dev.get("tier").and_then(|v| v.as_str()) {
            api_dev["tier"] = serde_json::Value::String(tier.to_string());
        }

        api_storage_devices.push(api_dev);
    }

    let interfaces_val = properties
        .get("interfaces")
        .and_then(|v| v.as_array())
        .ok_or("missing property: interfaces")?;

    let mut api_interfaces = Vec::new();
    for iface in interfaces_val {
        let iface_type = iface
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or("each interface must have a 'type' field")?;
        let family = iface
            .get("ip_family")
            .and_then(|v| v.as_str())
            .unwrap_or("IPv4");

        let mut api_iface = serde_json::json!({
            "ip_addresses": { "ip_address": [{ "family": family }] },
            "type": iface_type
        });

        if let Some(network) = iface.get("network").and_then(|v| v.as_str()) {
            api_iface["network"] = serde_json::Value::String(network.to_string());
        }

        api_interfaces.push(api_iface);
    }

    let networking = serde_json::json!({ "interfaces": { "interface": api_interfaces } });

    let mut server = serde_json::json!({
        "hostname": hostname,
        "title": title,
        "zone": zone,
        "plan": plan,
        "storage_devices": {
            "storage_device": api_storage_devices
        },
        "networking": networking
    });

    if let Some(metadata) = properties.get("metadata") {
        let val = match metadata {
            serde_json::Value::Bool(b) => {
                if *b {
                    "yes"
                } else {
                    "no"
                }
            }
            serde_json::Value::String(s) if s == "true" => "yes",
            serde_json::Value::String(s) if s == "false" => "no",
            _ => "no",
        };
        server["metadata"] = serde_json::Value::String(val.to_string());
    }

    let body = serde_json::json!({ "server": server });

    let url = format!("{}/1.3/server", client.base_url);
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

    let server = resp_body
        .get("server")
        .ok_or("unexpected create response: missing 'server' key")?;

    Ok(OperationResult::InProgress {
        outputs: server.clone(),
    })
}

pub fn read(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let uuid = outputs
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("read_resource: missing uuid in outputs")?;

    let url = format!("{}/1.3/server/{uuid}", client.base_url);
    let resp = client.http.get(&url).bearer_auth(&client.token).send()?;

    let status = resp.status();
    let text = resp.text()?;

    if !status.is_success() {
        return Ok(OperationResult::Failed {
            error: format!("UpCloud API error {status}: {text}"),
        });
    }

    let resp_body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse server response: {e}"))?;

    let server = resp_body
        .get("server")
        .ok_or("unexpected server response: missing 'server' key")?;

    let state = server
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match state {
        "started" => Ok(OperationResult::Complete {
            outputs: server.clone(),
        }),
        "maintenance" => Ok(OperationResult::InProgress {
            outputs: server.clone(),
        }),
        "error" => Ok(OperationResult::Failed {
            error: "server is in error state".to_string(),
        }),
        _other => Ok(OperationResult::InProgress {
            outputs: server.clone(),
        }),
    }
}

pub fn delete(
    client: &Client,
    outputs: &serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let uuid = outputs
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("delete_resource: missing uuid in outputs")?;

    // Stop the server first (ignore errors — may already be stopped)
    let stop_url = format!("{}/1.3/server/{uuid}/stop", client.base_url);
    let stop_body = serde_json::json!({
        "stop_server": {
            "stop_type": "hard",
            "timeout": "60"
        }
    });
    let _ = client
        .http
        .post(&stop_url)
        .bearer_auth(&client.token)
        .json(&stop_body)
        .send();

    // Poll until server reaches "stopped" state
    let server_url = format!("{}/1.3/server/{uuid}", client.base_url);
    let poll_interval = std::time::Duration::from_secs(5);
    let timeout = std::time::Duration::from_secs(120);
    let start = std::time::Instant::now();

    loop {
        std::thread::sleep(poll_interval);

        if start.elapsed() > timeout {
            return Err(format!("timed out waiting for server {uuid} to stop").into());
        }

        let resp = client
            .http
            .get(&server_url)
            .bearer_auth(&client.token)
            .send()?;
        if !resp.status().is_success() {
            continue;
        }
        let body: serde_json::Value = serde_json::from_str(&resp.text()?)?;
        let server_state = body
            .get("server")
            .and_then(|s| s.get("state"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match server_state {
            "stopped" => break,
            "error" => {
                return Err(format!("server {uuid} entered error state while stopping").into());
            }
            _ => {}
        }
    }

    // Delete server and its storages
    let delete_url = format!(
        "{}/1.3/server/{uuid}?storages=1&backups=delete",
        client.base_url
    );
    let resp = client
        .http
        .delete(&delete_url)
        .bearer_auth(&client.token)
        .send()?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text()?;
        return Ok(OperationResult::Failed {
            error: format!("UpCloud delete error {status}: {text}"),
        });
    }

    Ok(OperationResult::Complete {
        outputs: serde_json::Value::Object(Default::default()),
    })
}

fn json_as_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}
