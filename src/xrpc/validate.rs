//! Lexicon validation of an XRPC call, run before its handler.
//!
//! The atproto reference refuses a call whose parameters or input break the
//! method's lexicon with `400 InvalidRequest`, naming the first violation, and
//! so does this: a script never sees a missing required parameter, a string
//! past its `maxLength`, or a body of the wrong shape. Parameters not in the
//! lexicon pass through untouched, as in the reference.
//!
//! Messages follow the reference's wording (`Params/limit can not be greater
//! than 100`, `Input must have the property "text"`), so a client that has
//! learned to read one reads the other.
//!
//! A `ref` to a lexicon this instance doesn't hold is not checked: an app's
//! lexicon may point at `com.atproto.*` or `app.bsky.*` definitions it never
//! installed, and refusing every such call would break it for no gain.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

use crate::AppState;
use crate::error::AppError;
use crate::lexicon::ParsedLexicon;

fn invalid(message: String) -> AppError {
    AppError::XrpcError {
        status: axum::http::StatusCode::BAD_REQUEST,
        code: "InvalidRequest",
        message,
    }
}

/// Coerce a query's parameters to their declared types and check them against
/// `defs.main.parameters`. A repeated parameter is already an array; a
/// parameter the lexicon declares as an array arrives as one even when it was
/// sent once. A missing parameter with a `default` takes it.
pub(crate) fn params(params: &mut HashMap<String, Value>, schema: &Value) -> Result<(), AppError> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    let required = required_names(schema);
    // Parameters are scalars or arrays of them, so there is nothing to resolve.
    let lexicons = Lexicons::default();
    let validator = Validator {
        lexicons: &lexicons,
        nsid: "",
    };

    for (key, def) in properties {
        let path = format!("Params/{key}");
        let Some(raw) = params.remove(key) else {
            if let Some(default) = def.get("default") {
                params.insert(key.clone(), default.clone());
            } else if required.contains(&key.as_str()) {
                return Err(invalid(format!("Params must have the property \"{key}\"")));
            }
            continue;
        };
        let value = coerce_param(&path, def, raw).map_err(invalid)?;
        validator.value(&path, def, &value).map_err(invalid)?;
        params.insert(key.clone(), value);
    }
    Ok(())
}

/// Check a JSON input body against `defs.main.input.schema`. A method whose
/// input isn't JSON, or that declares no schema, isn't checked here.
pub(crate) async fn input(
    state: &AppState,
    lexicon: &ParsedLexicon,
    body: &Value,
) -> Result<(), AppError> {
    let Some(input) = lexicon.input.as_ref() else {
        return Ok(());
    };
    let json = input
        .get("encoding")
        .and_then(Value::as_str)
        .is_some_and(|e| e == "application/json");
    let Some(schema) = input.get("schema").filter(|_| json) else {
        return Ok(());
    };

    let lexicons = Lexicons::load(state, &lexicon.id, &lexicon.raw, schema).await;
    Validator {
        lexicons: &lexicons,
        nsid: &lexicon.id,
    }
    .value("Input", schema, body)
    .map_err(invalid)
}

