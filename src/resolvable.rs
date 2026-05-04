use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use crate::template::{PathSegment, Ref, extract_refs};
use crate::types::{FieldDef, FieldType};

/// A value that may not yet be fully resolved.
///
/// `Resolvable` is the plan-time intermediate representation between the
/// lenient resolver (which converts a `Value` tree containing `{{ }}`
/// templates into a `Resolvable`, leaving refs to not-yet-deployed resources
/// as `Unknown` leaves) and the deploy-time strict finalisation
/// (`into_concrete`, which converts back to `Value` once all refs have
/// resolved).
///
/// Invariant maintained by the resolver:
/// - `Known(Value)` represents a fully-concrete subtree. Concrete objects
///   and arrays live here, not under the `Object`/`Array` variants.
/// - `Object` and `Array` are used only when the subtree contains at least
///   one `Unknown` somewhere. This makes the common all-concrete case a
///   single move (`Known(value)`) rather than a recursive structure.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolvable {
    /// A fully-concrete subtree. Contains no `Unknown` leaves anywhere.
    Known(Value),

    /// A pending value: one or more template refs are known, the resolved
    /// value is not. Covers both the single-complete-ref case
    /// (`"{{ resources.x.uuid }}"`) and the mixed-text interpolation case
    /// (`"prefix-{{ resources.x.id }}-suffix"`). The deploy-time resolver
    /// distinguishes between them by examining `raw` and `refs`:
    /// - `refs.len() == 1` AND `raw.trim()` is exactly the single `{{ ... }}`
    ///   expression → resolve preserving the ref's source type (number stays
    ///   number, etc.).
    /// - Otherwise → string interpolation, result is always `Value::String`.
    Unknown {
        /// The original template string. Preserved verbatim so the
        /// deploy-time resolver can re-process it once `outputs` is richer.
        raw: String,
        /// Refs parsed out of `raw`. `len() >= 1`. For a single-complete-ref
        /// leaf this is one entry; for an interpolated string it's one per
        /// `{{ ... }}` expression in `raw`.
        refs: Vec<Ref>,
        /// The destination field's declared type from the consuming
        /// resource's schema, when the schema constrains it.
        ///
        /// `None` means the destination is a permissive context — e.g. an
        /// untyped object like `blue.script`'s `inputs` field, or an array
        /// declared without `items`. The `Option` here is *not* a deferral
        /// placeholder; it encodes the genuine bimodal semantic of "schema
        /// constrains this leaf" vs "schema is permissive at this point."
        expected_type: Option<FieldType>,
    },

    /// An object that contains at least one `Unknown` somewhere in its
    /// subtree. Fully-concrete objects are stored as
    /// `Known(Value::Object(..))`, not here.
    ///
    /// Uses `std::collections::BTreeMap` rather than `serde_json::Map`
    /// because `serde_json::Map`'s trait derives (`Debug`, `Clone`,
    /// `PartialEq`) are only implemented for `Map<String, Value>`, not for
    /// the generic value type. Iteration order matches `serde_json::Map`'s
    /// default backing (alphabetical).
    Object(BTreeMap<String, Resolvable>),

    /// An array that contains at least one `Unknown` somewhere in its
    /// subtree. Fully-concrete arrays are stored as
    /// `Known(Value::Array(..))`, not here.
    Array(Vec<Resolvable>),
}

/// One pending leaf, paired with its location in the containing tree.
/// Returned by `Resolvable::pending_refs` for plan-output rendering and
/// for diagnostics when strict finalisation finds a leftover `Unknown`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRef<'a> {
    /// Slash-separated path to this leaf, e.g. `"/networks/0/uuid"`.
    /// Empty string for a top-level `Unknown`.
    pub path: String,
    /// The source template ref that produced this `Unknown`.
    pub source: &'a Ref,
    /// The destination field's expected type, when constrained.
    pub expected_type: Option<&'a FieldType>,
}

impl Resolvable {
    /// Wrap a fully-concrete JSON value. The caller asserts that no template
    /// strings remain in the subtree — the lenient resolver only constructs
    /// `Known` when every nested ref has been resolved.
    pub fn known(value: Value) -> Self {
        Resolvable::Known(value)
    }

    /// Construct an `Unknown` leaf from the original template string, the
    /// refs parsed out of it, and the destination field's expected type
    /// (`None` for permissive contexts). `refs` must be non-empty —
    /// constructing an `Unknown` with no refs is meaningless.
    pub fn unknown(raw: String, refs: Vec<Ref>, expected_type: Option<FieldType>) -> Self {
        debug_assert!(!refs.is_empty(), "Unknown must carry at least one ref");
        Resolvable::Unknown {
            raw,
            refs,
            expected_type,
        }
    }

