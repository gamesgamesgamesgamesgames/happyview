//! The structured values a plugin exchanges with the host: its identity, the
//! call envelope, the API surface a library advertises to scripts, and the
//! inputs and outputs of an auth plugin's five exports.

use alloc::collections::BTreeMap;
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

    /// Environment-variable names the operator must supply, e.g.
    /// `PLUGIN_STEAM_API_KEY`. The prefix is `PLUGIN_`, the plugin id
    /// upper-cased with every non-alphanumeric character replaced by `_`, then
    /// `_` — so `auth-steam` reads `PLUGIN_AUTH_STEAM_API_KEY`. A plugin asks
    /// the host for the part after that prefix: `host::get_secret("API_KEY")`.
    pub fn required_secrets<I, S>(mut self, secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_secrets = secrets.into_iter().map(Into::into).collect();
        self
    }

    /// How this plugin authenticates: `"oauth2"`, `"openid"` or `"api_key"`.
    /// Only an [`auth_plugin!`](crate::auth_plugin) sets it; a library or an
    /// interpreter leaves the default alone, because nothing reads it there.
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

/// What `get_authorize_url` receives. `config` is the operator's plugin
/// configuration, passed through untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuthorizeUrlInput {
    /// Opaque CSRF state the host minted. Put it in the provider's URL
    /// unchanged — the host matches it when the callback lands.
    pub state: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub config: Value,
}

/// What `handle_callback` receives.
///
/// The host flattens *every* callback query parameter at the top level beside
/// `config`, because the shape differs by protocol: OAuth 2.0 sends `code` and
/// `state`, OpenID 2.0 sends a dozen `openid.*` keys. So the parameters are a
/// map rather than named fields, and [`param`](Self::param) reads one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallbackInput {
    #[serde(default)]
    pub config: Value,
    /// Every query parameter the provider sent back. The host sends these as
    /// strings; the type stays [`Value`] so a non-string value does not fail
    /// the whole callback.
    #[serde(flatten)]
    pub params: BTreeMap<String, Value>,
}

impl CallbackInput {
    /// One callback parameter, if it was sent as a string.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.get(name).and_then(Value::as_str)
    }
}

/// What `refresh_tokens` receives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RefreshInput {
    pub refresh_token: String,
    #[serde(default)]
    pub config: Value,
}

/// What `get_profile` receives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenInput {
    pub access_token: String,
    #[serde(default)]
    pub config: Value,
}

/// What `handle_callback` and `refresh_tokens` return.
///
/// `expires_at` is an RFC 3339 timestamp: the host parses it into a
/// `chrono::DateTime<Utc>`, so a string it cannot parse fails the whole call.
/// Absent fields are omitted rather than sent as null, matching the host.
///
/// Prefer [`expires_in`](Self::expires_in): a guest has no clock, so it cannot
/// turn a provider's duration (what Microsoft and Xbox return) into an absolute
/// timestamp. Use `expires_at` only when the provider gives an absolute instant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Seconds until expiry. The host computes an absolute expiry from this
    /// when `expires_at` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    pub token_type: String,
}

impl TokenSet {
    /// A token set with no refresh token and no expiry, e.g.
    /// `TokenSet::new(access, "Bearer")`.
    pub fn new(access_token: impl Into<String>, token_type: impl Into<String>) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: None,
            expires_at: None,
            expires_in: None,
            token_type: token_type.into(),
        }
    }

    pub fn refresh_token(mut self, refresh_token: impl Into<String>) -> Self {
        self.refresh_token = Some(refresh_token.into());
        self
    }

    /// An RFC 3339 instant, e.g. `2026-01-01T00:00:00Z`. Prefer
    /// [`expires_in`](Self::expires_in) unless the provider itself gives an
    /// absolute timestamp.
    pub fn expires_at(mut self, expires_at: impl Into<String>) -> Self {
        self.expires_at = Some(expires_at.into());
        self
    }

    /// Seconds until the token expires, e.g. `TokenSet::new(access,
    /// "Bearer").expires_in(3600)`. Forward the provider's value as-is; the host
    /// computes the absolute expiry.
    pub fn expires_in(mut self, seconds: u64) -> Self {
        self.expires_in = Some(seconds);
        self
    }
}

/// What `get_profile` returns: who the access token belongs to on the
/// provider's side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalProfile {
    /// The provider's own stable id for the account. HappyView keys the link
    /// on it, so it must not change between calls.
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

