use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Integer,
    Float,
    Boolean,
    Array,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldType::String => write!(f, "string"),
            FieldType::Integer => write!(f, "integer"),
            FieldType::Float => write!(f, "float"),
            FieldType::Boolean => write!(f, "boolean"),
            FieldType::Array => write!(f, "array"),
        }
    }
}

pub struct FieldDef {
    pub path: String,
    pub field_type: FieldType,
    pub required: bool,
    pub force_new: bool,
    pub requires_stop: bool,
    pub items: Vec<FieldDef>,
}

pub struct OutputDef {
    pub path: String,
    pub output_type: FieldType,
}

pub struct Schema {
    fields: Vec<FieldDef>,
    pub outputs: Vec<OutputDef>,
}

pub enum ValidationError {
    MissingRequired {
        resource: String,
        field: String,
    },
    UnknownField {
        resource: String,
        field: String,
    },
    TypeMismatch {
        resource: String,
        field: String,
        expected: String,
        got: String,
    },
    UnknownDataSource {
        resource: String,
        field: String,
        source: String,
    },
    UnknownDataField {
        resource: String,
        field: String,
        source: String,
        output: String,
    },
    UnknownResourceRef {
        resource: String,
        field: String,
        referenced: String,
    },
    UnknownResourceField {
        resource: String,
        field: String,
        referenced: String,
        output: String,
    },
    RefTypeMismatch {
        resource: String,
        field: String,
        expected: FieldType,
        ref_path: String,
        got: FieldType,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::MissingRequired { resource, field } => {
                write!(f, "resources.{resource}: missing required field '{field}'")
            }
            ValidationError::UnknownField { resource, field } => {
                write!(f, "resources.{resource}: unknown field '{field}'")
            }
            ValidationError::TypeMismatch {
                resource,
                field,
                expected,
                got,
            } => {
                write!(
                    f,
                    "resources.{resource}: field '{field}' expects {expected}, got '{got}'"
                )
            }
            ValidationError::UnknownDataSource {
                resource,
                field,
                source,
            } => {
                write!(
                    f,
                    "resources.{resource}: field '{field}' references unknown data source '{source}'"
                )
            }
            ValidationError::UnknownDataField {
                resource,
                field,
                source,
                output,
            } => {
                write!(
                    f,
                    "resources.{resource}: field '{field}' references unknown output '{output}' on data source '{source}'"
                )
            }
            ValidationError::UnknownResourceRef {
                resource,
                field,
                referenced,
            } => {
                write!(
                    f,
                    "resources.{resource}: field '{field}' references unknown resource '{referenced}'"
                )
            }
            ValidationError::UnknownResourceField {
                resource,
                field,
                referenced,
                output,
            } => {
                write!(
                    f,
                    "resources.{resource}: field '{field}' references unknown output '{output}' on resource '{referenced}'"
                )
            }
            ValidationError::RefTypeMismatch {
                resource,
                field,
                expected,
                ref_path,
                got,
            } => {
                write!(
                    f,
                    "resources.{resource}: field '{field}' expects {expected}, but {ref_path} is {got}"
                )
            }
        }
    }
}

pub struct ValidateContext<'a> {
    pub data_schemas: HashMap<String, &'a Schema>,
    pub resource_schemas: HashMap<String, &'a Schema>,
    pub data_hook_outputs: HashMap<String, Vec<&'a crate::config::HookOutput>>,
    pub resource_hook_outputs: HashMap<String, Vec<&'a crate::config::HookOutput>>,
}

#[derive(Deserialize)]
struct SchemaFile {
    #[serde(default)]
    fields: Vec<RawFieldDef>,
    #[serde(default)]
    outputs: Vec<RawOutputDef>,
}

#[derive(Deserialize)]
struct RawFieldDef {
    path: String,
    #[serde(rename = "type")]
    field_type: FieldType,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    force_new: bool,
    #[serde(default)]
    requires_stop: bool,
    #[serde(default)]
    items: Vec<RawFieldDef>,
}

#[derive(Deserialize)]
struct RawOutputDef {
    path: String,
    #[serde(rename = "type")]
    output_type: FieldType,
}