    /// Borrow the concrete `Value` if this subtree is fully resolved (i.e.
    /// the variant is `Known`). `None` if the subtree contains any pending
    /// state — `Unknown` leaves or compositional `Object`/`Array` variants.
    ///
    /// The intended use at plan-time provider hooks (`validate`,
    /// `customize_diff`) is the early-return pattern:
    ///
    /// ```ignore
    /// fn customize_diff(&self, diff: &mut Diff, inputs: &Resolvable, outputs: &Value)
    ///     -> Result<(), String>
    /// {
    ///     let Some(inputs) = inputs.as_concrete() else { return Ok(()) };
    ///     // ...existing logic, sees a concrete &Value
    /// }
    /// ```
    ///
    /// Providers that want to act on partial inputs can pattern-match on
    /// the variants directly and use `pending_refs()` for diagnostics.
    pub fn as_concrete(&self) -> Option<&Value> {
        match self {
            Resolvable::Known(v) => Some(v),
            _ => None,
        }
    }

    /// True iff this subtree contains no `Unknown` leaves anywhere.
    ///
    /// By the `Known`/compositional invariant, a fully-concrete subtree is
    /// always represented as `Known`, so this could be a one-line
    /// `matches!(self, Known(_))`. The recursive form is kept defensively in
    /// case the invariant is ever violated by a buggy producer — better to
    /// return the right answer than to lie because the shape was wrong.
    pub fn is_concrete(&self) -> bool {
        match self {
            Resolvable::Known(_) => true,
            Resolvable::Unknown { .. } => false,
            Resolvable::Object(map) => map.values().all(|v| v.is_concrete()),
            Resolvable::Array(arr) => arr.iter().all(|v| v.is_concrete()),
        }
    }

    /// Collect all `Unknown` leaves in this subtree, paired with their
    /// slash-separated paths.
    pub fn pending_refs(&self) -> Vec<PendingRef<'_>> {
        let mut acc = Vec::new();
        self.collect_pending("", &mut acc);
        acc
    }

    fn collect_pending<'a>(&'a self, path: &str, acc: &mut Vec<PendingRef<'a>>) {
        match self {
            Resolvable::Known(_) => {}
            Resolvable::Unknown {
                refs,
                expected_type,
                ..
            } => {
                // One PendingRef per ref. For a single-ref Unknown that's
                // one entry; for an interpolated string ("a-{{x}}-{{y}}-b")
                // it's one entry per `{{ ... }}` — all sharing the same
                // path (the leaf's path in the value tree). Consumers that
                // want one logical entry per leaf can dedupe by path.
                for source in refs {
                    acc.push(PendingRef {
                        path: path.to_string(),
                        source,
                        expected_type: expected_type.as_ref(),
                    });
                }
            }
            Resolvable::Object(map) => {
                for (k, v) in map {
                    v.collect_pending(&format!("{path}/{k}"), acc);
                }
            }
            Resolvable::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    v.collect_pending(&format!("{path}/{i}"), acc);
                }
            }
        }
    }

    /// Convert to a concrete `Value`. Errors with every leftover `Unknown`
    /// (its source ref + tree path) so the caller can format a single
    /// message that lists all of them rather than failing on the first.
    ///
    /// In normal use, deploy-time strict resolution should leave no
    /// `Unknown`s — every ref should have resolved against the accumulated
    /// output map. Anything returned in the `Err` here indicates a real bug
    /// (graph order, output map seeding, or resolver logic).
    pub fn into_concrete(self) -> Result<Value, Vec<(String, Ref)>> {
        let mut errors = Vec::new();
        let value = self.into_concrete_inner("", &mut errors);
        if errors.is_empty() {
            Ok(value)
        } else {
            Err(errors)
        }
    }

    /// Walks the whole tree even on error so all leftover `Unknown`s are
    /// collected into one error list. The partial `Value` returned along
    /// the way (with `Null` placeholders where `Unknown`s were) is only
    /// used when there are no errors — `into_concrete` discards it
    /// otherwise.
    fn into_concrete_inner(self, path: &str, errors: &mut Vec<(String, Ref)>) -> Value {
        match self {
            Resolvable::Known(v) => v,
            Resolvable::Unknown { refs, .. } => {
                // One error per ref so the caller's diagnostic lists every
                // unresolved upstream — same path repeated for an
                // interpolated string with multiple refs.
                for source in refs {
                    errors.push((path.to_string(), source));
                }
                Value::Null
            }
            Resolvable::Object(map) => {
                let mut result = Map::new();
                for (k, v) in map {
                    let child_path = format!("{path}/{k}");
                    let child_value = v.into_concrete_inner(&child_path, errors);
                    result.insert(k, child_value);
                }
                Value::Object(result)
            }
            Resolvable::Array(arr) => {
                let mut result = Vec::with_capacity(arr.len());
                for (i, v) in arr.into_iter().enumerate() {
                    let child_path = format!("{path}/{i}");
                    result.push(v.into_concrete_inner(&child_path, errors));
                }
                Value::Array(result)
            }
        }
    }
}

