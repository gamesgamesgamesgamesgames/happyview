//! What an editor needs to know about a script's world, in one structured
//! model: library surfaces, lexicons and `ctx`. The host builds it and an
//! interpreter's `get_declarations` renders it into its language's own
//! declaration files, so a library describes itself once and every language
//! gets types for it.

mod grammar;
mod input;
mod surface;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use grammar::{parse_type, print_type, TypeError};
pub use input::{
    CtxShapes, DeclarationFile, Declarations, DeclarationsInput, EventShapes, LexiconDeclaration,
    LibraryDeclaration, MainDef,
};
pub use surface::{
    assert_surface_parses, parse_surface, NamedType, SurfaceError, TypedExport, TypedMethod,
    TypedSurface,
};

/// A type, as a surface's type strings and a lexicon's field definitions both
/// describe one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TypeExpr {
    String {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        /// Values a lexicon suggests without excluding others.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        known_values: Vec<String>,
    },
    Integer,
    Number,
    Boolean,
    Bytes,
    Null,
    Unknown,
    Any,
    Blob,
    CidLink,
    Literal {
        value: Value,
    },
    Array {
        items: Box<TypeExpr>,
    },
    /// An object whose keys are not known: `object` alone, or a lexicon's
    /// `unknown`.
    Map {
        values: Box<TypeExpr>,
    },
    Object(ObjectType),
    Union {
        members: Vec<TypeExpr>,
        /// A lexicon union that admits types it does not list.
        #[serde(default)]
        open: bool,
    },
    /// A named type: a surface's `types` entry, or a lexicon def as
    /// `nsid#name`.
    Ref {
        name: String,
    },
    /// A type parameter, such as the record a record event carries.
    Var {
        name: String,
    },
}

impl TypeExpr {
    pub fn string() -> Self {
        TypeExpr::String {
            format: None,
            known_values: Vec::new(),
        }
    }

    /// `self | null`, flattened into an existing union and never listing
    /// `null` twice.
    pub fn nullable(self) -> Self {
        match self {
            TypeExpr::Null => TypeExpr::Null,
            TypeExpr::Union { mut members, open } => {
                if !members.contains(&TypeExpr::Null) {
                    members.push(TypeExpr::Null);
                }
                TypeExpr::Union { members, open }
            }
            other => TypeExpr::Union {
                members: Vec::from([other, TypeExpr::Null]),
                open: false,
            },
        }
    }

    pub fn is_nullable(&self) -> bool {
        match self {
            TypeExpr::Null => true,
            TypeExpr::Union { members, .. } => members.contains(&TypeExpr::Null),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectType {
    #[serde(default)]
    pub properties: Vec<Property>,
}

/// A named member of an object, or a parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeExpr,
    /// False when the caller may leave it out.
    #[serde(default = "yes")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

impl Property {
    pub fn new(name: impl Into<String>, ty: TypeExpr) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
            description: None,
            default: None,
        }
    }

    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

fn yes() -> bool {
    true
}
