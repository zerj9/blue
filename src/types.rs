use serde::{Deserialize, Serialize};
use serde_json::Value;

// --- Schema types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Number,
    Boolean,
    Array,
    Object,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub path: String,
    pub field_type: FieldType,
    pub required: bool,
    pub force_new: bool,
    pub requires_stop: bool,
    pub default: Option<Value>,
    pub items: Vec<FieldDef>,
    pub fields: Vec<FieldDef>,
    pub ordered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputDef {
    pub path: String,
    pub field_type: FieldType,
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutConfig {
    pub seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub inputs: Vec<FieldDef>,
    pub outputs: Vec<OutputDef>,
    pub retry: Option<RetryConfig>,
    pub timeout: Option<TimeoutConfig>,
}

// --- Operation types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OperationResult {
    Success { outputs: Value },
    NotFound,
    Failed {
        error: String,
        outputs: Option<Value>,
    },
}

// --- Diff types ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Action {
    Unchanged,
    Create,
    Update,
    Replace,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputChange {
    Added {
        field: String,
        value: Value,
        force_new: bool,
        requires_stop: bool,
    },
    Removed {
        field: String,
        value: Value,
        force_new: bool,
        requires_stop: bool,
    },
    Modified {
        field: String,
        old_value: Value,
        new_value: Value,
        force_new: bool,
        requires_stop: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diff {
    pub action: Action,
    pub changes: Vec<InputChange>,
    pub requires_stop: bool,
}
