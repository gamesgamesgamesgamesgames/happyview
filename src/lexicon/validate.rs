//! Checking an uploaded lexicon against the AT Protocol lexicon spec.
//!
//! [`validate`] is the single rule set. [`super::check_document`] renders its
//! errors as one string for the project build's diagnostics, and the admin
//! upload renders them per field, so the file and the API cannot disagree
//! about what a valid lexicon is.
//!
//! Severity is the one thing a caller does not choose. An [`Severity::Error`]
//! is a document that cannot work: an unknown type, a record with no key, a
//! `ref` pointing at a def that is not there. A [`Severity::Warning`] is a
//! document that works but reads oddly — a def name that is not camelCase, an
//! array with no `items` — and never refuses anything, matching what the
//! declarations generator already does with a type it cannot render.

use serde::Serialize;
use serde_json::Value;

/// The most a lexicon document may be. The largest real one to hand is
/// `at.happyproto.plugin.release` at about 6 KB, so this is roughly forty
/// times the biggest anyone has written, while still bounding what an upload
/// can ask the server to parse and store.
pub const MAX_LEXICON_BYTES: usize = 256 * 1024;

/// The field types a lexicon may use, as the declarations generator reads
/// them (`src/project/declarations/lexicon.rs`).
const FIELD_TYPES: &[&str] = &[
    "null", "boolean", "integer", "string", "bytes", "cid-link", "blob", "array", "object",
    "params", "token", "ref", "union", "unknown",
];

/// The types that define an endpoint or a collection. The spec allows these
/// only as the `main` def.
const PRIMARY_TYPES: &[&str] = &["record", "query", "procedure", "subscription"];

/// How a record's rkey is chosen.
const RECORD_KEYS: &[&str] = &["tid", "nsid", "any"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The document cannot work as written.
    Error,
    /// The document works; something about it is still worth saying.
    Warning,
}

/// One thing wrong with a lexicon, and where in the document it is.
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    /// A dotted path into the document, e.g.
    /// `defs.main.record.properties.title`. The empty string is the document
    /// itself.
    pub path: String,
    pub message: String,
    pub severity: Severity,
}

impl Issue {
    fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            severity: Severity::Error,
        }
    }

    fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            severity: Severity::Warning,
        }
    }

    /// `path: message`, or just the message for a document-level issue.
    pub fn render(&self) -> String {
        if self.path.is_empty() {
            self.message.clone()
        } else {
            format!("{}: {}", self.path, self.message)
        }
    }
}

/// Every issue with `raw`, in document order, errors and warnings together.
/// An empty answer means the document is valid.
pub fn validate(raw: &Value) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut refs: Vec<(String, String)> = Vec::new();

    if let Ok(bytes) = serde_json::to_vec(raw)
        && bytes.len() > MAX_LEXICON_BYTES
    {
        issues.push(Issue::error(
            "",
            format!(
                "the document is {} bytes, over the {MAX_LEXICON_BYTES} byte limit",
                bytes.len()
            ),
        ));
    }

    match raw.get("lexicon").and_then(Value::as_i64) {
        None => issues.push(Issue::error(
            "lexicon",
            "lexicon JSON must have a numeric 'lexicon' field",
        )),
        Some(1) => {}
        Some(other) => issues.push(Issue::error(
            "lexicon",
            format!("unsupported lexicon version: {other}"),
        )),
    }

    let id = match raw.get("id").and_then(Value::as_str) {
        None => {
            issues.push(Issue::error(
                "id",
                "lexicon JSON must have a string 'id' field",
            ));
            None
        }
        Some(id) => match happyview_nsid::validate_nsid(id) {
            Ok(()) => Some(id),
            Err(e) => {
                issues.push(Issue::error("id", e.to_string()));
                None
            }
        },
    };

    if let Some(description) = raw.get("description")
        && !description.is_string()
    {
        issues.push(Issue::warning("description", "should be a string"));
    }

    let defs = match raw.get("defs") {
        None => {
            issues.push(Issue::error(
                "defs",
                "lexicon JSON must have a 'defs' object",
            ));
            return issues;
        }
        Some(Value::Object(defs)) => defs,
        Some(_) => {
            issues.push(Issue::error("defs", "must be an object"));
            return issues;
        }
    };
    if defs.is_empty() {
        issues.push(Issue::error("defs", "defines nothing"));
    }

    for (name, def) in defs {
        let path = format!("defs.{name}");
        if !is_def_name(name) {
            issues.push(Issue::warning(
                &path,
                "a def name should start with a letter and hold only letters and digits",
            ));
        }
        let Some(def) = def.as_object() else {
            issues.push(Issue::error(&path, "must be an object"));
            continue;
        };
        let Some(kind) = def.get("type").and_then(Value::as_str) else {
            issues.push(Issue::error(
                format!("{path}.type"),
                "must be a string naming the def's type",
            ));
            continue;
        };
        if PRIMARY_TYPES.contains(&kind) {
            if name != "main" {
                issues.push(Issue::error(
                    format!("{path}.type"),
                    format!("a '{kind}' may only be the 'main' def, not '{name}'"),
                ));
            }
            primary(kind, def, &path, &mut issues, &mut refs);
        } else if FIELD_TYPES.contains(&kind) {
            field(def.get("type"), def, &path, &mut issues, &mut refs);
        } else {
            issues.push(Issue::error(
                format!("{path}.type"),
                format!("unknown lexicon type '{kind}'"),
            ));
        }
    }

    resolve_refs(id, defs, &refs, &mut issues);
    issues
}

