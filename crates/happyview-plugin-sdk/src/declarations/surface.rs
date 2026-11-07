//! A library's [`ApiSurface`] read into the type model, and refused with a
//! sentence saying where when it does not read.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::grammar::union_of;
use super::{parse_type, ObjectType, Property, TypeExpr};
use crate::wire::{ApiExport, ApiMethod, ApiSurface};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedSurface {
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub exports: Vec<TypedExport>,
    #[serde(default)]
    pub types: Vec<NamedType>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedExport {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub params: Vec<Property>,
    /// `None` when the library did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeExpr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<TypedMethod>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedMethod {
    pub name: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub params: Vec<Property>,
    /// `None` when the library did not say. A lazy method that does not say
    /// returns the object it was called on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedType {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub ty: TypeExpr,
}

/// Where in a surface something failed to read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceError {
    /// `export 'records', method 'where', param 'field'`.
    pub location: String,
    pub message: String,
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

pub fn parse_surface(surface: &ApiSurface) -> Result<TypedSurface, SurfaceError> {
    let mut reader = Reader {
        names: BTreeSet::new(),
    };
    for entry in &surface.types {
        if let Some(name) = entry.get("name").and_then(Value::as_str) {
            reader.names.insert(name.to_string());
        }
    }
    let types = surface
        .types
        .iter()
        .enumerate()
        .map(|(i, entry)| reader.named_type(i, entry))
        .collect::<Result<Vec<_>, _>>()?;
    let exports = surface
        .exports
        .iter()
        .map(|export| reader.export(export))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TypedSurface {
        namespace: surface.namespace.clone(),
        description: surface.description.clone(),
        exports,
        types,
    })
}

/// Panics with the [`SurfaceError`] sentence when `surface` does not parse,
/// for a plugin's own tests: `assert_surface_parses(&surface())`.
#[track_caller]
pub fn assert_surface_parses(surface: &ApiSurface) {
    if let Err(e) = parse_surface(surface) {
        panic!("the {} surface does not parse: {e}", surface.namespace);
    }
}

struct Reader {
    names: BTreeSet<String>,
}

