use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::resolvable::Resolvable;
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str = include_str!("schemas/upcloud_network_resource.toml");

/// Fields surfaced as Blue outputs. `ip_networks` is rebuilt from the API's
/// `{ip_network: [...]}` wrapper with per-entry `dhcp_effective_routes`
/// stripped — see `extract_outputs`.
const SCALAR_OUTPUT_FIELDS: &[&str] = &["uuid", "name", "type", "zone", "router"];

/// Per-`ip_network` entry fields stripped from outputs. These are computed
/// server-side and very chatty; downstream resources should not depend on
/// them.
const STRIPPED_IP_NETWORK_FIELDS: &[&str] = &["dhcp_effective_routes"];

pub struct UpCloudNetworkResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

#[derive(Deserialize)]
struct NetworkDetailResponse {
    network: Value,
}

impl UpCloudNetworkResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud network resource schema must be valid");
        UpCloudNetworkResource { schema, client }
    }

    fn create_network(&self, inputs: &Value) -> Result<Value, String> {
        let body = build_create_body(inputs);
        let path = "/network";
        let mut resp = self.client.post(path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        let wrapper: NetworkDetailResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud POST {path} response parse failed: {e}"))?;
        Ok(wrapper.network)
    }

    fn modify_network(&self, uuid: &str, inputs: &Value) -> Result<Value, String> {
        let body = build_modify_body(inputs);
        let path = format!("/network/{uuid}");
        let mut resp = self.client.put(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud PUT {path} failed: http status: {status}: {err_body}"
            ));
        }
        let wrapper: NetworkDetailResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud PUT {path} response parse failed: {e}"))?;
        Ok(wrapper.network)
    }

    /// Fetch a single network by UUID. Returns Ok(None) on HTTP 404 so the
    /// caller can signal drift via OperationResult::NotFound.
    fn get_network(&self, uuid: &str) -> Result<Option<Value>, String> {
        let path = format!("/network/{uuid}");
        let mut resp = self.client.get(&path)?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud GET {path} failed: http status: {status}: {err_body}"
            ));
        }
        let wrapper: NetworkDetailResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
        Ok(Some(wrapper.network))
    }
}

impl ResourceType for UpCloudNetworkResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let network = self.create_network(&inputs)?;

        // Persist the uuid as soon as we have it so a crash before this
        // function returns doesn't strand the resource.
        if let Some(uuid) = network.get("uuid").and_then(|v| v.as_str()) {
            ctx.save(&json!({ "uuid": uuid }));
        }

        let outputs = extract_outputs(&network);
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let uuid = outputs
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "upcloud.network read: missing 'uuid' in stored outputs".to_string())?;
        match self.get_network(uuid)? {
            Some(network) => Ok(OperationResult::Success {
                outputs: extract_outputs(&network),
            }),
            None => Ok(OperationResult::NotFound),
        }
    }

    fn update(
        &self,
        _ctx: &dyn OperationCtx,
        _old_inputs: &Value,
        old_outputs: &Value,
        new_inputs: Value,
    ) -> Result<OperationResult, String> {
        let uuid = old_outputs
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "upcloud.network update: missing 'uuid' in old outputs".to_string())?;
        let network = self.modify_network(uuid, &new_inputs)?;
        Ok(OperationResult::Success {
            outputs: extract_outputs(&network),
        })
    }

    fn delete(&self, _ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String> {
        let uuid = outputs
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.network delete: missing 'uuid' in stored outputs".to_string()
            })?;
        let path = format!("/network/{uuid}");
        let mut resp = self.client.delete(&path)?;
        let status = resp.status().as_u16();

        // 404 = already gone; idempotent success.
        if status == 404 {
            return Ok(OperationResult::Success { outputs: json!({}) });
        }

        // 409 = network not empty (servers still attached). UpCloud has no
        // force flag for this — surface the condition with a clear next step
        // rather than blaming the API.
        if status == 409 {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud DELETE {path}: network has servers still attached; \
                 detach them from the network before destroy ({err_body})"
            ));
        }

        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud DELETE {path} failed: http status: {status}: {err_body}"
            ));
        }
        Ok(OperationResult::Success { outputs: json!({}) })
    }

    fn validate(&self, inputs: &Resolvable) -> Result<(), String> {
        // Only validate when fully concrete — pending refs get re-checked
        // at deploy time after strict resolution.
        let Some(inputs) = inputs.as_concrete() else {
            return Ok(());
        };
        let Some(ip_networks) = inputs.get("ip_networks").and_then(|v| v.as_array()) else {
            return Ok(());
        };
        for (i, entry) in ip_networks.iter().enumerate() {
            let entry_path = format!("ip_networks[{i}]");
            validate_ip_network_entry(entry, &entry_path)?;
        }
        Ok(())
    }
}