/// Local refs must name a def this document defines; a ref into another NSID
/// is only checked for shape, since the document it names is not here.
fn resolve_refs(
    id: Option<&str>,
    defs: &serde_json::Map<String, Value>,
    refs: &[(String, String)],
    issues: &mut Vec<Issue>,
) {
    for (path, reference) in refs {
        let mut parts = reference.splitn(2, '#');
        let nsid = parts.next().unwrap_or("");
        let def = parts.next();
        if reference.matches('#').count() > 1 {
            issues.push(Issue::error(path, format!("'{reference}' has two '#'")));
            continue;
        }
        // A ref naming this document's own NSID is a local ref written the
        // long way, and resolves the same.
        let local = nsid.is_empty() || Some(nsid) == id;
        if !nsid.is_empty()
            && let Err(e) = happyview_nsid::validate_nsid(nsid)
        {
            issues.push(Issue::error(
                path,
                format!("'{reference}' does not name a lexicon: {e}"),
            ));
            continue;
        }
        if !local {
            continue;
        }
        let wanted = def.unwrap_or("main");
        if !defs.contains_key(wanted) {
            issues.push(Issue::error(
                path,
                format!("'{reference}' names no def in this lexicon ('{wanted}' is not defined)"),
            ));
        }
    }
}

/// A `record`, `query`, `procedure` or `subscription` def.
fn primary(
    kind: &str,
    def: &serde_json::Map<String, Value>,
    path: &str,
    issues: &mut Vec<Issue>,
    refs: &mut Vec<(String, String)>,
) {
    match kind {
        "record" => {
            match def.get("key").and_then(Value::as_str) {
                None => issues.push(Issue::error(
                    format!("{path}.key"),
                    "a record must say how its rkey is chosen: tid, nsid, any, or literal:<value>",
                )),
                Some(key) if RECORD_KEYS.contains(&key) => {}
                Some(key) if key.starts_with("literal:") => {
                    if key.len() == "literal:".len() {
                        issues.push(Issue::error(
                            format!("{path}.key"),
                            "'literal:' needs the literal value after the colon",
                        ));
                    }
                }
                Some(key) => issues.push(Issue::error(
                    format!("{path}.key"),
                    format!(
                        "unknown record key '{key}': must be tid, nsid, any, or literal:<value>"
                    ),
                )),
            }
            match def.get("record") {
                None => issues.push(Issue::error(
                    format!("{path}.record"),
                    "a record must carry its 'record' schema",
                )),
                Some(schema) => {
                    let schema_path = format!("{path}.record");
                    if schema.get("type").and_then(Value::as_str) != Some("object") {
                        issues.push(Issue::error(
                            format!("{schema_path}.type"),
                            "a record's schema must be an object",
                        ));
                    }
                    if let Some(map) = schema.as_object() {
                        field(schema.get("type"), map, &schema_path, issues, refs);
                    }
                }
            }
        }
        "query" | "procedure" => {
            if let Some(params) = def.get("parameters") {
                let params_path = format!("{path}.parameters");
                if params.get("type").and_then(Value::as_str) != Some("params") {
                    issues.push(Issue::error(
                        format!("{params_path}.type"),
                        "parameters must be of type 'params'",
                    ));
                }
                if let Some(map) = params.as_object() {
                    field(params.get("type"), map, &params_path, issues, refs);
                }
            }
            for body in ["input", "output"] {
                if kind == "query" && body == "input" {
                    if def.contains_key("input") {
                        issues.push(Issue::error(
                            format!("{path}.input"),
                            "a query takes no input body; use 'parameters'",
                        ));
                    }
                    continue;
                }
                if let Some(value) = def.get(body) {
                    body_schema(value, &format!("{path}.{body}"), issues, refs);
                }
            }
        }
        "subscription" => {
            if let Some(message) = def.get("message") {
                body_schema(message, &format!("{path}.message"), issues, refs);
            }
        }
        _ => {}
    }
}

