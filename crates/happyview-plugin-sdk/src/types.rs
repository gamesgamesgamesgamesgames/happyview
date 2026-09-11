//! The structured values a plugin exchanges with the host: its identity, the
//! call envelope, and the API surface a library advertises to scripts.

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What `plugin_info` returns. Mirrors the host's `PluginInfo`.
///
/// `icon_url` and `config_schema` are serialised even when absent, because the
/// host's struct gives neither a serde default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: String,
    pub icon_url: Option<String>,
    #[serde(default)]
    pub required_secrets: Vec<String>,
    /// `"oauth2"`, `"openid"` or `"api_key"`. Only auth plugins read it.
    #[serde(default = "default_auth_type")]
    pub auth_type: String,
    pub config_schema: Option<Value>,
}

fn default_auth_type() -> String {
    String::from("oauth2")
}

impl PluginInfo {
    /// A v2 plugin with no secrets, no icon and no config schema.
    pub fn new(id: impl Into<String>, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            api_version: String::from("2"),
            icon_url: None,
            required_secrets: Vec::new(),
            auth_type: default_auth_type(),
            config_schema: None,
        }
    }

    pub fn api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }

    pub fn icon_url(mut self, url: impl Into<String>) -> Self {
        self.icon_url = Some(url.into());
        self
    }

    /// Environment-variable names the operator must supply, e.g. `PLUGIN_STEAM_API_KEY`.
    pub fn required_secrets<I, S>(mut self, secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_secrets = secrets.into_iter().map(Into::into).collect();
        self
    }

    pub fn auth_type(mut self, auth_type: impl Into<String>) -> Self {
        self.auth_type = auth_type.into();
        self
    }

    pub fn config_schema(mut self, schema: Value) -> Self {
        self.config_schema = Some(schema);
        self
    }
}

/// Who a call acts as. Threaded from the script runner and never widened by a
/// library, so a plugin may narrow what it does but never claim more.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallContext {
    #[serde(default)]
    pub caller_did: Option<String>,
    #[serde(default)]
    pub has_pds_auth: bool,
}

/// The `call` export's input. Every field but `function` defaults, so an
/// abbreviated envelope still dispatches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallInput {
    pub function: String,
    #[serde(default)]
    pub args: Vec<Value>,
    #[serde(default)]
    pub context: CallContext,
}

/// What `get_api_surface` returns: everything an interpreter needs to render
/// this library into its own idiom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiSurface {
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub exports: Vec<ApiExport>,
    /// Named types the exports refer to. Opaque to the host.
    #[serde(default)]
    pub types: Vec<Value>,
}

impl ApiSurface {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            import_path: None,
            description: None,
            exports: Vec::new(),
            types: Vec::new(),
        }
    }

    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn import_path(mut self, path: impl Into<String>) -> Self {
        self.import_path = Some(path.into());
        self
    }

    pub fn export(mut self, export: ApiExport) -> Self {
        self.exports.push(export);
        self
    }

    pub fn exports<I: IntoIterator<Item = ApiExport>>(mut self, exports: I) -> Self {
        self.exports.extend(exports);
        self
    }

    pub fn types<I: IntoIterator<Item = Value>>(mut self, types: I) -> Self {
        self.types.extend(types);
        self
    }
}

/// One entry in an `ApiSurface`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiExport {
    pub name: String,
    /// `"function"` or `"constant"`; interpreters may agree on others.
    #[serde(default = "default_export_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Value>,
}

fn default_export_kind() -> String {
    String::from("function")
}

impl ApiExport {
    pub fn function(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: default_export_kind(),
            description: None,
            params: Vec::new(),
            returns: None,
        }
    }

    pub fn constant(name: impl Into<String>) -> Self {
        Self {
            kind: String::from("constant"),
            ..Self::function(name)
        }
    }

    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// A plain `{name, type, description}` parameter.
    pub fn param(self, name: &str, ty: &str, description: &str) -> Self {
        self.param_json(serde_json::json!({
            "name": name,
            "type": ty,
            "description": description,
        }))
    }

    /// A parameter that needs more than name, type and description — nested
    /// `properties`, say.
    pub fn param_json(mut self, param: Value) -> Self {
        self.params.push(param);
        self
    }

    pub fn returns(mut self, returns: Value) -> Self {
        self.returns = Some(returns);
        self
    }
}

/// A strong reference to an AT Protocol record, as `host_lookup_record` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrongRef {
    pub uri: String,
    pub cid: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_input_defaults_every_field_but_function() {
        let input: CallInput = serde_json::from_str(r#"{"function":"get"}"#).unwrap();
        assert_eq!(input.function, "get");
        assert!(input.args.is_empty());
        assert_eq!(input.context, CallContext::default());
        assert_eq!(input.context.caller_did, None);
        assert!(!input.context.has_pds_auth);
    }

    #[test]
    fn call_input_context_fields_default_independently() {
        let input: CallInput =
            serde_json::from_str(r#"{"function":"f","args":[1],"context":{"has_pds_auth":true}}"#)
                .unwrap();
        assert_eq!(input.args, alloc::vec![Value::from(1)]);
        assert_eq!(input.context.caller_did, None);
        assert!(input.context.has_pds_auth);
    }

    #[test]
    fn plugin_info_serialises_absent_optionals_as_null() {
        let json = serde_json::to_value(PluginInfo::new("http", "HTTP Client", "1.0.0")).unwrap();
        assert_eq!(json["api_version"], "2");
        assert_eq!(json["auth_type"], "oauth2");
        assert_eq!(json["icon_url"], Value::Null);
        assert_eq!(json["config_schema"], Value::Null);
        assert_eq!(json["required_secrets"], serde_json::json!([]));
    }

    #[test]
    fn api_surface_builder_matches_the_host_shape() {
        let surface = ApiSurface::new("http")
            .describe("Outbound HTTP requests")
            .export(
                ApiExport::function("get")
                    .describe("Send a GET request")
                    .param("url", "string", "Target URL")
                    .returns(serde_json::json!({"type": "object"})),
            );
        let json = serde_json::to_value(&surface).unwrap();
        assert_eq!(json["namespace"], "http");
        assert!(json.get("import_path").is_none(), "absent, not null");
        assert_eq!(json["exports"][0]["kind"], "function");
        assert_eq!(json["exports"][0]["params"][0]["name"], "url");
        assert_eq!(json["types"], serde_json::json!([]));
    }
}
