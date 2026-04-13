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

    if let Some(firewall) = properties.get("firewall").and_then(|v| v.as_str()) {
        server["firewall"] = serde_json::Value::String(firewall.to_string());
    }

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

    if let Some(user_data) = properties.get("user_data").and_then(|v| v.as_str()) {
        server["user_data"] = serde_json::Value::String(user_data.to_string());
    }

    if let Some(login_user) = properties.get("login_user").and_then(|v| v.as_object()) {
        let mut api_login = serde_json::json!({});
        if let Some(username) = login_user.get("username").and_then(|v| v.as_str()) {
            api_login["username"] = serde_json::Value::String(username.to_string());
        }
        if let Some(create_pw) = login_user.get("create_password").and_then(|v| v.as_str()) {
            api_login["create_password"] = serde_json::Value::String(create_pw.to_string());
        }
        if let Some(keys) = login_user.get("ssh_keys").and_then(|v| v.as_array()) {
            let ssh_keys: Vec<serde_json::Value> = keys
                .iter()
                .filter_map(|k| k.as_str().map(|s| serde_json::Value::String(s.to_string())))
                .collect();
            api_login["ssh_keys"] = serde_json::json!({ "ssh_key": ssh_keys });
        }
        server["login_user"] = api_login;
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
        "started" | "stopped" => Ok(OperationResult::Complete {
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

pub fn update(
    client: &Client,
    old_outputs: &serde_json::Value,
    new_properties: serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    // Extract server UUID from old outputs
    let uuid = old_outputs
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or("missing uuid in server outputs")?;

    // Build the update request
    let mut update_request = serde_json::json!({
        "server": {}
    });

    // Add simple updatable properties (can be updated on running servers)
    if let Some(title) = new_properties.get("title").and_then(|v| v.as_str()) {
        update_request["server"]["title"] = serde_json::Value::String(title.to_string());
    }

    if let Some(hostname) = new_properties.get("hostname").and_then(|v| v.as_str()) {
        update_request["server"]["hostname"] = serde_json::Value::String(hostname.to_string());
    }

    if let Some(firewall) = new_properties.get("firewall").and_then(|v| v.as_str()) {
        update_request["server"]["firewall"] = serde_json::Value::String(firewall.to_string());
    }

    if let Some(metadata) = new_properties.get("metadata").and_then(|v| v.as_str()) {
        update_request["server"]["metadata"] = serde_json::Value::String(metadata.to_string());
    }

    if let Some(simple_backup) = new_properties.get("simple_backup") {
        update_request["server"]["simple_backup"] = simple_backup.clone();
    }

    if let Some(timezone) = new_properties.get("timezone").and_then(|v| v.as_str()) {
        update_request["server"]["timezone"] = serde_json::Value::String(timezone.to_string());
    }

    if let Some(boot_order) = new_properties.get("boot_order").and_then(|v| v.as_str()) {
        update_request["server"]["boot_order"] = serde_json::Value::String(boot_order.to_string());
    }

    if let Some(nic_model) = new_properties.get("nic_model").and_then(|v| v.as_str()) {
        update_request["server"]["nic_model"] = serde_json::Value::String(nic_model.to_string());
    }

    if let Some(video_model) = new_properties.get("video_model").and_then(|v| v.as_str()) {
        update_request["server"]["video_model"] =
            serde_json::Value::String(video_model.to_string());
    }

    // Complex updates that may require server to be stopped
    // These are commented out for now as they require more complex handling
    // and state checking

    /*
    if let Some(core_number) = new_properties.get("core_number") {
        update_request["server"]["core_number"] = core_number.clone();
    }

    if let Some(memory_amount) = new_properties.get("memory_amount") {
        update_request["server"]["memory_amount"] = memory_amount.clone();
    }

    if let Some(plan) = new_properties.get("plan").and_then(|v| v.as_str()) {
        update_request["server"]["plan"] = serde_json::Value::String(plan.to_string());
    }
    */

    // Note: Storage devices and IP addresses require separate API calls
    // and are not handled by this update operation

    let mut outputs = old_outputs.clone();

    // Only send the PUT if there are property changes
    let server_obj = update_request["server"].as_object().unwrap();
    if !server_obj.is_empty() {
        let url = format!("{}/1.3/server/{}", client.base_url, uuid);
        let resp = client
            .http
            .put(&url)
            .bearer_auth(&client.token)
            .json(&update_request)
            .send()?;

        let status = resp.status();
        let text = resp.text()?;

        if !status.is_success() {
            return Ok(OperationResult::Failed {
                error: format!("UpCloud API error {status}: {text}"),
            });
        }
        let body: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("failed to parse server update response: {e}"))?;

        let server_info = body
            .get("server")
            .and_then(|s| s.as_object())
            .ok_or_else(|| "unexpected server update response: expected server object")?;

        if let Some(title) = server_info.get("title").and_then(|v| v.as_str()) {
            outputs["title"] = serde_json::Value::String(title.to_string());
        }
        if let Some(hostname) = server_info.get("hostname").and_then(|v| v.as_str()) {
            outputs["hostname"] = serde_json::Value::String(hostname.to_string());
        }
        if let Some(firewall) = server_info.get("firewall").and_then(|v| v.as_str()) {
            outputs["firewall"] = serde_json::Value::String(firewall.to_string());
        }
        if let Some(metadata) = server_info.get("metadata").and_then(|v| v.as_str()) {
            outputs["metadata"] = serde_json::Value::String(metadata.to_string());
        }
        if let Some(simple_backup) = server_info.get("simple_backup") {
            outputs["simple_backup"] = simple_backup.clone();
        }
        if let Some(timezone) = server_info.get("timezone").and_then(|v| v.as_str()) {
            outputs["timezone"] = serde_json::Value::String(timezone.to_string());
        }
        if let Some(boot_order) = server_info.get("boot_order").and_then(|v| v.as_str()) {
            outputs["boot_order"] = serde_json::Value::String(boot_order.to_string());
        }
        if let Some(nic_model) = server_info.get("nic_model").and_then(|v| v.as_str()) {
            outputs["nic_model"] = serde_json::Value::String(nic_model.to_string());
        }
        if let Some(video_model) = server_info.get("video_model").and_then(|v| v.as_str()) {
            outputs["video_model"] = serde_json::Value::String(video_model.to_string());
        }
        if let Some(state) = server_info.get("state").and_then(|v| v.as_str()) {
            outputs["state"] = serde_json::Value::String(state.to_string());
        }
    }

    // Handle state changes via start/stop endpoints
    let desired_state = new_properties.get("state").and_then(|v| v.as_str());
    let current_state = old_outputs.get("state").and_then(|v| v.as_str());

    if let Some(desired) = desired_state {
        let needs_stop = desired == "stopped" && current_state == Some("started");
        let needs_start = desired == "started" && current_state == Some("stopped");

        if needs_stop {
            let stop_url = format!("{}/1.3/server/{uuid}/stop", client.base_url);
            let stop_body = serde_json::json!({
                "stop_server": {
                    "stop_type": "soft",
                    "timeout": "60"
                }
            });
            match client
                .http
                .post(&stop_url)
                .bearer_auth(&client.token)
                .json(&stop_body)
                .send()
            {
                Ok(resp) if !resp.status().is_success() => {
                    let text = resp.text().unwrap_or_default();
                    return Ok(OperationResult::Failed {
                        error: format!("failed to stop server: {text}"),
                    });
                }
                _ => {}
            }
            return poll_state(client, uuid, "stopped", &mut outputs);
        }

        if needs_start {
            let start_url = format!("{}/1.3/server/{uuid}/start", client.base_url);
            match client
                .http
                .post(&start_url)
                .bearer_auth(&client.token)
                .json(&serde_json::json!({"server": {"start_type": "async"}}))
                .send()
            {
                Ok(resp) if !resp.status().is_success() => {
                    let text = resp.text().unwrap_or_default();
                    return Ok(OperationResult::Failed {
                        error: format!("failed to start server: {text}"),
                    });
                }
                _ => {}
            }
            return poll_state(client, uuid, "started", &mut outputs);
        }
    }

    Ok(OperationResult::Complete { outputs })
}

fn poll_state(
    client: &Client,
    uuid: &str,
    target_state: &str,
    outputs: &mut serde_json::Value,
) -> Result<OperationResult, Box<dyn std::error::Error>> {
    let server_url = format!("{}/1.3/server/{uuid}", client.base_url);
    let poll_interval = std::time::Duration::from_secs(5);
    let timeout = std::time::Duration::from_secs(120);
    let start = std::time::Instant::now();

    loop {
        std::thread::sleep(poll_interval);

        if start.elapsed() > timeout {
            return Err(
                format!("timed out waiting for server {uuid} to reach state '{target_state}'")
                    .into(),
            );
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
        let server = body.get("server").ok_or("missing 'server' key in response")?;
        let state = server
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if state == target_state {
            outputs["state"] = serde_json::Value::String(state.to_string());
            return Ok(OperationResult::Complete {
                outputs: outputs.clone(),
            });
        }

        if state == "error" {
            return Ok(OperationResult::Failed {
                error: format!("server {uuid} entered error state"),
            });
        }
    }
}
