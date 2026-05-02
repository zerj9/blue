/// A parsed reference like `resources.web-01.uuid` or `data.ubuntu.endpoints[type=public].address`
#[derive(Debug, Clone, PartialEq)]
pub struct Ref {
    pub source: String,
    pub name: String,
    pub path: Vec<PathSegment>,
}

impl Ref {
    /// Returns the dependency key, e.g. `"resources.web-01"` or `"parameters.name"`
    pub fn dependency_key(&self) -> String {
        format!("{}.{}", self.source, self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathSegment {
    Field(String),
    Index(usize),
    Filter(Vec<(String, String)>),
}

/// Extract all `{{ }}` refs from a template string.
pub fn extract_refs(input: &str) -> Result<Vec<Ref>, String> {
    let mut refs = Vec::new();
    let mut rest = input;

    while let Some(start) = rest.find("{{") {
        let after_open = &rest[start + 2..];
        let end = after_open
            .find("}}")
            .ok_or_else(|| format!("Unclosed '{{{{' in template: {input}"))?;

        let expr = after_open[..end].trim();
        refs.push(parse_ref(expr)?);
        rest = &after_open[end + 2..];
    }

    Ok(refs)
}

/// Parse a single ref expression like `resources.web-01.endpoints[type=public].address`
fn parse_ref(expr: &str) -> Result<Ref, String> {
    let segments = split_ref(expr)?;

    if segments.len() < 2 {
        return Err(format!("Ref must have at least source and name: '{expr}'"));
    }

    let source = match &segments[0] {
        PathSegment::Field(s) => s.clone(),
        _ => return Err(format!("Ref source must be a field name: '{expr}'")),
    };

    if !["parameters", "data", "resources"].contains(&source.as_str()) {
        return Err(format!(
            "Ref source must be 'parameters', 'data', or 'resources', got '{source}'"
        ));
    }

    let name = match &segments[1] {
        PathSegment::Field(s) => s.clone(),
        _ => return Err(format!("Ref name must be a field name: '{expr}'")),
    };

    Ok(Ref {
        source,
        name,
        path: segments[2..].to_vec(),
    })
}

fn split_ref(expr: &str) -> Result<Vec<PathSegment>, String> {
    let mut segments = Vec::new();
    for part in expr.split('.') {
        if let Some(bracket_start) = part.find('[') {
            let field = &part[..bracket_start];
            if !field.is_empty() {
                segments.push(parse_segment(field));
            }
            let close = part
                .find(']')
                .ok_or_else(|| format!("Unclosed '[' in ref: '{expr}'"))?;
            segments.push(parse_filter(&part[bracket_start + 1..close])?);
        } else {
            segments.push(parse_segment(part));
        }
    }
    Ok(segments)
}

fn parse_segment(s: &str) -> PathSegment {
    match s.parse::<usize>() {
        Ok(n) => PathSegment::Index(n),
        Err(_) => PathSegment::Field(s.to_string()),
    }
}

use serde_json::Value;
use std::collections::HashMap;

/// Resolve all {{ }} refs in a Value tree using the output map.
pub fn resolve_value(value: &Value, outputs: &HashMap<String, Value>) -> Result<Value, String> {
    match value {
        Value::String(s) => resolve_string(s, outputs),
        Value::Object(map) => {
            let mut resolved = serde_json::Map::new();
            for (k, v) in map {
                resolved.insert(k.clone(), resolve_value(v, outputs)?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(arr) => {
            let resolved: Result<Vec<Value>, String> =
                arr.iter().map(|v| resolve_value(v, outputs)).collect();
            Ok(Value::Array(resolved?))
        }
        _ => Ok(value.clone()),
    }
}

/// Resolve a template string. If the entire string is a single ref, preserve the type.
/// If mixed with text, stringify everything.
fn resolve_string(s: &str, outputs: &HashMap<String, Value>) -> Result<Value, String> {
    let refs = extract_refs(s)?;
    if refs.is_empty() {
        return Ok(Value::String(s.to_string()));
    }

    // Single ref covering the entire string — preserve type
    let trimmed = s.trim();
    if refs.len() == 1 && trimmed.starts_with("{{") && trimmed.ends_with("}}") {
        return resolve_ref(&refs[0], outputs);
    }

    // Mixed — rebuild the string, replacing each {{ }} with resolved text
    let mut result = String::new();
    let mut rest = s;
    for r in &refs {
        let start = rest.find("{{").unwrap();
        let after_open = &rest[start + 2..];
        let end = after_open.find("}}").unwrap();

        result.push_str(&rest[..start]);
        let resolved = resolve_ref(r, outputs)?;
        match &resolved {
            Value::String(s) => result.push_str(s),
            other => result.push_str(&other.to_string()),
        }
        rest = &after_open[end + 2..];
    }
    result.push_str(rest);
    Ok(Value::String(result))
}

/// Resolve a single ref against the output map, navigating the path.
fn resolve_ref(r: &Ref, outputs: &HashMap<String, Value>) -> Result<Value, String> {
    let dep_key = r.dependency_key();
    let root = outputs
        .get(&dep_key)
        .ok_or_else(|| format!("Ref target '{dep_key}' not found in outputs"))?;

    let mut current = root;
    for segment in &r.path {
        current = match segment {
            PathSegment::Field(name) => current
                .get(name)
                .ok_or_else(|| format!("Field '{name}' not found in '{dep_key}'"))?,
            PathSegment::Index(idx) => current
                .get(idx)
                .ok_or_else(|| format!("Index {idx} out of bounds in '{dep_key}'"))?,
            PathSegment::Filter(filters) => {
                let arr = current
                    .as_array()
                    .ok_or_else(|| format!("Expected array for filter in '{dep_key}'"))?;
                let matches: Vec<&Value> = arr
                    .iter()
                    .filter(|item| {
                        filters.iter().all(|(k, v)| {
                            item.get(k).and_then(|val| val.as_str()) == Some(v.as_str())
                        })
                    })
                    .collect();
                match matches.len() {
                    0 => return Err(format!("Filter matched zero elements in '{dep_key}'")),
                    1 => matches[0],
                    n => {
                        return Err(format!(
                            "Filter matched {n} elements in '{dep_key}', expected 1"
                        ));
                    }
                }
            }
        };
    }
    Ok(current.clone())
}

fn parse_filter(s: &str) -> Result<PathSegment, String> {
    let mut filters = Vec::new();
    for pair in s.split(',') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("Invalid filter syntax: '{pair}'"))?;
        filters.push((key.trim().to_string(), value.trim().to_string()));
    }
    Ok(PathSegment::Filter(filters))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_ref() {
        let refs = extract_refs("{{ parameters.name }}").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].source, "parameters");
        assert_eq!(refs[0].name, "name");
        assert!(refs[0].path.is_empty());
    }

    #[test]
    fn ref_with_path() {
        let refs = extract_refs("{{ resources.web-01.uuid }}").unwrap();
        assert_eq!(refs[0].source, "resources");
        assert_eq!(refs[0].name, "web-01");
        assert_eq!(refs[0].path, vec![PathSegment::Field("uuid".into())]);
    }

    #[test]
    fn ref_with_index() {
        let refs = extract_refs("{{ resources.x.endpoints.0.domain_name }}").unwrap();
        assert_eq!(
            refs[0].path,
            vec![
                PathSegment::Field("endpoints".into()),
                PathSegment::Index(0),
                PathSegment::Field("domain_name".into()),
            ]
        );
    }

    #[test]
    fn ref_with_filter() {
        let refs = extract_refs("{{ resources.x.endpoints[type=public].domain_name }}").unwrap();
        assert_eq!(
            refs[0].path,
            vec![
                PathSegment::Field("endpoints".into()),
                PathSegment::Filter(vec![("type".into(), "public".into())]),
                PathSegment::Field("domain_name".into()),
            ]
        );
    }

    #[test]
    fn ref_with_multiple_filters() {
        let refs = extract_refs("{{ resources.x.endpoints[type=public,family=IPv4].domain_name }}")
            .unwrap();
        assert_eq!(
            refs[0].path,
            vec![
                PathSegment::Field("endpoints".into()),
                PathSegment::Filter(vec![
                    ("type".into(), "public".into()),
                    ("family".into(), "IPv4".into()),
                ]),
                PathSegment::Field("domain_name".into()),
            ]
        );
    }

    #[test]
    fn multiple_refs_in_string() {
        let refs = extract_refs("server-{{ parameters.name }}-{{ parameters.zone }}").unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "name");
        assert_eq!(refs[1].name, "zone");
    }

    #[test]
    fn no_refs() {
        let refs = extract_refs("just a plain string").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn dependency_key() {
        let refs = extract_refs("{{ resources.web-01.uuid }}").unwrap();
        assert_eq!(refs[0].dependency_key(), "resources.web-01");
    }

    #[test]
    fn invalid_source() {
        let result = extract_refs("{{ foo.bar }}");
        assert!(result.is_err());
    }

    #[test]
    fn unclosed_braces() {
        let result = extract_refs("{{ resources.x.uuid");
        assert!(result.is_err());
    }

    // --- Resolver tests ---

    use serde_json::json;

    fn test_outputs() -> HashMap<String, Value> {
        let mut map = HashMap::new();
        map.insert("parameters.name".into(), json!("web-01"));
        map.insert("parameters.disk_size".into(), json!(10));
        map.insert("data.ubuntu".into(), json!({"uuid": "img-123"}));
        map.insert(
            "resources.web-01".into(),
            json!({
                "uuid": "srv-456",
                "ip": "1.2.3.4",
                "endpoints": [
                    {"type": "public", "family": "IPv4", "address": "1.2.3.4"},
                    {"type": "private", "family": "IPv4", "address": "10.0.0.1"},
                ]
            }),
        );
        map
    }

    #[test]
    fn resolve_simple_string_ref() {
        let outputs = test_outputs();
        let val = json!("{{ parameters.name }}");
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!("web-01"));
    }

    #[test]
    fn resolve_preserves_type() {
        let outputs = test_outputs();
        let val = json!("{{ parameters.disk_size }}");
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!(10));
    }