impl Resolvable {
    /// Re-resolve `Unknown` leaves against an updated `outputs` map. Used
    /// at deploy time after each completed step adds its outputs to the
    /// working output map: pending refs that now have a value become
    /// `Known`, refs still missing stay `Unknown`. The compositional
    /// invariant is maintained — fully-Known children re-collapse to a
    /// single `Known` at each level.
    ///
    /// Path-navigation errors during resolution (e.g. ref to a field
    /// that doesn't exist on the upstream's outputs) bubble up as `Err`.
    /// `expected_type` travels through unchanged on any leftover `Unknown`.
    pub fn finalize(self, outputs: &HashMap<String, Value>) -> Result<Self, String> {
        match self {
            Resolvable::Known(_) => Ok(self),
            Resolvable::Unknown {
                raw,
                expected_type,
                ..
            } => resolve_string(&raw, expected_type, outputs),
            Resolvable::Object(map) => {
                let mut out = BTreeMap::new();
                for (k, v) in map {
                    out.insert(k, v.finalize(outputs)?);
                }
                Ok(make_object(out))
            }
            Resolvable::Array(arr) => {
                let mut out = Vec::with_capacity(arr.len());
                for v in arr {
                    out.push(v.finalize(outputs)?);
                }
                Ok(make_array(out))
            }
        }
    }
}

impl From<Value> for Resolvable {
    fn from(v: Value) -> Self {
        Resolvable::Known(v)
    }
}

// === Schema-aware lenient resolver ===
//
// The resolver walks a `Value` tree representing a resource's inputs in
// lockstep with that resource's schema. At each leaf string containing
// `{{ ... }}` refs, it resolves what it can against `outputs` and produces
// a `Resolvable::Unknown` for anything that isn't yet available — carrying
// the destination field's expected type from the schema for use by
// plan-time type checking and plan-output rendering.
//
// Permissive contexts (objects with no declared `fields`, arrays with no
// declared `items`) recurse via `resolve_no_schema`, which produces
// `Unknown` leaves with `expected_type: None`.

/// Walk a resource's input value against its schema, resolving every
/// `{{ ... }}` ref against `outputs`. Refs whose dependency key isn't in
/// `outputs` produce `Resolvable::Unknown` leaves carrying the destination
/// field's expected type (or `None` for permissive contexts).
///
/// Path-navigation errors (e.g. ref to a non-existent field of a
/// dependency that *is* in `outputs`) are real errors and bubble up as
/// `Err` — they aren't deferred to deploy time.
pub fn resolve_inputs(
    inputs: &Value,
    schema_fields: &[FieldDef],
    outputs: &HashMap<String, Value>,
) -> Result<Resolvable, String> {
    let obj = inputs
        .as_object()
        .ok_or_else(|| "resolve_inputs: expected top-level Value::Object".to_string())?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        let resolved = match schema_fields.iter().find(|f| f.path == *k) {
            Some(field) => resolve_field(v, field, outputs)?,
            // Unknown top-level field — fall back to permissive. The schema
            // validator will flag the typo separately.
            None => resolve_no_schema(v, outputs)?,
        };
        out.insert(k.clone(), resolved);
    }
    Ok(make_object(out))
}

