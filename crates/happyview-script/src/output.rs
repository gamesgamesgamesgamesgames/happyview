use happyview_plugin_sdk::{ScriptValueKind, Value};
use serde::Serialize;

use crate::Result;

/// What a handler returned, as the host reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub value: Value,
    pub kind: ScriptValueKind,
}

/// A handler's return type. `()` and `None` are "returned nothing", which a
/// record handler uses to skip indexing; `Value::Null` is an explicit null.
pub trait IntoOutput {
    fn into_output(self) -> Result<Output>;
}

impl IntoOutput for Value {
    fn into_output(self) -> Result<Output> {
        let kind = if self.is_object() {
            ScriptValueKind::Object
        } else {
            ScriptValueKind::Other
        };
        Ok(Output { value: self, kind })
    }
}

impl IntoOutput for () {
    fn into_output(self) -> Result<Output> {
        Ok(Output {
            value: Value::Null,
            kind: ScriptValueKind::None,
        })
    }
}

impl IntoOutput for Option<Value> {
    fn into_output(self) -> Result<Output> {
        match self {
            Some(value) => value.into_output(),
            None => ().into_output(),
        }
    }
}

/// Any serializable value as a handler's answer.
pub struct Json<T>(pub T);

impl<T: Serialize> IntoOutput for Json<T> {
    fn into_output(self) -> Result<Output> {
        serde_json::to_value(self.0)?.into_output()
    }
}
