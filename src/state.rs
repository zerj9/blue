use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub lineage: String,
    pub serial: u64,
    #[serde(default)]
    pub resources: HashMap<String, ResourceState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceState {
    #[serde(rename = "type")]
    pub resource_type: String,
    pub inputs: Value,
    pub outputs: Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl State {
    pub fn new() -> Self {
        State {
            lineage: Uuid::new_v4().to_string(),
            serial: 0,
            resources: HashMap::new(),
        }
    }
}

pub fn read_state(path: &Path) -> Result<State, String> {
    if !path.exists() {
        return Ok(State::new());
    }
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read state file {}: {e}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse state file {}: {e}", path.display()))
}

pub fn write_state(path: &Path, state: &mut State) -> Result<(), String> {
    state.serial += 1;
    let contents = serde_json::to_string_pretty(state)
        .map_err(|e| format!("Failed to serialize state: {e}"))?;
    fs::write(path, contents)
        .map_err(|e| format!("Failed to write state file {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn new_state_has_lineage() {
        let state = State::new();
        assert!(!state.lineage.is_empty());
        assert_eq!(state.serial, 0);
        assert!(state.resources.is_empty());
    }

    #[test]
    fn read_nonexistent_returns_new() {
        let state = read_state(Path::new("/tmp/blue_test_nonexistent.json")).unwrap();
        assert_eq!(state.serial, 0);
        assert!(state.resources.is_empty());
    }

    #[test]
    fn roundtrip() {
        let path = Path::new("/tmp/blue_test_roundtrip.json");

        let mut state = State::new();
        state.resources.insert(
            "web-01".to_string(),
            ResourceState {
                resource_type: "upcloud.server".to_string(),
                inputs: json!({"hostname": "web-01", "zone": "uk-lon1"}),
                outputs: json!({"uuid": "abc-123", "state": "started"}),
                depends_on: vec!["resources.object-store".to_string()],
            },
        );

        write_state(path, &mut state).unwrap();
        assert_eq!(state.serial, 1);

        let loaded = read_state(path).unwrap();
        assert_eq!(loaded.lineage, state.lineage);
        assert_eq!(loaded.serial, 1);
        assert_eq!(loaded.resources.len(), 1);
        assert_eq!(loaded.resources["web-01"].resource_type, "upcloud.server");
        assert_eq!(loaded.resources["web-01"].outputs["uuid"], "abc-123");
        assert_eq!(
            loaded.resources["web-01"].depends_on,
            vec!["resources.object-store"]
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn serial_increments_on_each_write() {
        let path = Path::new("/tmp/blue_test_serial.json");

        let mut state = State::new();
        write_state(path, &mut state).unwrap();
        assert_eq!(state.serial, 1);
        write_state(path, &mut state).unwrap();
        assert_eq!(state.serial, 2);

        let loaded = read_state(path).unwrap();
        assert_eq!(loaded.serial, 2);

        fs::remove_file(path).ok();
    }
}