/// Resolve a value occupying a known `FieldDef` slot. The field's
/// `field_type` becomes the `expected_type` for any pending refs at leaves
/// directly under it.
fn resolve_field(
    value: &Value,
    field: &FieldDef,
    outputs: &HashMap<String, Value>,
) -> Result<Resolvable, String> {
    match value {
        Value::String(s) => resolve_string(s, Some(field.field_type.clone()), outputs),
        Value::Object(obj) => {
            if field.fields.is_empty() {
                // Permissive object — no FieldDefs for nested keys.
                let mut out = BTreeMap::new();
                for (k, v) in obj {
                    out.insert(k.clone(), resolve_no_schema(v, outputs)?);
                }
                Ok(make_object(out))
            } else {
                resolve_object_with_fields(value, &field.fields, outputs)
            }
        }
        Value::Array(arr) => {
            if field.items.is_empty() {
                // Permissive array.
                let mut out = Vec::with_capacity(arr.len());
                for v in arr {
                    out.push(resolve_no_schema(v, outputs)?);
                }
                Ok(make_array(out))
            } else if field.items.len() == 1 && field.items[0].path.is_empty() {
                // Typed primitive array — single item def with empty path.
                // Each element uses that single FieldDef's type.
                let item_def = &field.items[0];
                let mut out = Vec::with_capacity(arr.len());
                for v in arr {
                    out.push(resolve_field(v, item_def, outputs)?);
                }
                Ok(make_array(out))
            } else {
                // Typed object array — each element is an object whose keys
                // are constrained by `field.items`.
                let mut out = Vec::with_capacity(arr.len());
                for v in arr {
                    out.push(resolve_object_with_fields(v, &field.items, outputs)?);
                }
                Ok(make_array(out))
            }
        }
        _ => Ok(Resolvable::Known(value.clone())),
    }
}

/// Walk an object value against a slice of `FieldDef`s. Keys present in
/// the value but not declared in `fields` fall back to permissive
/// resolution (the schema validator will flag the typo separately).
/// Non-object values are passed through as `Known` — type mismatches are
/// the validator's concern.
fn resolve_object_with_fields(
    value: &Value,
    fields: &[FieldDef],
    outputs: &HashMap<String, Value>,
) -> Result<Resolvable, String> {
    match value.as_object() {
        Some(obj) => {
            let mut out = BTreeMap::new();
            for (k, v) in obj {
                let resolved = match fields.iter().find(|f| f.path == *k) {
                    Some(child_field) => resolve_field(v, child_field, outputs)?,
                    None => resolve_no_schema(v, outputs)?,
                };
                out.insert(k.clone(), resolved);
            }
            Ok(make_object(out))
        }
        None => Ok(Resolvable::Known(value.clone())),
    }
}

/// Schema-less recursive resolve — used for permissive contexts and for
/// any subtree where we've fallen out of the typed schema (unknown keys,
/// type mismatches). Every pending leaf produced here carries
/// `expected_type: None`.
fn resolve_no_schema(
    value: &Value,
    outputs: &HashMap<String, Value>,
) -> Result<Resolvable, String> {
    match value {
        Value::String(s) => resolve_string(s, None, outputs),
        Value::Object(obj) => {
            let mut out = BTreeMap::new();
            for (k, v) in obj {
                out.insert(k.clone(), resolve_no_schema(v, outputs)?);
            }
            Ok(make_object(out))
        }
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(resolve_no_schema(v, outputs)?);
            }
            Ok(make_array(out))
        }
        _ => Ok(Resolvable::Known(value.clone())),
    }
}

/// Resolve a string that may contain `{{ ... }}` refs.
///
/// - No refs → `Known(Value::String(s))`.
/// - All refs resolvable, single complete ref (entire trimmed string is
///   one `{{ ... }}`) → `Known(value)` preserving the ref's source type
///   (number stays number, object stays object, etc.).
/// - All refs resolvable, mixed text → `Known(Value::String(interpolated))`.
/// - Any ref unresolvable → `Unknown { raw: original, refs: only the
///   unresolvable ones, expected_type }`. Resolvable refs in a mixed
///   string aren't listed in `refs` because they aren't pending.
fn resolve_string(
    s: &str,
    expected_type: Option<FieldType>,
    outputs: &HashMap<String, Value>,
) -> Result<Resolvable, String> {
    let refs = extract_refs(s)?;
    if refs.is_empty() {
        return Ok(Resolvable::Known(Value::String(s.to_string())));
    }

    let unresolvable: Vec<Ref> = refs
        .iter()
        .filter(|r| !outputs.contains_key(&r.dependency_key()))
        .cloned()
        .collect();

    if !unresolvable.is_empty() {
        return Ok(Resolvable::unknown(
            s.to_string(),
            unresolvable,
            expected_type,
        ));
    }

    // All refs resolvable. Single complete ref → preserve type.
    let trimmed = s.trim();
    if refs.len() == 1 && trimmed.starts_with("{{") && trimmed.ends_with("}}") {
        return resolve_ref(&refs[0], outputs).map(Resolvable::Known);
    }

    // Mixed text — interpolate every ref into a string result.
    let mut result = String::new();
    let mut rest = s;
    for r in &refs {
        let start = rest.find("{{").unwrap();
        let after_open = &rest[start + 2..];
        let end = after_open.find("}}").unwrap();

        result.push_str(&rest[..start]);
        let resolved = resolve_ref(r, outputs)?;
        match &resolved {
            Value::String(rs) => result.push_str(rs),
            other => result.push_str(&other.to_string()),
        }
        rest = &after_open[end + 2..];
    }
    result.push_str(rest);
    Ok(Resolvable::Known(Value::String(result)))
}

