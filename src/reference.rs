use std::collections::HashMap;

/// A parsed reference from a `{{...}}` template expression.
///
/// Examples:
///   "data.ubuntu.uuid"                       -> Ref { source: "data",      name: "ubuntu", path: "uuid" }
///   "resources.web-01.uuid"                  -> Ref { source: "resources", name: "web-01", path: "uuid" }
///   "data.ubuntu.hooks.outputs.name"         -> Ref { source: "data",      name: "ubuntu", path: "hooks.outputs.name" }
///   "resources.web-01.hooks.outputs.suffix"  -> Ref { source: "resources", name: "web-01", path: "hooks.outputs.suffix" }
#[derive(Debug, Clone, PartialEq)]
pub struct Ref {
    pub source: String,
    pub name: String,
    pub path: String,
}

impl Ref {
    /// Parse the trimmed content between `{{` and `}}`.
    /// Returns None if the format is invalid.
    pub fn parse(inner: &str) -> Option<Self> {
        let inner = inner.trim();
        let (source, rest) = if let Some(rest) = inner.strip_prefix("data.") {
            ("data", rest)
        } else if let Some(rest) = inner.strip_prefix("resources.") {
            ("resources", rest)
        } else {
            return None;
        };

        let (name, path) = rest.split_once('.')?;
        if name.is_empty() || path.is_empty() {
            return None;
        }

        Some(Self {
            source: source.to_string(),
            name: name.to_string(),
            path: path.to_string(),
        })
    }

    /// Scan text for all `{{...}}` references and parse each one.
    pub fn parse_all(text: &str) -> Vec<Self> {
        let mut refs = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            let after_open = &rest[start + 2..];
            if let Some(end) = after_open.find("}}") {
                let inner = after_open[..end].trim();
                if let Some(r) = Self::parse(inner) {
                    refs.push(r);
                }
                rest = &after_open[end + 2..];
            } else {
                break;
            }
        }
        refs
    }

    /// The node name this ref depends on (for graph edge building).
    pub fn target(&self) -> &str {
        &self.name
    }

    /// Whether this ref points to a hook output.
    pub fn is_hook_output(&self) -> bool {
        self.path.starts_with("hooks.outputs.")
    }

    /// The hook output field name, if this is a hook output ref.
    pub fn hook_output_name(&self) -> Option<&str> {
        self.path.strip_prefix("hooks.outputs.")
    }

    /// Look up this ref's value in the output registry.
    pub fn resolve(&self, registry: &OutputRegistry) -> Result<String, String> {
        let value = registry
            .get(&self.source, &self.name, &self.path)
            .ok_or_else(|| {
                format!(
                    "unresolved reference: {}.{}.{}",
                    self.source, self.name, self.path
                )
            })?;
        Ok(json_value_to_string(value))
    }

    /// Resolve all `{{...}}` references in text using the output registry.
    /// Non-ref placeholders (e.g. parameters) are left as-is.
    pub fn resolve_all(text: &str, registry: &OutputRegistry) -> Result<String, String> {
        let mut result = String::with_capacity(text.len());
        let mut rest = text;

        while let Some(start) = rest.find("{{") {
            result.push_str(&rest[..start]);
            let after_open = &rest[start + 2..];
            let end = after_open
                .find("}}")
                .ok_or_else(|| format!("unclosed '{{{{' at byte {start}"))?;
            let inner = after_open[..end].trim();

            if let Some(r) = Self::parse(inner) {
                let value = r.resolve(registry)?;
                result.push_str(&value);
            } else {
                // Not a data/resource ref — leave as-is
                result.push_str(&rest[start..start + 2 + end + 2]);
            }

            rest = &after_open[end + 2..];
        }
        result.push_str(rest);
        Ok(result)
    }
}

/// Accumulates outputs from data sources, hooks, and resources during graph traversal.
#[derive(Debug, Default)]
pub struct OutputRegistry {
    /// "data.ubuntu" -> { "uuid": "...", "hooks.outputs.name": "..." }
    /// "resources.web-01" -> { "uuid": "...", "hooks.outputs.suffix": "..." }
    entries: HashMap<String, HashMap<String, serde_json::Value>>,
}