/// Plan-time checks for one `ip_networks` entry. UpCloud also validates these,
/// but they have closed value sets and surface user typos early.
fn validate_ip_network_entry(entry: &Value, path: &str) -> Result<(), String> {
    if let Some(v) = entry.get("family").and_then(|v| v.as_str()) {
        if v != "IPv4" {
            return Err(format!("{path}.family must be 'IPv4'; got '{v}'"));
        }
    }
    for field in ["dhcp", "dhcp_default_route"] {
        if let Some(v) = entry.get(field).and_then(|v| v.as_str()) {
            if v != "yes" && v != "no" {
                return Err(format!("{path}.{field} must be 'yes' or 'no'; got '{v}'"));
            }
        }
    }
    let Some(cfg) = entry.get("dhcp_routes_configuration") else {
        return Ok(());
    };
    let Some(pop) = cfg.get("effective_routes_auto_population") else {
        return Ok(());
    };
    if let Some(v) = pop.get("enabled").and_then(|v| v.as_str()) {
        if v != "yes" && v != "no" {
            return Err(format!(
                "{path}.dhcp_routes_configuration.effective_routes_auto_population.enabled must be 'yes' or 'no'; got '{v}'"
            ));
        }
    }
    if let Some(arr) = pop.get("exclude_by_source").and_then(|v| v.as_array()) {
        for v in arr {
            let s = v.as_str().unwrap_or("");
            if s != "router-connected-networks" && s != "static-route" {
                return Err(format!(
                    "{path}.dhcp_routes_configuration.effective_routes_auto_population.exclude_by_source entries must be 'router-connected-networks' or 'static-route'; got '{s}'"
                ));
            }
        }
    }
    if let Some(arr) = pop.get("filter_by_route_type").and_then(|v| v.as_array()) {
        for v in arr {
            let s = v.as_str().unwrap_or("");
            if s != "user" && s != "service" {
                return Err(format!(
                    "{path}.dhcp_routes_configuration.effective_routes_auto_population.filter_by_route_type entries must be 'user' or 'service'; got '{s}'"
                ));
            }
        }
    }
    Ok(())
}

/// Build the JSON body for `POST /network`. Wraps the resource under the
/// `"network"` key and re-shapes `ip_networks` from Blue's flat array into the
/// API's `{ip_network: [...]}` wrapper. `router` is always sent — null when
/// absent — to make "no attachment" explicit. `labels` is only sent if
/// non-empty, matching the create-time pattern used by MOS.
fn build_create_body(inputs: &Value) -> Value {
    let mut network = Map::new();
    if let Some(v) = inputs.get("name") {
        network.insert("name".to_string(), v.clone());
    }
    if let Some(v) = inputs.get("zone") {
        network.insert("zone".to_string(), v.clone());
    }
    network.insert(
        "router".to_string(),
        inputs.get("router").cloned().unwrap_or(Value::Null),
    );
    network.insert("ip_networks".to_string(), wrap_ip_networks(inputs));
    if let Some(labels) = inputs.get("labels").and_then(|v| v.as_array()) {
        if !labels.is_empty() {
            network.insert("labels".to_string(), Value::Array(labels.clone()));
        }
    }
    json!({ "network": network })
}