impl Schema {
    pub fn from_toml(s: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let file: SchemaFile = toml::from_str(s)?;
        let fields = file.fields.into_iter().map(convert_field).collect();
        let outputs = file
            .outputs
            .into_iter()
            .map(|raw| OutputDef {
                path: raw.path,
                output_type: raw.output_type,
            })
            .collect();
        Ok(Schema { fields, outputs })
    }

    pub fn output(&self, path: &str) -> Option<&OutputDef> {
        self.outputs.iter().find(|o| o.path == path)
    }

    pub fn is_force_new(&self, path: &str) -> bool {
        self.fields.iter().any(|f| f.path == path && f.force_new)
    }

    pub fn requires_stop(&self, path: &str) -> bool {
        self.fields
            .iter()
            .any(|f| f.path == path && f.requires_stop)
    }

    pub fn validate(&self, resource_name: &str, properties: &toml::Value) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        let mut flat = Vec::new();
        flatten("", properties, &mut flat);

        // Check required fields are present
        for field in &self.fields {
            if field.required && !flat.iter().any(|(path, _)| path == &field.path) {
                errors.push(ValidationError::MissingRequired {
                    resource: resource_name.to_string(),
                    field: field.path.clone(),
                });
            }
        }

        // Check each property against the schema
        for (path, value) in &flat {
            match self.fields.iter().find(|f| &f.path == path) {
                None => {
                    errors.push(ValidationError::UnknownField {
                        resource: resource_name.to_string(),
                        field: path.clone(),
                    });
                }
                Some(field) => {
                    if !type_matches(&field.field_type, value) {
                        errors.push(ValidationError::TypeMismatch {
                            resource: resource_name.to_string(),
                            field: path.clone(),
                            expected: field.field_type.to_string(),
                            got: toml_type_description(value),
                        });
                    }
                    if field.field_type == FieldType::Array
                        && !field.items.is_empty()
                        && let toml::Value::Array(elements) = value
                    {
                        validate_array_elements(
                            resource_name,
                            path,
                            elements,
                            &field.items,
                            &mut errors,
                        );
                    }
                }
            }
        }

        errors
    }

    pub fn validate_with_refs(
        &self,
        resource_name: &str,
        properties: &toml::Value,
        ctx: &ValidateContext,
    ) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        let mut flat = Vec::new();
        flatten("", properties, &mut flat);

        // Check required fields
        for field in &self.fields {
            if field.required && !flat.iter().any(|(path, _)| path == &field.path) {
                errors.push(ValidationError::MissingRequired {
                    resource: resource_name.to_string(),
                    field: field.path.clone(),
                });
            }
        }

        // Check each property against the schema with reference awareness
        for (path, value) in &flat {
            match self.fields.iter().find(|f| &f.path == path) {
                None => {
                    errors.push(ValidationError::UnknownField {
                        resource: resource_name.to_string(),
                        field: path.clone(),
                    });
                }
                Some(field_def) => {
                    validate_field_value(resource_name, path, value, field_def, ctx, &mut errors);
                }
            }
        }

        errors
    }
}

fn validate_array_elements(
    resource_name: &str,
    field_path: &str,
    elements: &[toml::Value],
    items: &[FieldDef],
    errors: &mut Vec<ValidationError>,
) {
    for (i, element) in elements.iter().enumerate() {
        if element.as_table().is_none() {
            errors.push(ValidationError::TypeMismatch {
                resource: resource_name.to_string(),
                field: format!("{field_path}[{i}]"),
                expected: "table".to_string(),
                got: toml_type_description(element),
            });
            continue;
        }

        let mut flat = Vec::new();
        flatten("", element, &mut flat);

        for item in items {
            if item.required && !flat.iter().any(|(p, _)| p == &item.path) {
                errors.push(ValidationError::MissingRequired {
                    resource: resource_name.to_string(),
                    field: format!("{field_path}[{i}].{}", item.path),
                });
            }
        }

        for (key, val) in &flat {
            match items.iter().find(|item| &item.path == key) {
                None => {
                    errors.push(ValidationError::UnknownField {
                        resource: resource_name.to_string(),
                        field: format!("{field_path}[{i}].{key}"),
                    });
                }
                Some(item_def) => {
                    if !type_matches(&item_def.field_type, val) {
                        errors.push(ValidationError::TypeMismatch {
                            resource: resource_name.to_string(),
                            field: format!("{field_path}[{i}].{key}"),
                            expected: item_def.field_type.to_string(),
                            got: toml_type_description(val),
                        });
                    }
                }
            }
        }
    }
}