/// The `{encoding, schema}` shape an input, output or message carries.
fn body_schema(
    body: &Value,
    path: &str,
    issues: &mut Vec<Issue>,
    refs: &mut Vec<(String, String)>,
) {
    let Some(map) = body.as_object() else {
        issues.push(Issue::error(path, "must be an object"));
        return;
    };
    if let Some(encoding) = map.get("encoding")
        && !encoding.is_string()
    {
        issues.push(Issue::error(format!("{path}.encoding"), "must be a string"));
    }
    if let Some(schema) = map.get("schema") {
        let schema_path = format!("{path}.schema");
        match schema.as_object() {
            Some(schema_map) => field(schema.get("type"), schema_map, &schema_path, issues, refs),
            None => issues.push(Issue::error(&schema_path, "must be an object")),
        }
    }
}

/// One field schema, and everything nested inside it.
fn field(
    kind: Option<&Value>,
    map: &serde_json::Map<String, Value>,
    path: &str,
    issues: &mut Vec<Issue>,
    refs: &mut Vec<(String, String)>,
) {
    let Some(kind) = kind.and_then(Value::as_str) else {
        issues.push(Issue::error(
            format!("{path}.type"),
            "must be a string naming the field's type",
        ));
        return;
    };
    match kind {
        "object" | "params" => {
            match map.get("properties") {
                None => {}
                Some(Value::Object(properties)) => {
                    for (name, property) in properties {
                        let property_path = format!("{path}.properties.{name}");
                        match property.as_object() {
                            Some(property_map) => field(
                                property.get("type"),
                                property_map,
                                &property_path,
                                issues,
                                refs,
                            ),
                            None => issues.push(Issue::error(&property_path, "must be an object")),
                        }
                    }
                }
                Some(_) => issues.push(Issue::error(
                    format!("{path}.properties"),
                    "must be an object",
                )),
            }
            for list in ["required", "nullable"] {
                let Some(value) = map.get(list) else { continue };
                let list_path = format!("{path}.{list}");
                let Some(names) = value.as_array() else {
                    issues.push(Issue::error(
                        &list_path,
                        "must be an array of property names",
                    ));
                    continue;
                };
                let properties = map.get("properties").and_then(Value::as_object);
                for (i, name) in names.iter().enumerate() {
                    let Some(name) = name.as_str() else {
                        issues.push(Issue::error(
                            format!("{list_path}[{i}]"),
                            "must be a property name",
                        ));
                        continue;
                    };
                    if properties.is_some_and(|p| !p.contains_key(name)) {
                        issues.push(Issue::error(
                            format!("{list_path}[{i}]"),
                            format!("'{name}' is not a property of this object"),
                        ));
                    }
                }
            }
        }
        "array" => match map.get("items") {
            None => issues.push(Issue::warning(
                format!("{path}.items"),
                "an array with no 'items' accepts anything",
            )),
            Some(items) => {
                let items_path = format!("{path}.items");
                match items.as_object() {
                    Some(items_map) => {
                        field(items.get("type"), items_map, &items_path, issues, refs)
                    }
                    None => issues.push(Issue::error(&items_path, "must be an object")),
                }
            }
        },
        "ref" => match map.get("ref").and_then(Value::as_str) {
            Some(reference) if !reference.is_empty() => {
                refs.push((format!("{path}.ref"), reference.to_string()))
            }
            Some(_) => issues.push(Issue::error(format!("{path}.ref"), "is empty")),
            None => issues.push(Issue::error(
                format!("{path}.ref"),
                "a ref must name what it points at",
            )),
        },
        "union" => match map.get("refs") {
            None => issues.push(Issue::error(
                format!("{path}.refs"),
                "a union must list the refs it accepts",
            )),
            Some(Value::Array(list)) => {
                if list.is_empty() {
                    issues.push(Issue::warning(
                        format!("{path}.refs"),
                        "a union accepting nothing matches nothing",
                    ));
                }
                for (i, reference) in list.iter().enumerate() {
                    match reference.as_str() {
                        Some(reference) if !reference.is_empty() => {
                            refs.push((format!("{path}.refs[{i}]"), reference.to_string()))
                        }
                        _ => issues.push(Issue::error(
                            format!("{path}.refs[{i}]"),
                            "must be a ref string",
                        )),
                    }
                }
            }
            Some(_) => issues.push(Issue::error(format!("{path}.refs"), "must be an array")),
        },
        "string" => {
            bounds(map, path, "minLength", "maxLength", issues);
            bounds(map, path, "minGraphemes", "maxGraphemes", issues);
            for list in ["enum", "knownValues"] {
                if let Some(value) = map.get(list)
                    && !value.is_array()
                {
                    issues.push(Issue::error(format!("{path}.{list}"), "must be an array"));
                }
            }
        }
        "integer" => bounds(map, path, "minimum", "maximum", issues),
        "null" | "boolean" | "bytes" | "cid-link" | "blob" | "token" | "unknown" => {}
        other => issues.push(Issue::error(
            format!("{path}.type"),
            format!("unknown lexicon type '{other}'"),
        )),
    }
}