impl Reader {
    fn named_type(&self, index: usize, entry: &Value) -> Result<NamedType, SurfaceError> {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            return Err(SurfaceError {
                location: format!("types[{index}]"),
                message: "a named type needs a name".into(),
            });
        };
        let location = format!("type '{name}'");
        let ty = match entry.get("type") {
            Some(ty) => self.type_at(ty, &location)?,
            None => self.type_at(entry, &location)?,
        };
        Ok(NamedType {
            name: name.to_string(),
            description: text(entry, "description"),
            ty,
        })
    }

    fn export(&self, export: &ApiExport) -> Result<TypedExport, SurfaceError> {
        let location = format!("export '{}'", export.name);
        Ok(TypedExport {
            name: export.name.clone(),
            kind: export.kind.clone(),
            description: export.description.clone(),
            params: self.params(&export.params, &location)?,
            returns: self.returns(export.returns.as_ref(), &location)?,
            methods: export
                .methods
                .iter()
                .map(|method| self.method(method, &location))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    fn method(&self, method: &ApiMethod, outer: &str) -> Result<TypedMethod, SurfaceError> {
        let location = format!("{outer}, method '{}'", method.name);
        Ok(TypedMethod {
            name: method.name.clone(),
            mode: method.mode.clone(),
            description: method.description.clone(),
            params: self.params(&method.params, &location)?,
            returns: self.returns(method.returns.as_ref(), &location)?,
        })
    }

    fn returns(
        &self,
        returns: Option<&Value>,
        location: &str,
    ) -> Result<Option<TypeExpr>, SurfaceError> {
        returns
            .map(|ty| self.type_at(ty, &format!("{location}, returns")))
            .transpose()
    }

    fn params(&self, params: &[Value], location: &str) -> Result<Vec<Property>, SurfaceError> {
        params
            .iter()
            .enumerate()
            .map(|(i, param)| self.property(param, location, "param", i))
            .collect()
    }

    fn property(
        &self,
        value: &Value,
        outer: &str,
        word: &str,
        index: usize,
    ) -> Result<Property, SurfaceError> {
        let Some(name) = value.get("name").and_then(Value::as_str) else {
            return Err(SurfaceError {
                location: format!("{outer}, {word} {}", index + 1),
                message: format!("a {word} needs a name"),
            });
        };
        let location = format!("{outer}, {word} '{name}'");
        let ty = if value.is_object() {
            self.type_at(value, &location)?
        } else {
            TypeExpr::Unknown
        };
        let required = value
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(!ty.is_nullable());
        Ok(Property {
            name: name.to_string(),
            ty,
            required,
            description: text(value, "description"),
            default: value.get("default").cloned(),
        })
    }

    /// A type position: a type string, or one of the object forms published
    /// plugins use — `{"type": ..., "properties": [...]}`, `{"type": "array",
    /// "items": ...}`, `{"anyOf": [...]}` and `{"type": {...}}`. An object
    /// that names no type is `unknown`.
    fn type_at(&self, value: &Value, location: &str) -> Result<TypeExpr, SurfaceError> {
        match value {
            Value::String(text) => {
                let ty = parse_type(text).map_err(|e| SurfaceError {
                    location: location.to_string(),
                    message: format!("'{text}': {e}"),
                })?;
                self.check_refs(&ty, location)?;
                Ok(ty)
            }
            Value::Object(map) => self.object_form(map, location),
            other => Err(SurfaceError {
                location: location.to_string(),
                message: format!("a type is a string or an object, not {other}"),
            }),
        }
    }

    fn object_form(
        &self,
        map: &Map<String, Value>,
        location: &str,
    ) -> Result<TypeExpr, SurfaceError> {
        if let Some(any_of) = map.get("anyOf") {
            let Some(members) = any_of.as_array() else {
                return Err(SurfaceError {
                    location: location.to_string(),
                    message: "anyOf must be a list of types".into(),
                });
            };
            let members = members
                .iter()
                .map(|member| self.type_at(member, location))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(union_of(members));
        }
        match map.get("type") {
            Some(Value::String(text)) => {
                let base = text.trim_end_matches('?').trim();
                let nullable = base.len() < text.trim().len();
                let ty = match base {
                    "object" if map.contains_key("properties") => {
                        TypeExpr::Object(self.object(map, location)?)
                    }
                    "array" if map.contains_key("items") => TypeExpr::Array {
                        items: Box::new(self.type_at(&map["items"], location)?),
                    },
                    _ => return self.type_at(&Value::String(text.clone()), location),
                };
                Ok(if nullable { ty.nullable() } else { ty })
            }
            Some(inner @ Value::Object(_)) => self.type_at(inner, location),
            Some(other) => Err(SurfaceError {
                location: location.to_string(),
                message: format!("a type is a string or an object, not {other}"),
            }),
            None if map.contains_key("properties") => {
                Ok(TypeExpr::Object(self.object(map, location)?))
            }
            None => Ok(TypeExpr::Unknown),
        }
    }

    fn object(&self, map: &Map<String, Value>, location: &str) -> Result<ObjectType, SurfaceError> {
        let Some(properties) = map.get("properties").and_then(Value::as_array) else {
            return Err(SurfaceError {
                location: location.to_string(),
                message: "properties must be a list of {name, type}".into(),
            });
        };
        Ok(ObjectType {
            properties: properties
                .iter()
                .enumerate()
                .map(|(i, property)| self.property(property, location, "property", i))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    fn check_refs(&self, ty: &TypeExpr, location: &str) -> Result<(), SurfaceError> {
        match ty {
            TypeExpr::Ref { name } if !self.names.contains(name) => Err(SurfaceError {
                location: location.to_string(),
                message: format!("'{name}' names no type in this surface's types"),
            }),
            TypeExpr::Array { items } => self.check_refs(items, location),
            TypeExpr::Map { values } => self.check_refs(values, location),
            TypeExpr::Union { members, .. } => members
                .iter()
                .try_for_each(|member| self.check_refs(member, location)),
            TypeExpr::Object(object) => object
                .properties
                .iter()
                .try_for_each(|property| self.check_refs(&property.ty, location)),
            _ => Ok(()),
        }
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(surface: &ApiSurface) -> String {
        parse_surface(surface).unwrap_err().to_string()
    }

    /// The `sdk_http` fixture's surface, as published.
    #[test]
    fn the_http_surface_parses() {
        let surface = ApiSurface::new("http")
            .export(
                ApiExport::function("get")
                    .param("url", "string", "Target URL")
                    .param_json(json!({"name": "opts", "type": "object?", "description": "Request options", "properties": [
                        {"name": "headers", "type": "object?", "description": "Header name to value"},
                        {"name": "body", "type": "string?"}
                    ]}))
                    .returns(json!({"type": "object", "properties": [
                        {"name": "status", "type": "integer"},
                        {"name": "body", "type": "string|object"}
                    ]})),
            )
            .export(
                ApiExport::function("allowed_hosts")
                    .returns(json!({"type": "array", "items": {"type": "string"}})),
            );
        let typed = parse_surface(&surface).unwrap();
        let opts = &typed.exports[0].params[1];
        assert!(!opts.required);
        assert!(opts.ty.is_nullable());
        let TypeExpr::Union { members, .. } = &opts.ty else {
            panic!("{:?}", opts.ty)
        };
        let TypeExpr::Object(object) = &members[0] else {
            panic!("{members:?}")
        };
        assert_eq!(object.properties[0].name, "headers");
        assert!(!object.properties[0].required);
        assert_eq!(
            typed.exports[1].returns,
            Some(TypeExpr::Array {
                items: Box::new(TypeExpr::string())
            })
        );
        assert!(typed.exports[0].params[0].required);
    }

    #[test]
    fn any_of_is_a_union() {
        let surface =
            ApiSurface::new("spaces").export(ApiExport::function("get").returns(
                json!({"anyOf": [{"type": "object", "properties": []}, {"type": "null"}]}),
            ));
        let returns = parse_surface(&surface).unwrap().exports[0]
            .returns
            .clone()
            .unwrap();
        assert!(returns.is_nullable());
    }

    #[test]
    fn chain_methods_carry_signatures() {
        let surface = ApiSurface::new("db")
            .export(
                ApiExport::constructor("records")
                    .param("collection", "string", "NSID")
                    .method(
                        ApiMethod::lazy("where")
                            .describe("Filter")
                            .param("field", "string", "")
                            .returns(json!("Query")),
                    )
                    .immediate("run"),
            )
            .named_type("Query", "A query in progress", json!("object"));
        let typed = parse_surface(&surface).unwrap();
        let methods = &typed.exports[0].methods;
        assert_eq!(methods[0].params[0].name, "field");
        assert_eq!(methods[0].description.as_deref(), Some("Filter"));
        assert_eq!(
            methods[0].returns,
            Some(TypeExpr::Ref {
                name: "Query".into()
            })
        );
        assert_eq!(methods[1].returns, None);
        assert_eq!(typed.types[0].name, "Query");
        assert_eq!(
            typed.types[0].description.as_deref(),
            Some("A query in progress")
        );
    }

    #[test]
    fn a_named_type_can_be_an_inline_object() {
        let surface = ApiSurface::new("db")
            .export(ApiExport::function("get").returns(json!("Record?")))
            .named_type(
                "Record",
                "",
                json!({"type": "object", "properties": [{"name": "uri", "type": "string"}]}),
            );
        let typed = parse_surface(&surface).unwrap();
        assert!(matches!(typed.types[0].ty, TypeExpr::Object(_)));
        assert_eq!(typed.types[0].description, None);
    }

    #[test]
    fn a_ref_must_name_a_type() {
        let surface =
            ApiSurface::new("db").export(ApiExport::function("get").returns(json!("Record")));
        assert_eq!(
            error(&surface),
            "export 'get', returns: 'Record' names no type in this surface's types"
        );
        let nested = ApiSurface::new("db").export(ApiExport::function("get").param_json(
            json!({"name": "opts", "type": "object", "properties": [{"name": "sort", "type": "Sort[]"}]}),
        ));
        assert_eq!(
            error(&nested),
            "export 'get', param 'opts', property 'sort': 'Sort' names no type in this surface's types"
        );
    }

    #[test]
    fn a_bad_string_says_where_it_is() {
        let surface = ApiSurface::new("db").export(ApiExport::function("get").param(
            "limit",
            "integer |",
            "",
        ));
        assert_eq!(
            error(&surface),
            "export 'get', param 'limit': 'integer |': expected a type at column 10"
        );
        let method = ApiSurface::new("db").export(
            ApiExport::constructor("records")
                .method(ApiMethod::lazy("where").param("op", "'=", "")),
        );
        assert_eq!(
            error(&method),
            "export 'records', method 'where', param 'op': ''=': unclosed string literal at column 1"
        );
    }

    #[test]
    fn a_param_needs_a_name() {
        let surface = ApiSurface::new("x")
            .export(ApiExport::function("f").param_json(json!({"type": "string"})));
        assert_eq!(error(&surface), "export 'f', param 1: a param needs a name");
    }

    #[test]
    fn required_follows_the_question_mark_unless_stated() {
        let surface = ApiSurface::new("x").export(
            ApiExport::function("f")
                .param("a", "string?", "")
                .param("b", "string", "")
                .param_json(json!({"name": "c", "type": "string", "required": false}))
                .param_json(json!({"name": "d", "type": "integer?", "default": 10})),
        );
        let params = &parse_surface(&surface).unwrap().exports[0].params;
        let required: Vec<bool> = params.iter().map(|p| p.required).collect();
        assert_eq!(required, [false, true, false, false]);
        assert_eq!(params[3].default, Some(json!(10)));
        assert_eq!(params[0].description, None);
    }

    #[test]
    fn missing_returns_and_types_stay_missing() {
        let surface = ApiSurface::new("x")
            .export(ApiExport::function("backend").param_json(json!({"name": "opts"})));
        let typed = parse_surface(&surface).unwrap();
        assert_eq!(typed.exports[0].returns, None);
        assert_eq!(typed.exports[0].params[0].ty, TypeExpr::Unknown);
    }

    #[test]
    #[should_panic(expected = "the db surface does not parse: export 'get', returns")]
    fn the_assertion_names_the_surface() {
        assert_surface_parses(
            &ApiSurface::new("db").export(ApiExport::function("get").returns(json!("Nope"))),
        );
    }
}