fn validate_array_elements_with_refs(
    resource_name: &str,
    field_path: &str,
    elements: &[toml::Value],
    items: &[FieldDef],
    ctx: &ValidateContext,
    errors: &mut Vec<ValidationError>,
) {
    for (i, element) in elements.iter().enumerate() {
        if element.as_table().is_none() {
            errors.push(ValidationError::TypeMismatch {
                resource: resource_name.to_string(),
                field: format!("{field_path}[{i}]"),
                expected: "table".to_string(),
                got: toml_type_description(element),
            });
            continue;
        }

        let mut flat = Vec::new();
        flatten("", element, &mut flat);

        for item in items {
            if item.required && !flat.iter().any(|(p, _)| p == &item.path) {
                errors.push(ValidationError::MissingRequired {
                    resource: resource_name.to_string(),
                    field: format!("{field_path}[{i}].{}", item.path),
                });
            }
        }

        for (key, val) in &flat {
            let element_field = format!("{field_path}[{i}].{key}");
            match items.iter().find(|item| &item.path == key) {
                None => {
                    errors.push(ValidationError::UnknownField {
                        resource: resource_name.to_string(),
                        field: element_field,
                    });
                }
                Some(item_def) => {
                    validate_field_value(resource_name, &element_field, val, item_def, ctx, errors);
                }
            }
        }
    }
}