/// A min that is above its max describes nothing, but it stores and serves
/// perfectly well, so it is a warning.
fn bounds(
    map: &serde_json::Map<String, Value>,
    path: &str,
    min: &str,
    max: &str,
    issues: &mut Vec<Issue>,
) {
    let (Some(low), Some(high)) = (
        map.get(min).and_then(Value::as_i64),
        map.get(max).and_then(Value::as_i64),
    ) else {
        return;
    };
    if low > high {
        issues.push(Issue::warning(
            format!("{path}.{min}"),
            format!("{min} {low} is above {max} {high}, so nothing can satisfy both"),
        ));
    }
}

/// Def names are alphanumeric, starting with a letter.
fn is_def_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn errors(raw: &Value) -> Vec<String> {
        validate(raw)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.render())
            .collect()
    }

    fn warnings(raw: &Value) -> Vec<String> {
        validate(raw)
            .into_iter()
            .filter(|i| i.severity == Severity::Warning)
            .map(|i| i.render())
            .collect()
    }

    /// A record lexicon with a nested object, an array, a local ref and a
    /// union, so the walk has somewhere to go.
    fn good() -> Value {
        json!({
            "lexicon": 1,
            "id": "com.example.thing",
            "defs": {
                "main": {
                    "type": "record",
                    "key": "tid",
                    "record": {
                        "type": "object",
                        "required": ["title"],
                        "properties": {
                            "title": { "type": "string", "maxLength": 300 },
                            "tags": { "type": "array", "items": { "type": "string" } },
                            "author": { "type": "ref", "ref": "#author" },
                            "either": { "type": "union", "refs": ["#author", "com.example.other#thing"] },
                            "blob": { "type": "blob" },
                            "when": { "type": "string", "format": "datetime" }
                        }
                    }
                },
                "author": {
                    "type": "object",
                    "properties": { "did": { "type": "string", "format": "did" } }
                }
            }
        })
    }

    #[test]
    fn a_valid_lexicon_has_nothing_to_say() {
        assert_eq!(validate(&good()).len(), 0, "{:?}", validate(&good()));
    }

    /// Every case here is a document someone could plausibly write, and the
    /// message has to name the field so an editor can point at it.
    #[test]
    fn known_bad_documents_are_refused_per_field() {
        let cases: Vec<(&str, Value, &str)> = vec![
            (
                "no version",
                json!({ "id": "com.example.thing", "defs": { "main": { "type": "object" } } }),
                "lexicon: lexicon JSON must have a numeric 'lexicon' field",
            ),
            (
                "wrong version",
                json!({ "lexicon": 2, "id": "com.example.thing", "defs": { "main": { "type": "object" } } }),
                "lexicon: unsupported lexicon version: 2",
            ),
            (
                "id is not an nsid",
                json!({ "lexicon": 1, "id": "not an nsid", "defs": { "main": { "type": "object" } } }),
                "id: invalid NSID",
            ),
            (
                "no defs",
                json!({ "lexicon": 1, "id": "com.example.thing" }),
                "defs: lexicon JSON must have a 'defs' object",
            ),
            (
                "defs is not an object",
                json!({ "lexicon": 1, "id": "com.example.thing", "defs": [] }),
                "defs: must be an object",
            ),
            (
                "a def with no type",
                json!({ "lexicon": 1, "id": "com.example.thing", "defs": { "main": {} } }),
                "defs.main.type: must be a string naming the def's type",
            ),
            (
                "a misspelled type",
                json!({ "lexicon": 1, "id": "com.example.thing", "defs": { "main": { "type": "recrod" } } }),
                "defs.main.type: unknown lexicon type 'recrod'",
            ),
            (
                "a record somewhere other than main",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "other": { "type": "record", "key": "tid", "record": { "type": "object" } } }
                }),
                "defs.other.type: a 'record' may only be the 'main' def, not 'other'",
            ),
            (
                "a record with no key",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "record", "record": { "type": "object" } } }
                }),
                "defs.main.key: a record must say how its rkey is chosen",
            ),
            (
                "a record with an unknown key type",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "record", "key": "uuid", "record": { "type": "object" } } }
                }),
                "defs.main.key: unknown record key 'uuid'",
            ),
            (
                "literal with nothing after the colon",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "record", "key": "literal:", "record": { "type": "object" } } }
                }),
                "defs.main.key: 'literal:' needs the literal value",
            ),
            (
                "a record with no schema",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "record", "key": "tid" } }
                }),
                "defs.main.record: a record must carry its 'record' schema",
            ),
            (
                "a record whose schema is not an object",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "record", "key": "tid", "record": { "type": "string" } } }
                }),
                "defs.main.record.type: a record's schema must be an object",
            ),
            (
                "a local ref that resolves to nothing",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": {
                        "type": "object",
                        "properties": { "a": { "type": "ref", "ref": "#missing" } }
                    } }
                }),
                "defs.main.properties.a.ref: '#missing' names no def in this lexicon",
            ),
            (
                "a ref with no target",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "object", "properties": { "a": { "type": "ref" } } } }
                }),
                "defs.main.properties.a.ref: a ref must name what it points at",
            ),
            (
                "a union with no refs",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "object", "properties": { "a": { "type": "union" } } } }
                }),
                "defs.main.properties.a.refs: a union must list the refs it accepts",
            ),
            (
                "a union ref that resolves to nothing",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": {
                        "type": "object",
                        "properties": { "a": { "type": "union", "refs": ["#gone"] } }
                    } }
                }),
                "defs.main.properties.a.refs[0]: '#gone' names no def in this lexicon",
            ),
            (
                "a ref whose nsid is not an nsid",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": {
                        "type": "object",
                        "properties": { "a": { "type": "ref", "ref": "nope#x" } }
                    } }
                }),
                "does not name a lexicon",
            ),
            (
                "required names a property that is not there",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": {
                        "type": "object",
                        "required": ["absent"],
                        "properties": { "present": { "type": "string" } }
                    } }
                }),
                "defs.main.required[0]: 'absent' is not a property of this object",
            ),
            (
                "a nested property with a bad type",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": {
                        "type": "object",
                        "properties": { "a": { "type": "object", "properties": { "b": { "type": "flote" } } } }
                    } }
                }),
                "defs.main.properties.a.properties.b.type: unknown lexicon type 'flote'",
            ),
            (
                "a query with an input body",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "query", "input": { "encoding": "application/json" } } }
                }),
                "defs.main.input: a query takes no input body; use 'parameters'",
            ),
            (
                "parameters that are not params",
                json!({
                    "lexicon": 1, "id": "com.example.thing",
                    "defs": { "main": { "type": "query", "parameters": { "type": "object" } } }
                }),
                "defs.main.parameters.type: parameters must be of type 'params'",
            ),
        ];

        for (name, raw, expected) in cases {
            let found = errors(&raw);
            assert!(
                found.iter().any(|e| e.contains(expected)),
                "{name}: expected an error containing {expected:?}, got {found:?}"
            );
        }
    }

    /// A ref written as this document's own NSID is a local ref the long way.
    #[test]
    fn a_ref_through_the_documents_own_nsid_resolves_locally() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.thing",
            "defs": {
                "main": {
                    "type": "object",
                    "properties": {
                        "here": { "type": "ref", "ref": "com.example.thing#other" },
                        "gone": { "type": "ref", "ref": "com.example.thing#absent" }
                    }
                },
                "other": { "type": "object" }
            }
        });
        let found = errors(&raw);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("'absent' is not defined"), "{found:?}");
    }

    /// A ref into another lexicon is not resolvable from here, so its shape
    /// is all that can be checked.
    #[test]
    fn a_ref_into_another_lexicon_is_only_checked_for_shape() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.thing",
            "defs": { "main": {
                "type": "object",
                "properties": { "a": { "type": "ref", "ref": "com.example.other#whatever" } }
            } }
        });
        assert_eq!(errors(&raw), Vec::<String>::new());
    }

    #[test]
    fn warnings_do_not_refuse_the_document() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.thing",
            "defs": {
                "main": { "type": "object", "properties": {
                    "loose": { "type": "array" },
                    "impossible": { "type": "string", "minLength": 10, "maxLength": 2 }
                } },
                "Not_A_Name": { "type": "object" }
            }
        });
        assert_eq!(
            errors(&raw),
            Vec::<String>::new(),
            "none of these are errors"
        );
        let warned = warnings(&raw);
        assert!(warned.iter().any(|w| w.contains("items")), "{warned:?}");
        assert!(
            warned.iter().any(|w| w.contains("minLength 10 is above")),
            "{warned:?}"
        );
        assert!(
            warned
                .iter()
                .any(|w| w.contains("camelCase") || w.contains("letters and digits")),
            "{warned:?}"
        );
    }

    #[test]
    fn an_oversized_document_is_refused() {
        let mut raw = good();
        let filler: String = "x".repeat(MAX_LEXICON_BYTES);
        raw["defs"]["main"]["description"] = json!(filler);
        let found = errors(&raw);
        assert!(found.iter().any(|e| e.contains("over the")), "{found:?}");
    }

    /// `check_document` is the build's view of the same rules: errors join
    /// into one diagnostic, and warnings never refuse.
    #[test]
    fn check_document_renders_the_same_errors() {
        assert_eq!(
            super::super::check_document(&good()).unwrap(),
            "com.example.thing"
        );
        let raw = json!({
            "lexicon": 1, "id": "com.example.thing",
            "defs": { "main": { "type": "record", "key": "nope", "record": { "type": "object" } } }
        });
        let err = super::super::check_document(&raw).unwrap_err();
        assert!(err.contains("defs.main.key"), "{err}");
    }
}