impl ExternalProfile {
    pub fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            display_name: None,
            profile_url: None,
            avatar_url: None,
        }
    }

    pub fn display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    pub fn profile_url(mut self, profile_url: impl Into<String>) -> Self {
        self.profile_url = Some(profile_url.into());
        self
    }

    pub fn avatar_url(mut self, avatar_url: impl Into<String>) -> Self {
        self.avatar_url = Some(avatar_url.into());
        self
    }
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

    #[test]
    fn authorize_url_input_matches_what_the_host_sends() {
        let input: AuthorizeUrlInput = serde_json::from_str(
            r#"{"state":"s1","redirect_uri":"https://app.test/cb","config":{"realm":"r"}}"#,
        )
        .unwrap();
        assert_eq!(input.state, "s1");
        assert_eq!(input.redirect_uri, "https://app.test/cb");
        assert_eq!(input.config["realm"], "r");
    }

    #[test]
    fn a_callback_parameter_is_readable_whatever_the_provider_called_it() {
        // OpenID 2.0 sends dotted keys; the host flattens them beside `config`.
        let input: CallbackInput =
            serde_json::from_str(r#"{"code":"c","state":"s","openid.claimed_id":"x","config":{}}"#)
                .unwrap();
        assert_eq!(input.param("code"), Some("c"));
        assert_eq!(input.param("state"), Some("s"));
        assert_eq!(input.param("openid.claimed_id"), Some("x"));
        assert_eq!(input.param("missing"), None);
        // `config` is a field, not a parameter, so it never shows up as one.
        assert_eq!(input.params.len(), 3);
        assert_eq!(input.config, serde_json::json!({}));
    }

    #[test]
    fn a_callback_with_no_parameters_and_no_config_still_decodes() {
        let input: CallbackInput = serde_json::from_str("{}").unwrap();
        assert!(input.params.is_empty());
        assert_eq!(input.config, Value::Null);
        assert_eq!(input.param("code"), None);
    }

    #[test]
    fn a_non_string_callback_parameter_is_kept_but_not_returned_as_text() {
        let input: CallbackInput = serde_json::from_str(r#"{"code":7}"#).unwrap();
        assert_eq!(input.param("code"), None);
        assert_eq!(input.params["code"], serde_json::json!(7));
    }

    #[test]
    fn refresh_and_token_inputs_default_their_config() {
        let refresh: RefreshInput = serde_json::from_str(r#"{"refresh_token":"r"}"#).unwrap();
        assert_eq!(refresh.refresh_token, "r");
        assert_eq!(refresh.config, Value::Null);

        let token: TokenInput =
            serde_json::from_str(r#"{"access_token":"a","config":{"k":1}}"#).unwrap();
        assert_eq!(token.access_token, "a");
        assert_eq!(token.config["k"], 1);
    }

    #[test]
    fn token_set_omits_absent_fields_rather_than_sending_null() {
        let json = serde_json::to_string(&TokenSet::new("a", "Bearer")).unwrap();
        assert_eq!(json, r#"{"access_token":"a","token_type":"Bearer"}"#);

        let full = TokenSet::new("a", "Bearer")
            .refresh_token("r")
            .expires_at("2026-01-01T00:00:00Z");
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(value["refresh_token"], "r");
        assert_eq!(value["expires_at"], "2026-01-01T00:00:00Z");
        assert_eq!(value.get("expires_in"), None);
        assert_eq!(serde_json::from_value::<TokenSet>(value).unwrap(), full);
    }

    #[test]
    fn token_set_expires_in_is_the_preferred_duration_field() {
        let json = serde_json::to_string(&TokenSet::new("a", "Bearer").expires_in(3600)).unwrap();
        assert_eq!(
            json,
            r#"{"access_token":"a","expires_in":3600,"token_type":"Bearer"}"#
        );

        let round_tripped: TokenSet = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.expires_in, Some(3600));
        assert_eq!(round_tripped.expires_at, None);
    }

    #[test]
    fn token_set_without_expires_in_deserializes_with_it_absent() {
        let ts: TokenSet =
            serde_json::from_str(r#"{"access_token":"a","token_type":"Bearer"}"#).unwrap();
        assert_eq!(ts.expires_in, None);
    }

    #[test]
    fn external_profile_round_trips_with_only_an_account_id() {
        let json = serde_json::to_string(&ExternalProfile::new("76561198000000000")).unwrap();
        assert_eq!(json, r#"{"account_id":"76561198000000000"}"#);

        let full = ExternalProfile::new("id")
            .display_name("Name")
            .profile_url("https://example.test/u/id")
            .avatar_url("https://example.test/a/id.png");
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(value["display_name"], "Name");
        assert_eq!(
            serde_json::from_value::<ExternalProfile>(value).unwrap(),
            full
        );
    }
}