impl OutputRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a single output value.
    pub fn insert(&mut self, source: &str, name: &str, path: &str, value: serde_json::Value) {
        let key = format!("{source}.{name}");
        self.entries
            .entry(key)
            .or_default()
            .insert(path.to_string(), value);
    }

    /// Bulk insert from a JSON object (e.g. provider outputs or extracted schema outputs).
    pub fn insert_outputs(
        &mut self,
        source: &str,
        name: &str,
        outputs: &HashMap<String, serde_json::Value>,
    ) {
        for (path, value) in outputs {
            self.insert(source, name, path, value.clone());
        }
    }

    /// Look up a value by source, name, and path.
    pub fn get(&self, source: &str, name: &str, path: &str) -> Option<&serde_json::Value> {
        let key = format!("{source}.{name}");
        self.entries.get(&key)?.get(path)
    }

    /// Convert to a flat data vars map for backward compatibility with snapshot functions.
    /// Returns keys like "data.ubuntu.uuid" -> Value.
    pub fn to_data_vars(&self) -> HashMap<String, serde_json::Value> {
        let mut vars = HashMap::new();
        for (key, outputs) in &self.entries {
            if let Some(rest) = key.strip_prefix("data.") {
                for (path, value) in outputs {
                    if !path.starts_with("hooks.outputs.") {
                        vars.insert(format!("data.{rest}.{path}"), value.clone());
                    }
                }
            }
        }
        vars
    }
}

fn json_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_data_ref() {
        let r = Ref::parse("data.ubuntu.uuid").unwrap();
        assert_eq!(r.source, "data");
        assert_eq!(r.name, "ubuntu");
        assert_eq!(r.path, "uuid");
        assert!(!r.is_hook_output());
        assert_eq!(r.target(), "ubuntu");
    }

    #[test]
    fn parse_resource_ref() {
        let r = Ref::parse("resources.web-01.uuid").unwrap();
        assert_eq!(r.source, "resources");
        assert_eq!(r.name, "web-01");
        assert_eq!(r.path, "uuid");
    }

    #[test]
    fn parse_hook_output_ref() {
        let r = Ref::parse("data.ubuntu.hooks.outputs.name").unwrap();
        assert_eq!(r.source, "data");
        assert_eq!(r.name, "ubuntu");
        assert_eq!(r.path, "hooks.outputs.name");
        assert!(r.is_hook_output());
        assert_eq!(r.hook_output_name(), Some("name"));
    }

    #[test]
    fn parse_invalid() {
        assert!(Ref::parse("just_a_param").is_none());
        assert!(Ref::parse("data.").is_none());
        assert!(Ref::parse("data.ubuntu").is_none());
        assert!(Ref::parse("").is_none());
    }

    #[test]
    fn parse_all_finds_refs() {
        let text = r#"hostname = "{{ data.ubuntu.uuid }}" storage = "{{ resources.web-01.id }}""#;
        let refs = Ref::parse_all(text);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "ubuntu");
        assert_eq!(refs[1].name, "web-01");
    }

    #[test]
    fn resolve_from_registry() {
        let mut reg = OutputRegistry::new();
        reg.insert("data", "ubuntu", "uuid", serde_json::json!("abc-123"));

        let r = Ref::parse("data.ubuntu.uuid").unwrap();
        assert_eq!(r.resolve(&reg).unwrap(), "abc-123");
    }

    #[test]
    fn resolve_all_in_text() {
        let mut reg = OutputRegistry::new();
        reg.insert("data", "ubuntu", "uuid", serde_json::json!("abc-123"));

        let text = r#"storage = "{{ data.ubuntu.uuid }}""#;
        let resolved = Ref::resolve_all(text, &reg).unwrap();
        assert_eq!(resolved, r#"storage = "abc-123""#);
    }

    #[test]
    fn resolve_all_leaves_params() {
        let reg = OutputRegistry::new();
        let text = r#"size = "{{ disk_size }}""#;
        let resolved = Ref::resolve_all(text, &reg).unwrap();
        assert_eq!(resolved, text);
    }

    #[test]
    fn to_data_vars_excludes_hook_outputs() {
        let mut reg = OutputRegistry::new();
        reg.insert("data", "ubuntu", "uuid", serde_json::json!("abc"));
        reg.insert(
            "data",
            "ubuntu",
            "hooks.outputs.name",
            serde_json::json!("dev"),
        );
        reg.insert("resources", "web", "id", serde_json::json!("r1"));

        let vars = reg.to_data_vars();
        assert_eq!(vars.len(), 1);
        assert!(vars.contains_key("data.ubuntu.uuid"));
        assert!(!vars.contains_key("data.ubuntu.hooks.outputs.name"));
        assert!(!vars.contains_key("resources.web.id"));
    }
}