/// Validate a single field value against its schema definition with reference awareness.
fn validate_field_value(
    resource_name: &str,
    field_path: &str,
    value: &toml::Value,
    field_def: &FieldDef,
    ctx: &ValidateContext,
    errors: &mut Vec<ValidationError>,
) {
    match classify_value(value) {
        RefKind::Literal => {
            if !type_matches(&field_def.field_type, value) {
                errors.push(ValidationError::TypeMismatch {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    expected: field_def.field_type.to_string(),
                    got: toml_type_description(value),
                });
            }
            if field_def.field_type == FieldType::Array
                && !field_def.items.is_empty()
                && let toml::Value::Array(elements) = value
            {
                validate_array_elements_with_refs(
                    resource_name,
                    field_path,
                    elements,
                    &field_def.items,
                    ctx,
                    errors,
                );
            }
        }
        RefKind::DataRef { source, field } => match ctx.data_schemas.get(&source) {
            None => {
                errors.push(ValidationError::UnknownDataSource {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    source,
                });
            }
            Some(schema) => match schema.output(&field) {
                None => {
                    errors.push(ValidationError::UnknownDataField {
                        resource: resource_name.to_string(),
                        field: field_path.to_string(),
                        source,
                        output: field,
                    });
                }
                Some(output_def) => {
                    if !output_type_compatible(&field_def.field_type, &output_def.output_type) {
                        errors.push(ValidationError::RefTypeMismatch {
                            resource: resource_name.to_string(),
                            field: field_path.to_string(),
                            expected: field_def.field_type.clone(),
                            ref_path: format!("data.{source}.{field}"),
                            got: output_def.output_type.clone(),
                        });
                    }
                }
            },
        },
        RefKind::ResourceRef { resource, field } => match ctx.resource_schemas.get(&resource) {
            None => {
                errors.push(ValidationError::UnknownResourceRef {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    referenced: resource,
                });
            }
            Some(schema) => match schema.output(&field) {
                None => {
                    errors.push(ValidationError::UnknownResourceField {
                        resource: resource_name.to_string(),
                        field: field_path.to_string(),
                        referenced: resource,
                        output: field,
                    });
                }
                Some(output_def) => {
                    if !output_type_compatible(&field_def.field_type, &output_def.output_type) {
                        errors.push(ValidationError::RefTypeMismatch {
                            resource: resource_name.to_string(),
                            field: field_path.to_string(),
                            expected: field_def.field_type.clone(),
                            ref_path: format!("resources.{resource}.{field}"),
                            got: output_def.output_type.clone(),
                        });
                    }
                }
            },
        },
        RefKind::DataHookRef { source, output } => {
            if !ctx.data_schemas.contains_key(&source) {
                errors.push(ValidationError::UnknownDataSource {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    source,
                });
            } else if let Some(hook_output) = ctx
                .data_hook_outputs
                .get(&source)
                .and_then(|outputs| outputs.iter().find(|o| o.name == output))
            {
                if let Some(output_type) = hook_output_type_to_field_type(&hook_output.r#type) {
                    if !output_type_compatible(&field_def.field_type, &output_type) {
                        errors.push(ValidationError::RefTypeMismatch {
                            resource: resource_name.to_string(),
                            field: field_path.to_string(),
                            expected: field_def.field_type.clone(),
                            ref_path: format!("data.{source}.hooks.outputs.{output}"),
                            got: output_type,
                        });
                    }
                }
            } else {
                errors.push(ValidationError::UnknownDataField {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    source,
                    output,
                });
            }
        }
        RefKind::ResourceHookRef { resource, output } => {
            if !ctx.resource_schemas.contains_key(&resource) {
                errors.push(ValidationError::UnknownResourceRef {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    referenced: resource,
                });
            } else if let Some(hook_output) = ctx
                .resource_hook_outputs
                .get(&resource)
                .and_then(|outputs| outputs.iter().find(|o| o.name == output))
            {
                if let Some(output_type) = hook_output_type_to_field_type(&hook_output.r#type) {
                    if !output_type_compatible(&field_def.field_type, &output_type) {
                        errors.push(ValidationError::RefTypeMismatch {
                            resource: resource_name.to_string(),
                            field: field_path.to_string(),
                            expected: field_def.field_type.clone(),
                            ref_path: format!("resources.{resource}.hooks.outputs.{output}"),
                            got: output_type,
                        });
                    }
                }
            } else {
                errors.push(ValidationError::UnknownResourceField {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    referenced: resource,
                    output,
                });
            }
        }
        RefKind::MixedTemplate => {
            if field_def.field_type != FieldType::String {
                errors.push(ValidationError::TypeMismatch {
                    resource: resource_name.to_string(),
                    field: field_path.to_string(),
                    expected: field_def.field_type.to_string(),
                    got: "mixed template (always string)".to_string(),
                });
            }
        }
    }
}

fn convert_field(raw: RawFieldDef) -> FieldDef {
    FieldDef {
        path: raw.path,
        field_type: raw.field_type,
        required: raw.required,
        force_new: raw.force_new,
        requires_stop: raw.requires_stop,
        items: raw.items.into_iter().map(convert_field).collect(),
    }
}

pub fn flatten<'a>(prefix: &str, value: &'a toml::Value, out: &mut Vec<(String, &'a toml::Value)>) {
    if let toml::Value::Table(table) = value {
        for (key, val) in table {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            if val.is_table() {
                flatten(&path, val, out);
            } else {
                out.push((path, val));
            }
        }
    }
}

fn type_matches(expected: &FieldType, value: &toml::Value) -> bool {
    match expected {
        FieldType::String => matches!(value, toml::Value::String(_)),
        FieldType::Integer => match value {
            toml::Value::Integer(_) => true,
            toml::Value::String(s) => s.parse::<i64>().is_ok(),
            _ => false,
        },
        FieldType::Float => match value {
            toml::Value::Float(_) | toml::Value::Integer(_) => true,
            toml::Value::String(s) => s.parse::<f64>().is_ok(),
            _ => false,
        },
        FieldType::Boolean => match value {
            toml::Value::Boolean(_) => true,
            toml::Value::String(s) => s == "true" || s == "false",
            _ => false,
        },
        FieldType::Array => matches!(value, toml::Value::Array(_)),
    }
}

