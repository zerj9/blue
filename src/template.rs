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

}