/// Build the JSON body for `PUT /network/{uuid}`. Same as create minus `zone`
/// (force-new — never sent in PUT). `router`, `ip_networks` and `labels` are
/// always included so removing them from config produces a real clear/detach
/// rather than UpCloud preserving stale state.
fn build_modify_body(inputs: &Value) -> Value {
    let mut network = Map::new();
    if let Some(v) = inputs.get("name") {
        network.insert("name".to_string(), v.clone());
    }
    network.insert(
        "router".to_string(),
        inputs.get("router").cloned().unwrap_or(Value::Null),
    );
    network.insert("ip_networks".to_string(), wrap_ip_networks(inputs));
    let labels = inputs
        .get("labels")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    network.insert("labels".to_string(), labels);
    json!({ "network": network })
}

/// Convert Blue's flat `ip_networks` array into the API's
/// `{"ip_network": [...]}` wrapper. Entries are passed through unmodified —
/// scalars, arrays, and the nested `dhcp_routes_configuration` block all
/// already match the API shape.
fn wrap_ip_networks(inputs: &Value) -> Value {
    let entries = inputs
        .get("ip_networks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    json!({ "ip_network": entries })
}

/// Extract Blue outputs from a UpCloud network response.
///
/// - Scalars (`uuid`, `name`, `type`, `zone`, `router`) are passed through if
///   present.
/// - `ip_networks` is un-wrapped from `{ip_network: [...]}` back into Blue's
///   flat array shape, with each entry's `dhcp_effective_routes` removed.
/// - `effective_routes` and `labels` pass through as arrays (default to
///   empty if absent).
/// - `servers`, `peerings`, and any other API field are intentionally dropped.
fn extract_outputs(network: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = network.as_object() {
        for &field in SCALAR_OUTPUT_FIELDS {
            if let Some(v) = obj.get(field) {
                out.insert(field.to_string(), v.clone());
            }
        }
    }
    out.insert("ip_networks".to_string(), unwrap_ip_networks(network));
    out.insert(
        "effective_routes".to_string(),
        network
            .get("effective_routes")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![])),
    );
    out.insert(
        "labels".to_string(),
        network
            .get("labels")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![])),
    );
    Value::Object(out)
}