enum RefKind {
    Literal,
    DataRef { source: String, field: String },
    ResourceRef { resource: String, field: String },
    DataHookRef { source: String, output: String },
    ResourceHookRef { resource: String, output: String },
    MixedTemplate,
}

fn classify_value(value: &toml::Value) -> RefKind {
    use crate::reference::Ref;

    if let toml::Value::String(s) = value {
        let trimmed = s.trim();
        if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
            let inner = trimmed[2..trimmed.len() - 2].trim();
            if !inner.contains("{{") && !inner.contains("}}") {
                if let Some(r) = Ref::parse(inner) {
                    let hook_output = r.hook_output_name().map(|s| s.to_string());
                    return match (r.source.as_str(), r.is_hook_output()) {
                        ("data", true) => RefKind::DataHookRef {
                            source: r.name,
                            output: hook_output.unwrap(),
                        },
                        ("data", false) => RefKind::DataRef {
                            source: r.name,
                            field: r.path,
                        },
                        ("resources", true) => RefKind::ResourceHookRef {
                            resource: r.name,
                            output: hook_output.unwrap(),
                        },
                        ("resources", false) => RefKind::ResourceRef {
                            resource: r.name,
                            field: r.path,
                        },
                        _ => RefKind::Literal,
                    };
                }
            }
        }
        if s.contains("{{") {
            return RefKind::MixedTemplate;
        }
    }
    RefKind::Literal
}

fn output_type_compatible(expected: &FieldType, output: &FieldType) -> bool {
    if expected == output {
        return true;
    }
    // Integer → Float widening
    if *expected == FieldType::Float && *output == FieldType::Integer {
        return true;
    }
    false
}

fn hook_output_type_to_field_type(type_str: &str) -> Option<FieldType> {
    match type_str {
        "string" => Some(FieldType::String),
        "integer" => Some(FieldType::Integer),
        "float" => Some(FieldType::Float),
        "boolean" => Some(FieldType::Boolean),
        "array" => Some(FieldType::Array),
        _ => None,
    }
}

fn toml_type_description(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Array(_) => "array".to_string(),
        toml::Value::Table(_) => "table".to_string(),
        toml::Value::Datetime(d) => d.to_string(),
    }
}

pub fn extract_outputs(
    value: &serde_json::Value,
    outputs: &[OutputDef],
) -> Result<HashMap<String, serde_json::Value>, Box<dyn std::error::Error>> {
    let mut result = HashMap::new();
    for output in outputs {
        let segments: Vec<&str> = output.path.split('.').collect();
        let mut leaves = Vec::new();
        resolve_output_path(&segments, value, &output.path, &mut leaves);
        for (key, leaf) in leaves {
            if !leaf.is_null() && json_type_matches(&output.output_type, leaf) {
                result.insert(key.clone(), leaf.clone());
            }
        }
    }
    Ok(result)
}

fn resolve_output_path<'a>(
    segments: &[&str],
    value: &'a serde_json::Value,
    current_path: &str,
    out: &mut Vec<(String, &'a serde_json::Value)>,
) {
    if segments.is_empty() {
        out.push((current_path.to_string(), value));
        return;
    }

    let segment = segments[0];
    let rest = &segments[1..];

    if segment == "*" {
        if let serde_json::Value::Array(arr) = value {
            for (i, item) in arr.iter().enumerate() {
                let indexed_path = current_path.replacen('*', &i.to_string(), 1);
                resolve_output_path(rest, item, &indexed_path, out);
            }
        }
    } else if let serde_json::Value::Object(map) = value
        && let Some(child) = map.get(segment)
    {
        resolve_output_path(rest, child, current_path, out);
    }
}

fn json_type_matches(expected: &FieldType, value: &serde_json::Value) -> bool {
    match expected {
        FieldType::String => matches!(value, serde_json::Value::String(_)),
        FieldType::Integer => {
            matches!(value, serde_json::Value::Number(n) if n.is_i64() || n.is_u64())
        }
        FieldType::Float => matches!(value, serde_json::Value::Number(_)),
        FieldType::Boolean => matches!(value, serde_json::Value::Bool(_)),
        FieldType::Array => matches!(value, serde_json::Value::Array(_)),
    }
}
