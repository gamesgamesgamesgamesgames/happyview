//! What `get_declarations` receives and returns.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use super::{ObjectType, TypeExpr, TypedSurface};

/// Everything a script can see, as types: the installed libraries, the
/// project's lexicons, and `ctx` and the event inputs per script kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeclarationsInput {
    #[serde(default)]
    pub libraries: Vec<LibraryDeclaration>,
    #[serde(default)]
    pub lexicons: Vec<LexiconDeclaration>,
    #[serde(default)]
    pub ctx: CtxShapes,
    #[serde(default)]
    pub events: EventShapes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryDeclaration {
    /// What a script requires it by, after the `happyview.` prefix.
    pub namespace: String,
    pub surface: TypedSurface,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexiconDeclaration {
    pub nsid: String,
    /// `query`, `procedure`, `record`, `space` or `definitions`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The `main` def of a query, procedure or record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<MainDef>,
    /// Every other def, by name. A [`TypeExpr::Ref`] names one of these, in
    /// this or another lexicon, as `nsid#name`.
    #[serde(default)]
    pub defs: BTreeMap<String, TypeExpr>,
}

/// What a handler for this lexicon receives and returns. Which fields are set
/// follows the lexicon's kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MainDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<TypeExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<TypeExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<TypeExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<TypeExpr>,
}

/// `ctx` per script kind: the data HappyView sets. Functions an interpreter
/// adds itself, such as `ctx.job.progress`, are the interpreter's to declare.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CtxShapes {
    pub query: ObjectType,
    pub procedure: ObjectType,
    pub record: ObjectType,
    pub label: ObjectType,
    pub job: ObjectType,
    pub test: ObjectType,
}

/// The `input` of the scripts whose input is an event rather than a lexicon's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventShapes {
    /// Its `record` property is [`TypeExpr::Var`] `record`: the record
    /// lexicon the handler is for.
    pub record: ObjectType,
    pub label: ObjectType,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Declarations {
    #[serde(default)]
    pub files: Vec<DeclarationFile>,
}

/// One generated file. `path` is relative to the interpreter's own output
/// directory and uses `/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclarationFile {
    pub path: String,
    pub contents: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declarations::Property;
    use alloc::boxed::Box;
    use serde_json::json;

    #[test]
    fn the_input_round_trips() {
        let input = DeclarationsInput {
            libraries: Vec::from([LibraryDeclaration {
                namespace: "db".into(),
                surface: TypedSurface {
                    namespace: "db".into(),
                    description: None,
                    exports: Vec::new(),
                    types: Vec::new(),
                },
            }]),
            lexicons: Vec::from([LexiconDeclaration {
                nsid: "com.example.getThing".into(),
                kind: "query".into(),
                description: Some("Gets a thing".into()),
                main: Some(MainDef {
                    params: Some(TypeExpr::Object(ObjectType {
                        properties: Vec::from([Property::new("uri", TypeExpr::string())]),
                    })),
                    output: Some(TypeExpr::Ref {
                        name: "com.example.getThing#view".into(),
                    }),
                    ..MainDef::default()
                }),
                defs: BTreeMap::from([(
                    "view".into(),
                    TypeExpr::Array {
                        items: Box::new(TypeExpr::CidLink),
                    },
                )]),
            }]),
            ctx: CtxShapes::default(),
            events: EventShapes {
                record: ObjectType {
                    properties: Vec::from([Property::new(
                        "record",
                        TypeExpr::Var {
                            name: "record".into(),
                        },
                    )]),
                },
                label: ObjectType::default(),
            },
        };
        let wire = serde_json::to_value(&input).unwrap();
        assert_eq!(
            wire["lexicons"][0]["main"]["output"],
            json!({"kind": "ref", "name": "com.example.getThing#view"})
        );
        assert_eq!(
            wire["lexicons"][0]["defs"]["view"],
            json!({"kind": "array", "items": {"kind": "cid-link"}})
        );
        let back: DeclarationsInput = serde_json::from_value(wire).unwrap();
        assert_eq!(back, input);
    }
}