    #[test]
    fn resolve_nested_path() {
        let outputs = test_outputs();
        let val = json!("{{ data.ubuntu.uuid }}");
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!("img-123"));
    }

    #[test]
    fn resolve_with_filter() {
        let outputs = test_outputs();
        let val = json!("{{ resources.web-01.endpoints[type=public].address }}");
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!("1.2.3.4"));
    }

    #[test]
    fn resolve_embedded_stringify() {
        let outputs = test_outputs();
        let val = json!("server-{{ parameters.name }}");
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!("server-web-01"));
    }

    #[test]
    fn resolve_object_recursion() {
        let outputs = test_outputs();
        let val = json!({"host": "{{ parameters.name }}", "storage": "{{ data.ubuntu.uuid }}"});
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!({"host": "web-01", "storage": "img-123"}));
    }

    #[test]
    fn resolve_no_refs_passthrough() {
        let outputs = test_outputs();
        let val = json!("plain string");
        let resolved = resolve_value(&val, &outputs).unwrap();
        assert_eq!(resolved, json!("plain string"));
    }

    #[test]
    fn resolve_missing_ref_fails() {
        let outputs = test_outputs();
        let val = json!("{{ resources.nonexistent.uuid }}");
        assert!(resolve_value(&val, &outputs).is_err());
    }
}