fn required_names(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// A query parameter arrives as a string, or an array of strings when it was
/// repeated. A script calling another method passes typed values, which are
/// kept as they are.
fn coerce_param(path: &str, def: &Value, raw: Value) -> Result<Value, String> {
    match def.get("type").and_then(Value::as_str) {
        Some("array") => {
            let items = def.get("items").unwrap_or(&Value::Null);
            let raw = match raw {
                Value::Array(values) => values,
                single => vec![single],
            };
            raw.into_iter()
                .enumerate()
                .map(|(i, v)| coerce_param(&format!("{path}/{i}"), items, v))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        Some("integer") => match raw {
            Value::String(s) => s
                .parse::<i64>()
                .map(|n| Value::Number(n.into()))
                .map_err(|_| format!("{path} must be an integer")),
            other => Ok(other),
        },
        Some("boolean") => match raw {
            Value::String(s) => match s.as_str() {
                "true" | "1" => Ok(Value::Bool(true)),
                "false" | "0" => Ok(Value::Bool(false)),
                _ => Err(format!("{path} must be a boolean")),
            },
            other => Ok(other),
        },
        _ => Ok(raw),
    }
}

/// The lexicons a schema's `ref`s and `union`s may name, by NSID. Gathered
/// before validating, so the walk itself needn't await.
#[derive(Default)]
struct Lexicons(HashMap<String, Value>);

impl Lexicons {
    async fn load(state: &AppState, nsid: &str, raw: &Value, schema: &Value) -> Self {
        let mut found = HashMap::from([(nsid.to_string(), raw.clone())]);
        let mut pending = Vec::new();
        external_refs(nsid, schema, &mut pending);
        if let Some(defs) = raw.get("defs") {
            external_refs(nsid, defs, &mut pending);
        }
        while let Some(next) = pending.pop() {
            if found.contains_key(&next) {
                continue;
            }
            let Some(lexicon) = state.lexicons.get(&next).await else {
                continue;
            };
            if let Some(defs) = lexicon.raw.get("defs") {
                external_refs(&next, defs, &mut pending);
            }
            found.insert(next, lexicon.raw.clone());
        }
        Self(found)
    }

    /// The definition `reference` names, read from inside the lexicon `nsid`.
    fn resolve(&self, nsid: &str, reference: &str) -> Option<(&str, &Value)> {
        let (target, def) = split_ref(nsid, reference);
        let (key, lexicon) = self.0.get_key_value(target)?;
        Some((key.as_str(), lexicon.get("defs")?.get(def)?))
    }
}

/// `#name` is a definition in the same lexicon; `a.b.c` is `a.b.c#main`.
fn split_ref<'a>(nsid: &'a str, reference: &'a str) -> (&'a str, &'a str) {
    match reference.split_once('#') {
        Some(("", def)) => (nsid, def),
        Some((target, def)) => (target, def),
        None => (reference, "main"),
    }
}

fn external_refs(nsid: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let refs = match map.get("type").and_then(Value::as_str) {
                Some("ref") => map.get("ref").into_iter().collect::<Vec<_>>(),
                Some("union") => map
                    .get("refs")
                    .and_then(Value::as_array)
                    .map(|r| r.iter().collect())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            for r in refs.into_iter().filter_map(Value::as_str) {
                let (target, _) = split_ref(nsid, r);
                if target != nsid {
                    out.push(target.to_string());
                }
            }
            for child in map.values() {
                external_refs(nsid, child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| external_refs(nsid, v, out)),
        _ => {}
    }
}

struct Validator<'a> {
    lexicons: &'a Lexicons,
    /// The lexicon the definition being checked belongs to, for `#local` refs.
    nsid: &'a str,
}

impl Validator<'_> {
    fn value(&self, path: &str, def: &Value, v: &Value) -> Result<(), String> {
        match def.get("type").and_then(Value::as_str) {
            Some("string") => string(path, def, v),
            Some("integer") => integer(path, def, v),
            Some("boolean") => match v {
                Value::Bool(b) => match def.get("const").and_then(Value::as_bool) {
                    Some(c) if c != *b => Err(format!("{path} must be {c}")),
                    _ => Ok(()),
                },
                _ => Err(format!("{path} must be a boolean")),
            },
            Some("null") => match v {
                Value::Null => Ok(()),
                _ => Err(format!("{path} must be null")),
            },
            Some("array") => self.array(path, def, v),
            Some("object") => self.object(path, def, v),
            Some("ref") => self.reference(path, def, v),
            Some("union") => self.union(path, def, v),
            Some("unknown") => match v {
                Value::Object(_) => Ok(()),
                _ => Err(format!("{path} must be an object")),
            },
            Some("bytes") => marker(path, v, "$bytes", "a byte array"),
            Some("cid-link") => marker(path, v, "$link", "a CID"),
            Some("blob") => match v {
                Value::Object(_) => Ok(()),
                _ => Err(format!("{path} should be a blob ref")),
            },
            _ => Ok(()),
        }
    }

    fn array(&self, path: &str, def: &Value, v: &Value) -> Result<(), String> {
        let Value::Array(items) = v else {
            return Err(format!("{path} must be an array"));
        };
        if let Some(max) = def.get("maxLength").and_then(Value::as_u64)
            && items.len() as u64 > max
        {
            return Err(format!("{path} must not have more than {max} elements"));
        }
        if let Some(min) = def.get("minLength").and_then(Value::as_u64)
            && (items.len() as u64) < min
        {
            return Err(format!("{path} must not have fewer than {min} elements"));
        }
        let item_def = def.get("items").unwrap_or(&Value::Null);
        for (i, item) in items.iter().enumerate() {
            self.value(&format!("{path}/{i}"), item_def, item)?;
        }
        Ok(())
    }

    fn object(&self, path: &str, def: &Value, v: &Value) -> Result<(), String> {
        let Value::Object(map) = v else {
            return Err(format!("{path} must be an object"));
        };
        for name in required_names(def) {
            if !map.contains_key(name) {
                return Err(format!("{path} must have the property \"{name}\""));
            }
        }
        let nullable = def
            .get("nullable")
            .and_then(Value::as_array)
            .map(|n| n.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let Some(properties) = def.get("properties").and_then(Value::as_object) else {
            return Ok(());
        };
        for (key, prop) in properties {
            let Some(value) = map.get(key) else { continue };
            if value.is_null() && nullable.contains(&key.as_str()) {
                continue;
            }
            self.value(&format!("{path}/{key}"), prop, value)?;
        }
        Ok(())
    }

    fn reference(&self, path: &str, def: &Value, v: &Value) -> Result<(), String> {
        let Some(r) = def.get("ref").and_then(Value::as_str) else {
            return Ok(());
        };
        match self.lexicons.resolve(self.nsid, r) {
            Some((nsid, target)) => Validator {
                lexicons: self.lexicons,
                nsid,
            }
            .value(path, target, v),
            None => Ok(()),
        }
    }

    fn union(&self, path: &str, def: &Value, v: &Value) -> Result<(), String> {
        let Value::Object(map) = v else {
            return Err(format!(
                "{path} must be an object which includes the \"$type\" property"
            ));
        };
        let Some(ty) = map.get("$type").and_then(Value::as_str) else {
            return Err(format!(
                "{path} must be an object which includes the \"$type\" property"
            ));
        };
        let refs: Vec<&str> = def
            .get("refs")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let wanted = split_ref(ty, ty);
        let matched = refs.iter().find(|r| split_ref(self.nsid, r) == wanted);
        match matched {
            Some(r) => self.reference(path, &serde_json::json!({ "ref": r }), v),
            None if def.get("closed").and_then(Value::as_bool) == Some(true) => {
                Err(format!("{path} $type must be one of {}", refs.join(", ")))
            }
            None => Ok(()),
        }
    }
}

fn marker(path: &str, v: &Value, key: &str, what: &str) -> Result<(), String> {
    match v.get(key) {
        Some(Value::String(_)) => Ok(()),
        _ => Err(format!("{path} must be {what}")),
    }
}

fn string(path: &str, def: &Value, v: &Value) -> Result<(), String> {
    let Value::String(s) = v else {
        return Err(format!("{path} must be a string"));
    };
    if let Some(c) = def.get("const").and_then(Value::as_str)
        && s != c
    {
        return Err(format!("{path} must be {c}"));
    }
    if let Some(options) = def.get("enum").and_then(Value::as_array)
        && !options.iter().any(|o| o.as_str() == Some(s))
    {
        let options: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
        return Err(format!("{path} must be one of ({})", options.join("|")));
    }
    // Lengths are UTF-8 bytes, graphemes are what a person sees.
    if let Some(max) = def.get("maxLength").and_then(Value::as_u64)
        && s.len() as u64 > max
    {
        return Err(format!("{path} must not be longer than {max} characters"));
    }
    if let Some(min) = def.get("minLength").and_then(Value::as_u64)
        && (s.len() as u64) < min
    {
        return Err(format!("{path} must not be shorter than {min} characters"));
    }
    if def.get("maxGraphemes").is_some() || def.get("minGraphemes").is_some() {
        let graphemes = s.graphemes(true).count() as u64;
        if let Some(max) = def.get("maxGraphemes").and_then(Value::as_u64)
            && graphemes > max
        {
            return Err(format!("{path} must not be longer than {max} graphemes"));
        }
        if let Some(min) = def.get("minGraphemes").and_then(Value::as_u64)
            && graphemes < min
        {
            return Err(format!("{path} must not be shorter than {min} graphemes"));
        }
    }
    match def.get("format").and_then(Value::as_str) {
        Some(format) if !format_ok(format, s) => {
            Err(format!("{path} must be a valid {}", format_name(format)))
        }
        _ => Ok(()),
    }
}

fn integer(path: &str, def: &Value, v: &Value) -> Result<(), String> {
    let Some(n) = v.as_i64() else {
        return Err(format!("{path} must be an integer"));
    };
    if let Some(c) = def.get("const").and_then(Value::as_i64)
        && n != c
    {
        return Err(format!("{path} must be {c}"));
    }
    if let Some(options) = def.get("enum").and_then(Value::as_array)
        && !options.iter().any(|o| o.as_i64() == Some(n))
    {
        let options: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(format!("{path} must be one of ({})", options.join("|")));
    }
    if let Some(max) = def.get("maximum").and_then(Value::as_i64)
        && n > max
    {
        return Err(format!("{path} can not be greater than {max}"));
    }
    if let Some(min) = def.get("minimum").and_then(Value::as_i64)
        && n < min
    {
        return Err(format!("{path} can not be less than {min}"));
    }
    Ok(())
}

fn format_name(format: &str) -> &str {
    match format {
        "did" => "did",
        "handle" => "handle",
        "at-identifier" => "did or a handle",
        "at-uri" => "at-uri",
        "datetime" => "datetime (ISO 8601 with a timezone)",
        "nsid" => "nsid",
        "cid" => "cid",
        "uri" => "uri",
        "language" => "language code",
        "tid" => "TID",
        "record-key" => "record key",
        other => other,
    }
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a valid pattern"))
}

fn is_did(s: &str) -> bool {
    static DID: OnceLock<Regex> = OnceLock::new();
    s.len() <= 2048 && regex(&DID, r"^did:[a-z]+:[a-zA-Z0-9._:%-]*[a-zA-Z0-9._-]$").is_match(s)
}

fn is_handle(s: &str) -> bool {
    static HANDLE: OnceLock<Regex> = OnceLock::new();
    s.len() <= 253
        && regex(
            &HANDLE,
            r"^([a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$",
        )
        .is_match(s)
}

fn is_record_key(s: &str) -> bool {
    static RKEY: OnceLock<Regex> = OnceLock::new();
    s != "." && s != ".." && regex(&RKEY, r"^[a-zA-Z0-9_~.:-]{1,512}$").is_match(s)
}

fn format_ok(format: &str, s: &str) -> bool {
    static URI: OnceLock<Regex> = OnceLock::new();
    static LANGUAGE: OnceLock<Regex> = OnceLock::new();
    static TID: OnceLock<Regex> = OnceLock::new();
    match format {
        "did" => is_did(s),
        "handle" => is_handle(s),
        "at-identifier" => is_did(s) || is_handle(s),
        "nsid" => happyview_nsid::validate_nsid(s).is_ok(),
        "cid" => cid::Cid::try_from(s).is_ok(),
        "datetime" => {
            s.contains('T')
                && !s.ends_with("-00:00")
                && chrono::DateTime::parse_from_rfc3339(s).is_ok()
        }
        "uri" => s.len() <= 8192 && regex(&URI, r"^[a-z][a-z.+-]*:\S+$").is_match(s),
        "language" => regex(&LANGUAGE, r"^(i|[a-z]{2,3})(-[a-zA-Z0-9]+)*$").is_match(s),
        "tid" => regex(
            &TID,
            r"^[234567abcdefghij][234567abcdefghijklmnopqrstuvwxyz]{12}$",
        )
        .is_match(s),
        "record-key" => is_record_key(s),
        "at-uri" => at_uri_ok(s),
        _ => true,
    }
}

/// `at://<did or handle>[/<collection nsid>[/<record key>]]`, with an
/// optional `#fragment`.
fn at_uri_ok(s: &str) -> bool {
    if s.len() > 8192 {
        return false;
    }
    let Some(rest) = s.strip_prefix("at://") else {
        return false;
    };
    let rest = rest.split_once('#').map_or(rest, |(path, _)| path);
    let mut parts = rest.split('/');
    let authority = parts.next().unwrap_or("");
    if !(is_did(authority) || is_handle(authority)) {
        return false;
    }
    match (parts.next(), parts.next(), parts.next()) {
        (None, ..) => true,
        (Some(collection), None, _) => happyview_nsid::validate_nsid(collection).is_ok(),
        (Some(collection), Some(rkey), None) => {
            happyview_nsid::validate_nsid(collection).is_ok() && is_record_key(rkey)
        }
        _ => false,
    }
}
