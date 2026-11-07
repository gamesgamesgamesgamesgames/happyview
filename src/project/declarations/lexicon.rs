//! A lexicon document as declaration types: what a handler for it receives
//! and returns, and every def it names.

use std::collections::BTreeMap;

use happyview_plugin_sdk::declarations::{
    LexiconDeclaration, MainDef, ObjectType, Property, TypeExpr,
};
use serde_json::Value;

/// The main kinds a handler can be written for, plus `space`; anything else
/// is a lexicon of plain definitions.
const MAIN_KINDS: &[&str] = &["query", "procedure", "record", "space"];

/// `raw` is a lexicon the build has already accepted, so nothing here is an
/// error: a field type the model has no word for becomes `unknown`, with a
/// warning naming it.
pub fn lexicon_declaration(raw: &Value) -> (LexiconDeclaration, Vec<String>) {
    let nsid = raw.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut converter = Converter {
        nsid,
        def: "main",
        warnings: Vec::new(),
    };
    let defs = raw.get("defs").and_then(Value::as_object);
    let main = defs.and_then(|defs| defs.get("main"));
    let main_type = main
        .and_then(|main| main.get("type"))
        .and_then(Value::as_str);
    let kind = main_type
        .filter(|t| MAIN_KINDS.contains(t))
        .unwrap_or("definitions");

    let main_def = main.and_then(|main| match kind {
        "query" => Some(MainDef {
            params: main.get("parameters").map(|p| converter.field(p)),
            output: main.get("output").map(|b| converter.body(b)),
            ..MainDef::default()
        }),
        "procedure" => Some(MainDef {
            params: main.get("parameters").map(|p| converter.field(p)),
            input: main.get("input").map(|b| converter.body(b)),
            output: main.get("output").map(|b| converter.body(b)),
            ..MainDef::default()
        }),
        "record" => Some(MainDef {
            record: main.get("record").map(|r| converter.field(r)),
            ..MainDef::default()
        }),
        _ => None,
    });

    let mut converted = BTreeMap::new();
    for (name, def) in defs.into_iter().flatten() {
        // A handler's main def is described by `main` above; a space's main
        // def describes a container, which no script receives.
        if name == "main" && kind != "definitions" {
            continue;
        }
        converter.def = name;
        let ty = if def.get("type").and_then(Value::as_str) == Some("token") {
            TypeExpr::Literal {
                value: Value::String(format!("{nsid}#{name}")),
            }
        } else {
            converter.field(def)
        };
        converted.insert(name.clone(), ty);
    }

    let description = raw
        .get("description")
        .or_else(|| main.and_then(|main| main.get("description")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let warnings = converter.warnings;
    (
        LexiconDeclaration {
            nsid: nsid.to_string(),
            kind: kind.to_string(),
            description,
            main: main_def,
            defs: converted,
        },
        warnings,
    )
}

struct Converter<'a> {
    nsid: &'a str,
    def: &'a str,
    warnings: Vec<String>,
}

impl Converter<'_> {
    /// A query's or procedure's `input`/`output`. Only a JSON body has a
    /// shape; anything else reaches a script as bytes.
    fn body(&mut self, body: &Value) -> TypeExpr {
        match body.get("encoding").and_then(Value::as_str) {
            Some("application/json") => match body.get("schema") {
                Some(schema) => self.field(schema),
                None => TypeExpr::Map {
                    values: Box::new(TypeExpr::Unknown),
                },
            },
            _ => TypeExpr::Bytes,
        }
    }

    fn field(&mut self, field: &Value) -> TypeExpr {
        let Some(kind) = field.get("type").and_then(Value::as_str) else {
            return self.unsupported("<none>");
        };
        match kind {
            "string" => match (field.get("const"), field.get("enum")) {
                (Some(value), _) => TypeExpr::Literal {
                    value: value.clone(),
                },
                (None, Some(Value::Array(values))) => literals(values),
                _ => TypeExpr::String {
                    format: text(field, "format"),
                    known_values: field
                        .get("knownValues")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                },
            },
            "integer" | "boolean" => match (field.get("const"), field.get("enum")) {
                (Some(value), _) => TypeExpr::Literal {
                    value: value.clone(),
                },
                (None, Some(Value::Array(values))) => literals(values),
                _ if kind == "integer" => TypeExpr::Integer,
                _ => TypeExpr::Boolean,
            },
            "bytes" => TypeExpr::Bytes,
            "cid-link" => TypeExpr::CidLink,
            "blob" => TypeExpr::Blob,
            "unknown" => TypeExpr::Map {
                values: Box::new(TypeExpr::Unknown),
            },
            "array" => TypeExpr::Array {
                items: Box::new(match field.get("items") {
                    Some(items) => self.field(items),
                    None => TypeExpr::Unknown,
                }),
            },
            "object" | "params" => TypeExpr::Object(self.object(field)),
            "ref" => TypeExpr::Ref {
                name: self.reference(field.get("ref").and_then(Value::as_str).unwrap_or("")),
            },
            "union" => TypeExpr::Union {
                members: field
                    .get("refs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|r| TypeExpr::Ref {
                        name: self.reference(r),
                    })
                    .collect(),
                open: !field
                    .get("closed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            other => self.unsupported(other),
        }
    }

    fn object(&mut self, field: &Value) -> ObjectType {
        let names = |key: &str| -> Vec<String> {
            field
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        };
        let required = names("required");
        let nullable = names("nullable");
        let properties = field
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, property)| {
                let mut ty = self.field(property);
                if nullable.contains(name) {
                    ty = ty.nullable();
                }
                Property {
                    name: name.clone(),
                    ty,
                    required: required.contains(name),
                    description: text(property, "description"),
                    default: property.get("default").cloned(),
                }
            })
            .collect();
        ObjectType { properties }
    }

    /// Every ref as `nsid#name`: `#x` is this lexicon's, and a bare NSID is
    /// that lexicon's `main`.
    fn reference(&self, reference: &str) -> String {
        if let Some(local) = reference.strip_prefix('#') {
            format!("{}#{local}", self.nsid)
        } else if reference.contains('#') {
            reference.to_string()
        } else {
            format!("{reference}#main")
        }
    }

    fn unsupported(&mut self, kind: &str) -> TypeExpr {
        self.warnings.push(format!(
            "{}#{}: unsupported lexicon type '{kind}'",
            self.nsid, self.def
        ));
        TypeExpr::Unknown
    }
}