/// Pull `network.ip_networks.ip_network[]` into a flat array, stripping
/// computed/chatty per-entry fields (`dhcp_effective_routes`). Returns an
/// empty array if the structure is absent or malformed.
fn unwrap_ip_networks(network: &Value) -> Value {
    let entries = network
        .get("ip_networks")
        .and_then(|v| v.get("ip_network"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let cleaned: Vec<Value> = entries
        .into_iter()
        .map(|entry| {
            if let Value::Object(mut obj) = entry {
                for &field in STRIPPED_IP_NETWORK_FIELDS {
                    obj.remove(field);
                }
                Value::Object(obj)
            } else {
                entry
            }
        })
        .collect();
    Value::Array(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolvable::Resolvable;
    use serde_json::json;

    #[test]
    fn schema_parses() {
        let _ = crate::schema::parse_schema(SCHEMA).unwrap();
    }

    #[test]
    fn build_create_body_minimal() {
        let inputs = json!({
            "name": "blue-net",
            "zone": "uk-lon1",
            "ip_networks": [
                {"address": "172.16.0.0/22"},
            ],
        });
        let body = build_create_body(&inputs);
        assert_eq!(
            body,
            json!({
                "network": {
                    "name": "blue-net",
                    "zone": "uk-lon1",
                    "router": null,
                    "ip_networks": {
                        "ip_network": [
                            {"address": "172.16.0.0/22"},
                        ],
                    },
                }
            })
        );
    }

    #[test]
    fn build_create_body_full() {
        let inputs = json!({
            "name": "blue-net",
            "zone": "uk-lon1",
            "router": "04c0df35-2658-4b0c-8ac7-962090f4e92a",
            "ip_networks": [
                {
                    "address": "172.16.0.0/22",
                    "dhcp": "yes",
                    "dhcp_default_route": "no",
                    "dhcp_dns": ["172.16.0.10", "172.16.1.10"],
                    "dhcp_routes": ["192.168.0.0/24-nexthop=10.0.1.100"],
                    "dhcp_routes_configuration": {
                        "effective_routes_auto_population": {
                            "enabled": "yes",
                            "exclude_by_source": ["static-route"],
                        },
                    },
                    "family": "IPv4",
                    "gateway": "172.16.0.1",
                },
            ],
            "labels": [{"key": "env", "value": "prod"}],
        });
        let body = build_create_body(&inputs);
        let net = body.get("network").unwrap();
        assert_eq!(net.get("name").unwrap(), "blue-net");
        assert_eq!(
            net.get("router").unwrap(),
            "04c0df35-2658-4b0c-8ac7-962090f4e92a"
        );
        let entry = net
            .get("ip_networks")
            .unwrap()
            .get("ip_network")
            .unwrap()
            .as_array()
            .unwrap()
            .first()
            .unwrap();
        assert_eq!(entry.get("address").unwrap(), "172.16.0.0/22");
        assert_eq!(entry.get("dhcp_dns").unwrap().as_array().unwrap().len(), 2);
        assert_eq!(
            entry
                .get("dhcp_routes_configuration")
                .unwrap()
                .get("effective_routes_auto_population")
                .unwrap()
                .get("enabled")
                .unwrap(),
            "yes"
        );
        assert_eq!(net.get("labels").unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn build_create_body_omits_empty_labels() {
        let inputs = json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{"address": "10.0.0.0/24"}],
            "labels": [],
        });
        let body = build_create_body(&inputs);
        let net = body.get("network").unwrap().as_object().unwrap();
        assert!(!net.contains_key("labels"));
    }

    #[test]
    fn build_create_body_sends_router_null_when_absent() {
        let inputs = json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{"address": "10.0.0.0/24"}],
        });
        let body = build_create_body(&inputs);
        assert_eq!(
            body.get("network").unwrap().get("router").unwrap(),
            &Value::Null
        );
    }

    #[test]
    fn build_modify_body_omits_zone() {
        let inputs = json!({
            "name": "renamed",
            "zone": "uk-lon1", // force_new — must NOT appear
            "router": "abc",
            "ip_networks": [{"address": "10.0.0.0/24"}],
        });
        let body = build_modify_body(&inputs);
        let net = body.get("network").unwrap().as_object().unwrap();
        assert!(!net.contains_key("zone"));
        assert_eq!(net.get("name").unwrap(), "renamed");
        assert_eq!(net.get("router").unwrap(), "abc");
    }

    #[test]
    fn build_modify_body_sends_empty_labels_to_clear() {
        // User removed labels from config — PUT must send labels: [] so
        // UpCloud actually clears them rather than preserving prior values.
        let inputs = json!({
            "name": "x",
            "ip_networks": [{"address": "10.0.0.0/24"}],
        });
        let body = build_modify_body(&inputs);
        assert_eq!(
            body.get("network").unwrap().get("labels").unwrap(),
            &json!([])
        );
    }

    #[test]
    fn build_modify_body_sends_router_null_to_detach() {
        let inputs = json!({
            "name": "x",
            "ip_networks": [{"address": "10.0.0.0/24"}],
        });
        let body = build_modify_body(&inputs);
        assert_eq!(
            body.get("network").unwrap().get("router").unwrap(),
            &Value::Null
        );
    }

    #[test]
    fn extract_outputs_strips_dhcp_effective_routes() {
        let network = json!({
            "uuid": "u1",
            "name": "n",
            "type": "private",
            "zone": "uk-lon1",
            "ip_networks": {
                "ip_network": [
                    {
                        "address": "172.16.0.0/22",
                        "gateway": "172.16.0.1",
                        "dhcp_effective_routes": [
                            {"auto_populated": "yes", "route": "10.0.0.0/24", "nexthop": "172.16.0.1"}
                        ],
                    },
                ],
            },
            "effective_routes": [],
            "labels": [],
        });
        let outputs = extract_outputs(&network);
        let entries = outputs.get("ip_networks").unwrap().as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = entries.first().unwrap().as_object().unwrap();
        assert_eq!(entry.get("address").unwrap(), "172.16.0.0/22");
        assert!(!entry.contains_key("dhcp_effective_routes"));
    }

    #[test]
    fn extract_outputs_flattens_ip_networks_wrapper() {
        let network = json!({
            "uuid": "u1",
            "ip_networks": {"ip_network": [{"address": "10.0.0.0/24"}]},
        });
        let outputs = extract_outputs(&network);
        assert!(outputs.get("ip_networks").unwrap().is_array());
    }

    #[test]
    fn extract_outputs_omits_unexposed_fields() {
        let network = json!({
            "uuid": "u1",
            "name": "n",
            "type": "private",
            "zone": "uk-lon1",
            "router": "r1",
            "ip_networks": {"ip_network": []},
            "effective_routes": [{"source": "router-connected-network", "route": "10.0.0.0/24"}],
            "labels": [{"key": "k", "value": "v"}],
            "servers": {"server": [{"uuid": "s1", "title": "s"}]},
            "peerings": {"peering": [{"uuid": "p1", "name": "p", "state": "active"}]},
        });
        let outputs = extract_outputs(&network);
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj["uuid"], "u1");
        assert_eq!(obj["router"], "r1");
        assert_eq!(obj["effective_routes"].as_array().unwrap().len(), 1);
        assert_eq!(obj["labels"].as_array().unwrap().len(), 1);
        assert!(!obj.contains_key("servers"));
        assert!(!obj.contains_key("peerings"));
    }

    #[test]
    fn extract_outputs_defaults_arrays_when_absent() {
        let network = json!({"uuid": "u1"});
        let outputs = extract_outputs(&network);
        assert_eq!(outputs.get("ip_networks").unwrap(), &json!([]));
        assert_eq!(outputs.get("effective_routes").unwrap(), &json!([]));
        assert_eq!(outputs.get("labels").unwrap(), &json!([]));
    }

    fn res() -> UpCloudNetworkResource {
        UpCloudNetworkResource::new(Arc::new(UpCloudClient::new(String::new())))
    }

    #[test]
    fn validate_rejects_bad_dhcp() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{"address": "10.0.0.0/24", "dhcp": "true"}],
        }));
        let err = res().validate(&inputs).unwrap_err();
        assert!(err.contains("dhcp"), "got: {err}");
        assert!(err.contains("'yes' or 'no'"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_dhcp_default_route() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{"address": "10.0.0.0/24", "dhcp_default_route": "maybe"}],
        }));
        let err = res().validate(&inputs).unwrap_err();
        assert!(err.contains("dhcp_default_route"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_enabled() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{
                "address": "10.0.0.0/24",
                "dhcp_routes_configuration": {
                    "effective_routes_auto_population": {"enabled": "true"},
                },
            }],
        }));
        let err = res().validate(&inputs).unwrap_err();
        assert!(err.contains("enabled"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_exclude_by_source() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{
                "address": "10.0.0.0/24",
                "dhcp_routes_configuration": {
                    "effective_routes_auto_population": {
                        "exclude_by_source": ["something-bogus"],
                    },
                },
            }],
        }));
        let err = res().validate(&inputs).unwrap_err();
        assert!(err.contains("exclude_by_source"), "got: {err}");
        assert!(err.contains("something-bogus"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_filter_by_route_type() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{
                "address": "10.0.0.0/24",
                "dhcp_routes_configuration": {
                    "effective_routes_auto_population": {
                        "filter_by_route_type": ["typo"],
                    },
                },
            }],
        }));
        let err = res().validate(&inputs).unwrap_err();
        assert!(err.contains("filter_by_route_type"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_family() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{"address": "10.0.0.0/24", "family": "IPv6"}],
        }));
        let err = res().validate(&inputs).unwrap_err();
        assert!(err.contains("family"), "got: {err}");
    }

    #[test]
    fn validate_passes_on_valid_inputs() {
        let inputs = Resolvable::known(json!({
            "name": "x",
            "zone": "uk-lon1",
            "ip_networks": [{
                "address": "10.0.0.0/24",
                "dhcp": "yes",
                "dhcp_default_route": "no",
                "family": "IPv4",
                "dhcp_routes_configuration": {
                    "effective_routes_auto_population": {
                        "enabled": "yes",
                        "exclude_by_source": ["router-connected-networks", "static-route"],
                        "filter_by_route_type": ["user", "service"],
                    },
                },
            }],
        }));
        res().validate(&inputs).unwrap();
    }
}