/// Navigate a parsed ref against `outputs`. Caller is responsible for
/// pre-checking that `outputs` contains the ref's dep_key — this function
/// errors if the dep_key is missing OR if path navigation fails (typo,
/// missing field, out-of-bounds index, filter mismatch).
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

/// Build a `Resolvable::Object`, collapsing to `Known(Value::Object(..))`
/// if every direct child is already `Known`. This maintains the invariant
/// that `Object` and `Array` variants exist only when at least one
/// descendant is `Unknown` — keeping fully-concrete subtrees represented
/// as a single `Known(value)`.
fn make_object(map: BTreeMap<String, Resolvable>) -> Resolvable {
    if map.values().all(|r| matches!(r, Resolvable::Known(_))) {
        let json_map: Map<String, Value> = map
            .into_iter()
            .map(|(k, v)| match v {
                Resolvable::Known(val) => (k, val),
                _ => unreachable!("checked all children are Known above"),
            })
            .collect();
        Resolvable::Known(Value::Object(json_map))
    } else {
        Resolvable::Object(map)
    }
}

fn make_array(arr: Vec<Resolvable>) -> Resolvable {
    if arr.iter().all(|r| matches!(r, Resolvable::Known(_))) {
        let json_arr: Vec<Value> = arr
            .into_iter()
            .map(|r| match r {
                Resolvable::Known(val) => val,
                _ => unreachable!("checked all children are Known above"),
            })
            .collect();
        Resolvable::Known(Value::Array(json_arr))
    } else {
        Resolvable::Array(arr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::extract_refs;
    use serde_json::json;

    /// Helper: parse the first template ref out of a string. Tests below
    /// don't need to construct `Ref` by hand — extract_refs gives us the
    /// real parser's output, so the constructed `Unknown` matches what the
    /// lenient resolver will eventually produce.
    fn ref_from(template: &str) -> Ref {
        extract_refs(template).unwrap().into_iter().next().unwrap()
    }

    /// Helper: build an `Unknown` from a template string. Extracts every
    /// `{{ ... }}` ref out of the string and stores the original verbatim
    /// as `raw`. Mirrors what the lenient resolver will produce for any
    /// input string.
    fn pending(template: &str, expected_type: Option<FieldType>) -> Resolvable {
        Resolvable::unknown(
            template.to_string(),
            extract_refs(template).unwrap(),
            expected_type,
        )
    }

    #[test]
    fn known_is_concrete() {
        assert!(Resolvable::known(json!("x")).is_concrete());
        assert!(Resolvable::known(json!(42)).is_concrete());
        assert!(Resolvable::known(json!({"k": "v"})).is_concrete());
        assert!(Resolvable::known(json!([1, 2, 3])).is_concrete());
    }

    #[test]
    fn unknown_is_not_concrete() {
        let r = pending("{{ resources.x.uuid }}", Some(FieldType::String));
        assert!(!r.is_concrete());
    }

    #[test]
    fn object_with_unknown_descendant_is_not_concrete() {
        let mut map = BTreeMap::new();
        map.insert("ok".into(), Resolvable::known(json!("a")));
        map.insert(
            "pending".into(),
            pending("{{ resources.x.uuid }}", None),
        );
        let r = Resolvable::Object(map);
        assert!(!r.is_concrete());
    }

    #[test]
    fn array_with_unknown_descendant_is_not_concrete() {
        let r = Resolvable::Array(vec![
            Resolvable::known(json!("a")),
            pending("{{ resources.x.uuid }}", None),
        ]);
        assert!(!r.is_concrete());
    }

    #[test]
    fn pending_refs_empty_for_concrete() {
        let r = Resolvable::known(json!({"a": [1, 2], "b": "x"}));
        assert!(r.pending_refs().is_empty());
    }

    #[test]
    fn pending_refs_top_level_unknown_has_empty_path() {
        let src = ref_from("{{ resources.x.uuid }}");
        let r = pending("{{ resources.x.uuid }}", Some(FieldType::String));
        let pending = r.pending_refs();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "");
        assert_eq!(pending[0].source, &src);
        assert_eq!(pending[0].expected_type, Some(&FieldType::String));
    }

    #[test]
    fn pending_refs_records_paths_for_nested_unknowns() {
        // { "name": "ok", "networks": [ {"uuid": <pending>}, {"uuid": "ok"} ] }
        let pending_src = ref_from("{{ resources.svc.uuid }}");
        let mut net0 = BTreeMap::new();
        net0.insert(
            "uuid".into(),
            pending("{{ resources.svc.uuid }}", Some(FieldType::String)),
        );
        let net1 = Resolvable::known(json!({"uuid": "ok"}));

        let mut root = BTreeMap::new();
        root.insert("name".into(), Resolvable::known(json!("ok")));
        root.insert(
            "networks".into(),
            Resolvable::Array(vec![Resolvable::Object(net0), net1]),
        );
        let r = Resolvable::Object(root);

        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].path, "/networks/0/uuid");
        assert_eq!(pending_list[0].source, &pending_src);
    }

    #[test]
    fn pending_refs_returns_one_per_ref_for_interpolated_string() {
        // The mixed-text case: "prefix-{{ a.x }}-mid-{{ b.y }}-suffix" has
        // two refs in one Unknown. Both should appear in pending_refs,
        // sharing the same path.
        let src_a = ref_from("{{ resources.a.x }}");
        let src_b = ref_from("{{ resources.b.y }}");
        let r = pending(
            "prefix-{{ resources.a.x }}-mid-{{ resources.b.y }}-suffix",
            Some(FieldType::String),
        );
        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 2);
        assert_eq!(pending_list[0].path, "");
        assert_eq!(pending_list[0].source, &src_a);
        assert_eq!(pending_list[1].path, "");
        assert_eq!(pending_list[1].source, &src_b);
    }

    #[test]
    fn into_concrete_returns_value_when_fully_resolved() {
        let r = Resolvable::known(json!({"a": [1, 2], "b": "x"}));
        let v = r.into_concrete().unwrap();
        assert_eq!(v, json!({"a": [1, 2], "b": "x"}));
    }

    #[test]
    fn into_concrete_walks_compositional_variants() {
        // A tree built from compositional Object/Array variants but with
        // every leaf Known should still convert successfully — defensive
        // against an upstream that didn't normalize to `Known(Value::..)`.
        let mut inner = BTreeMap::new();
        inner.insert("x".into(), Resolvable::known(json!(1)));
        inner.insert("y".into(), Resolvable::known(json!("two")));
        let r = Resolvable::Array(vec![Resolvable::Object(inner)]);
        let v = r.into_concrete().unwrap();
        assert_eq!(v, json!([{"x": 1, "y": "two"}]));
    }

    #[test]
    fn into_concrete_collects_all_unknowns_with_paths() {
        let src1 = ref_from("{{ resources.a.uuid }}");
        let src2 = ref_from("{{ resources.b.uuid }}");

        let mut root = BTreeMap::new();
        root.insert(
            "first".into(),
            pending("{{ resources.a.uuid }}", Some(FieldType::String)),
        );
        root.insert(
            "list".into(),
            Resolvable::Array(vec![
                Resolvable::known(json!("ok")),
                pending("{{ resources.b.uuid }}", None),
            ]),
        );
        let r = Resolvable::Object(root);

        let err = r.into_concrete().unwrap_err();
        assert_eq!(err.len(), 2);
        // BTreeMap ordering means "first" comes before "list" alphabetically.
        assert_eq!(err[0], ("/first".to_string(), src1));
        assert_eq!(err[1], ("/list/1".to_string(), src2));
    }

    #[test]
    fn into_concrete_pushes_one_error_per_ref_in_interpolated_string() {
        // Interpolated string with two refs at one leaf produces two error
        // entries, both pointing at the same path.
        let src_a = ref_from("{{ resources.a.x }}");
        let src_b = ref_from("{{ resources.b.y }}");
        let mut root = BTreeMap::new();
        root.insert(
            "name".into(),
            pending(
                "prefix-{{ resources.a.x }}-mid-{{ resources.b.y }}-suffix",
                Some(FieldType::String),
            ),
        );
        let r = Resolvable::Object(root);

        let err = r.into_concrete().unwrap_err();
        assert_eq!(err.len(), 2);
        assert_eq!(err[0], ("/name".to_string(), src_a));
        assert_eq!(err[1], ("/name".to_string(), src_b));
    }

    #[test]
    fn from_value_wraps_as_known() {
        let v: Value = json!({"k": "v"});
        let r: Resolvable = v.clone().into();
        assert_eq!(r, Resolvable::Known(v));
    }

    // === Resolver tests ===

    fn schema_for(toml_str: &str) -> Vec<FieldDef> {
        crate::schema::parse_schema(toml_str).unwrap().inputs
    }

    fn outputs_with(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn resolver_passes_through_all_concrete_inputs() {
        let schema = schema_for(
            r#"
[inputs.name]
type = "string"
required = true

[inputs.size]
type = "number"
"#,
        );
        let inputs = json!({"name": "data-disk", "size": 100});
        let r = resolve_inputs(&inputs, &schema, &HashMap::new()).unwrap();
        // No refs anywhere → fully concrete, collapsed to Known.
        assert_eq!(
            r,
            Resolvable::Known(json!({"name": "data-disk", "size": 100}))
        );
    }

    #[test]
    fn resolver_resolves_ref_against_outputs() {
        let schema = schema_for(
            r#"
[inputs.zone]
type = "string"
required = true
"#,
        );
        let outputs = outputs_with(&[("parameters.region", json!("uk-lon1"))]);
        let inputs = json!({"zone": "{{ parameters.region }}"});
        let r = resolve_inputs(&inputs, &schema, &outputs).unwrap();
        assert_eq!(r, Resolvable::Known(json!({"zone": "uk-lon1"})));
    }

    #[test]
    fn resolver_preserves_type_for_single_complete_ref() {
        let schema = schema_for(
            r#"
[inputs.size]
type = "number"
"#,
        );
        let outputs = outputs_with(&[("parameters.disk_size", json!(100))]);
        let inputs = json!({"size": "{{ parameters.disk_size }}"});
        let r = resolve_inputs(&inputs, &schema, &outputs).unwrap();
        // Single complete ref → number stays a number, not stringified.
        assert_eq!(r, Resolvable::Known(json!({"size": 100})));
    }

    #[test]
    fn resolver_interpolates_mixed_string_when_all_known() {
        let schema = schema_for(
            r#"
[inputs.hostname]
type = "string"
"#,
        );
        let outputs = outputs_with(&[("parameters.name", json!("web-01"))]);
        let inputs = json!({"hostname": "server-{{ parameters.name }}"});
        let r = resolve_inputs(&inputs, &schema, &outputs).unwrap();
        assert_eq!(r, Resolvable::Known(json!({"hostname": "server-web-01"})));
    }

    #[test]
    fn resolver_produces_unknown_with_expected_type_for_pending_ref() {
        let schema = schema_for(
            r#"
[inputs.service_uuid]
type = "string"
required = true
"#,
        );
        let inputs = json!({"service_uuid": "{{ resources.svc.uuid }}"});
        let r = resolve_inputs(&inputs, &schema, &HashMap::new()).unwrap();

        // Should be Object with one Unknown leaf carrying expected_type=String.
        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].path, "/service_uuid");
        assert_eq!(pending_list[0].expected_type, Some(&FieldType::String));
    }

    #[test]
    fn resolver_partitions_refs_in_mixed_string() {
        // "prefix-{{ known }}-{{ pending }}" — only the pending ref ends
        // up in Unknown.refs; the known one is not listed because it isn't
        // pending.
        let schema = schema_for(
            r#"
[inputs.bucket_name]
type = "string"
"#,
        );
        let outputs = outputs_with(&[("parameters.env", json!("prod"))]);
        let inputs = json!({
            "bucket_name": "{{ parameters.env }}-{{ resources.acct.id }}-bucket"
        });
        let r = resolve_inputs(&inputs, &schema, &outputs).unwrap();

        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].path, "/bucket_name");
        assert_eq!(pending_list[0].source.dependency_key(), "resources.acct");
        assert_eq!(pending_list[0].expected_type, Some(&FieldType::String));
    }

    #[test]
    fn resolver_uses_none_for_permissive_object_context() {
        // blue.script-style: inputs is a permissive object with no
        // declared nested fields. Refs inside should get expected_type=None.
        let schema = schema_for(
            r#"
[inputs.inputs]
type = "object"
"#,
        );
        let inputs = json!({"inputs": {"ubuntu_uuid": "{{ resources.x.uuid }}"}});
        let r = resolve_inputs(&inputs, &schema, &HashMap::new()).unwrap();

        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].path, "/inputs/ubuntu_uuid");
        assert_eq!(pending_list[0].expected_type, None);
    }

    #[test]
    fn resolver_uses_item_type_for_typed_primitive_array() {
        // tags = ["a", "{{ resources.x.label }}"] where items declare
        // type=string. Pending leaf should carry expected_type=String.
        let schema = schema_for(
            r#"
[inputs.tags]
type = "array"
items = { type = "string" }
"#,
        );
        let inputs = json!({"tags": ["literal", "{{ resources.x.label }}"]});
        let r = resolve_inputs(&inputs, &schema, &HashMap::new()).unwrap();

        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].path, "/tags/1");
        assert_eq!(pending_list[0].expected_type, Some(&FieldType::String));
    }

    #[test]
    fn resolver_recurses_into_typed_object_array_items() {
        // Each network entry has uuid + name — typed object array. The
        // pending ref inside one of them gets the FieldDef's type.
        let schema = schema_for(
            r#"
[inputs.networks]
type = "array"

[inputs.networks.items.name]
type = "string"
required = true

[inputs.networks.items.uuid]
type = "string"
"#,
        );
        let inputs = json!({
            "networks": [
                {"name": "public", "uuid": "{{ resources.svc.uuid }}"},
            ]
        });
        let r = resolve_inputs(&inputs, &schema, &HashMap::new()).unwrap();

        let pending_list = r.pending_refs();
        assert_eq!(pending_list.len(), 1);
        assert_eq!(pending_list[0].path, "/networks/0/uuid");
        assert_eq!(pending_list[0].expected_type, Some(&FieldType::String));
    }

    #[test]
    fn resolver_errors_on_path_navigation_failure() {
        // Ref's dep_key IS in outputs, but the path navigates to a
        // non-existent field — that's a real error, not deferred.
        let schema = schema_for(
            r#"
[inputs.zone]
type = "string"
"#,
        );
        let outputs = outputs_with(&[("parameters.region", json!("uk-lon1"))]);
        let inputs = json!({"zone": "{{ parameters.region.subfield }}"});
        let err = resolve_inputs(&inputs, &schema, &outputs).unwrap_err();
        assert!(err.contains("subfield"), "got: {err}");
    }

    #[test]
    fn resolver_collapses_partially_resolved_object_to_known_when_all_concrete() {
        // Object with all-resolvable refs should collapse to Known(Value::Object),
        // not stay as Resolvable::Object with Known leaves. Verifies the
        // make_object invariant.
        let schema = schema_for(
            r#"
[inputs.name]
type = "string"

[inputs.zone]
type = "string"
"#,
        );
        let outputs = outputs_with(&[
            ("parameters.name", json!("web-01")),
            ("parameters.region", json!("uk-lon1")),
        ]);
        let inputs = json!({
            "name": "{{ parameters.name }}",
            "zone": "{{ parameters.region }}",
        });
        let r = resolve_inputs(&inputs, &schema, &outputs).unwrap();
        // Must be Known(Value::Object), not Resolvable::Object.
        match r {
            Resolvable::Known(v) => {
                assert_eq!(v, json!({"name": "web-01", "zone": "uk-lon1"}));
            }
            other => panic!("expected Known(Value::Object), got {other:?}"),
        }
    }

    #[test]
    fn resolver_keeps_object_variant_when_subtree_has_pending() {
        // One field resolves, one is pending → result is Resolvable::Object
        // with mixed children (Known + Unknown). Should NOT be Known.
        let schema = schema_for(
            r#"
[inputs.name]
type = "string"

[inputs.uuid]
type = "string"
"#,
        );
        let outputs = outputs_with(&[("parameters.name", json!("web-01"))]);
        let inputs = json!({
            "name": "{{ parameters.name }}",
            "uuid": "{{ resources.svc.uuid }}",
        });
        let r = resolve_inputs(&inputs, &schema, &outputs).unwrap();
        assert!(!r.is_concrete());
        assert!(matches!(r, Resolvable::Object(_)));
    }

    #[test]
    fn resolver_errors_on_non_object_top_level() {
        let schema = schema_for(
            r#"
[inputs.x]
type = "string"
"#,
        );
        let inputs = json!("not an object");
        let err = resolve_inputs(&inputs, &schema, &HashMap::new()).unwrap_err();
        assert!(err.contains("expected"), "got: {err}");
    }
}