fn literals(values: &[Value]) -> TypeExpr {
    TypeExpr::Union {
        members: values
            .iter()
            .map(|value| TypeExpr::Literal {
                value: value.clone(),
            })
            .collect(),
        open: false,
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(raw: Value) -> LexiconDeclaration {
        let (declaration, warnings) = lexicon_declaration(&raw);
        assert!(warnings.is_empty(), "{warnings:?}");
        declaration
    }

    fn property<'a>(ty: &'a TypeExpr, name: &str) -> &'a Property {
        let TypeExpr::Object(object) = ty else {
            panic!("not an object: {ty:?}")
        };
        object
            .properties
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no property {name} in {ty:?}"))
    }

    #[test]
    fn a_query_has_params_and_output() {
        let declaration = convert(json!({
            "lexicon": 1, "id": "com.example.getThing",
            "defs": {
                "main": {
                    "type": "query", "description": "Gets a thing",
                    "parameters": {"type": "params", "required": ["uri"], "properties": {
                        "uri": {"type": "string", "format": "at-uri"},
                        "sort": {"type": "string", "knownValues": ["new", "top"]},
                        "limit": {"type": "integer", "default": 50}
                    }},
                    "output": {"encoding": "application/json", "schema": {"type": "ref", "ref": "#view"}}
                },
                "view": {"type": "object", "properties": {"uri": {"type": "string"}}}
            }
        }));
        assert_eq!(declaration.kind, "query");
        assert_eq!(declaration.description.as_deref(), Some("Gets a thing"));
        let main = declaration.main.unwrap();
        let params = main.params.unwrap();
        let uri = property(&params, "uri");
        assert!(uri.required);
        assert_eq!(
            uri.ty,
            TypeExpr::String {
                format: Some("at-uri".into()),
                known_values: vec![]
            }
        );
        let sort = property(&params, "sort");
        assert!(!sort.required);
        assert_eq!(
            sort.ty,
            TypeExpr::String {
                format: None,
                known_values: vec!["new".into(), "top".into()]
            }
        );
        assert_eq!(property(&params, "limit").default, Some(json!(50)));
        assert_eq!(
            main.output,
            Some(TypeExpr::Ref {
                name: "com.example.getThing#view".into()
            })
        );
        assert!(main.input.is_none());
        assert!(matches!(declaration.defs["view"], TypeExpr::Object(_)));
        assert!(!declaration.defs.contains_key("main"));
    }

    #[test]
    fn a_body_without_a_schema_or_not_json() {
        let declaration = convert(json!({
            "lexicon": 1, "id": "com.example.upload",
            "defs": {"main": {
                "type": "procedure",
                "input": {"encoding": "*/*"},
                "output": {"encoding": "application/json"}
            }}
        }));
        let main = declaration.main.unwrap();
        assert_eq!(main.input, Some(TypeExpr::Bytes));
        assert_eq!(
            main.output,
            Some(TypeExpr::Map {
                values: Box::new(TypeExpr::Unknown)
            })
        );
        assert!(main.params.is_none());
    }

    #[test]
    fn a_record_honours_required_and_nullable() {
        let declaration = convert(json!({
            "lexicon": 1, "id": "com.example.thing",
            "defs": {"main": {
                "type": "record", "key": "tid",
                "record": {"type": "object", "required": ["title", "subtitle"], "nullable": ["subtitle"], "properties": {
                    "title": {"type": "string", "description": "What it is called"},
                    "subtitle": {"type": "string"},
                    "cover": {"type": "blob"},
                    "embed": {"type": "union", "refs": ["com.example.embed.image", "#link"]},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "extra": {"type": "unknown"},
                    "ref": {"type": "cid-link"}
                }}
            }}
        }));
        let record = declaration.main.unwrap().record.unwrap();
        let title = property(&record, "title");
        assert!(title.required);
        assert_eq!(title.description.as_deref(), Some("What it is called"));
        let subtitle = property(&record, "subtitle");
        assert!(subtitle.required && subtitle.ty.is_nullable());
        assert!(!property(&record, "cover").required);
        assert_eq!(property(&record, "cover").ty, TypeExpr::Blob);
        assert_eq!(
            property(&record, "embed").ty,
            TypeExpr::Union {
                members: vec![
                    TypeExpr::Ref {
                        name: "com.example.embed.image#main".into()
                    },
                    TypeExpr::Ref {
                        name: "com.example.thing#link".into()
                    },
                ],
                open: true
            }
        );
        assert_eq!(
            property(&record, "extra").ty,
            TypeExpr::Map {
                values: Box::new(TypeExpr::Unknown)
            }
        );
        assert_eq!(property(&record, "ref").ty, TypeExpr::CidLink);
    }

    #[test]
    fn definitions_keep_main_tokens_and_closed_unions() {
        let declaration = convert(json!({
            "lexicon": 1, "id": "com.example.defs",
            "defs": {
                "main": {"type": "object", "properties": {}},
                "featured": {"type": "token", "description": "A featured thing"},
                "choice": {"type": "union", "refs": ["#a", "#b"], "closed": true},
                "mode": {"type": "string", "enum": ["on", "off"]},
                "level": {"type": "integer", "enum": [1, 2]},
                "fixed": {"type": "boolean", "const": true}
            }
        }));
        assert_eq!(declaration.kind, "definitions");
        assert!(declaration.main.is_none());
        assert!(declaration.defs.contains_key("main"));
        assert_eq!(
            declaration.defs["featured"],
            TypeExpr::Literal {
                value: json!("com.example.defs#featured")
            }
        );
        assert!(matches!(
            declaration.defs["choice"],
            TypeExpr::Union { open: false, .. }
        ));
        assert_eq!(
            declaration.defs["mode"],
            TypeExpr::Union {
                members: vec![
                    TypeExpr::Literal { value: json!("on") },
                    TypeExpr::Literal {
                        value: json!("off")
                    }
                ],
                open: false
            }
        );
        assert!(matches!(declaration.defs["level"], TypeExpr::Union { .. }));
        assert_eq!(
            declaration.defs["fixed"],
            TypeExpr::Literal { value: json!(true) }
        );
    }

    #[test]
    fn a_space_keeps_its_other_defs_only() {
        let declaration = convert(json!({
            "lexicon": 1, "id": "com.example.space",
            "defs": {"main": {"type": "space"}, "member": {"type": "object", "properties": {}}}
        }));
        assert_eq!(declaration.kind, "space");
        assert!(declaration.main.is_none());
        assert_eq!(declaration.defs.keys().collect::<Vec<_>>(), ["member"]);
    }

    #[test]
    fn an_unknown_field_type_is_unknown_with_a_warning() {
        let (declaration, warnings) = lexicon_declaration(&json!({
            "lexicon": 1, "id": "com.example.odd",
            "defs": {"main": {"type": "record", "record": {"type": "object", "properties": {
                "when": {"type": "datetime"}
            }}}}
        }));
        assert_eq!(
            property(&declaration.main.unwrap().record.unwrap(), "when").ty,
            TypeExpr::Unknown
        );
        assert_eq!(
            warnings,
            ["com.example.odd#main: unsupported lexicon type 'datetime'"]
        );
    }

    /// Copied from atproto: a record with a union embed, facets by ref, and
    /// a languages array.
    #[test]
    fn app_bsky_feed_post_converts_without_warnings() {
        let declaration = convert(json!({
          "lexicon": 1, "id": "app.bsky.feed.post",
          "defs": {
            "main": {"type": "record", "description": "Record containing a Bluesky post.", "key": "tid",
              "record": {"type": "object", "required": ["text", "createdAt"], "properties": {
                "text": {"type": "string", "maxLength": 3000, "maxGraphemes": 300},
                "facets": {"type": "array", "items": {"type": "ref", "ref": "app.bsky.richtext.facet"}},
                "reply": {"type": "ref", "ref": "#replyRef"},
                "embed": {"type": "union", "refs": ["app.bsky.embed.images", "app.bsky.embed.video", "app.bsky.embed.external", "app.bsky.embed.record", "app.bsky.embed.recordWithMedia"]},
                "langs": {"type": "array", "maxLength": 3, "items": {"type": "string", "format": "language"}},
                "labels": {"type": "union", "refs": ["com.atproto.label.defs#selfLabels"]},
                "tags": {"type": "array", "maxLength": 8, "items": {"type": "string", "maxLength": 640, "maxGraphemes": 64}},
                "createdAt": {"type": "string", "format": "datetime"}
              }}},
            "replyRef": {"type": "object", "required": ["root", "parent"], "properties": {
              "root": {"type": "ref", "ref": "com.atproto.repo.strongRef"},
              "parent": {"type": "ref", "ref": "com.atproto.repo.strongRef"}}},
            "entity": {"type": "object", "description": "Deprecated", "required": ["index", "type", "value"], "properties": {
              "index": {"type": "ref", "ref": "#textSlice"}, "type": {"type": "string"}, "value": {"type": "string"}}},
            "textSlice": {"type": "object", "required": ["start", "end"], "properties": {
              "start": {"type": "integer", "minimum": 0}, "end": {"type": "integer", "minimum": 0}}}
          }
        }));
        assert_eq!(declaration.kind, "record");
        assert_eq!(
            declaration.description.as_deref(),
            Some("Record containing a Bluesky post.")
        );
        let record = declaration.main.unwrap().record.unwrap();
        assert_eq!(
            property(&record, "reply").ty,
            TypeExpr::Ref {
                name: "app.bsky.feed.post#replyRef".into()
            }
        );
        assert_eq!(
            property(&declaration.defs["replyRef"], "root").ty,
            TypeExpr::Ref {
                name: "com.atproto.repo.strongRef#main".into()
            }
        );
        assert_eq!(declaration.defs.len(), 3);
    }
}
