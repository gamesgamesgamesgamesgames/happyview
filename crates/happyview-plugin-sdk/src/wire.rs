//! Every type that crosses the WASM boundary, defined once.
//!
//! The host (`src/plugin/` in HappyView) re-exports these rather than keeping
//! its own copies, so a field added here reaches both sides at once and the two
//! cannot drift. Guest ergonomics — the builders, `no_std`, the `Cow` body
//! reader — live here too; none of them change what goes on the wire.
//!
//! Import paths are stable: [`crate::types`], [`crate::envelope`] and
//! [`crate::host`] re-export the types that used to live in them.

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// The wire envelope, in both directions.
///
/// A plugin returns either `{"ok": <value>}` or
/// `{"error": {"code", "message", "retryable"}}`; the host parses exactly this
/// shape, under the name `PluginResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response<T> {
    Ok { ok: T },
    Err { error: PluginError },
}

impl<T> Response<T> {
    /// Collapse the envelope into a `Result`.
    pub fn into_result(self) -> Result<T, PluginError> {
        match self {
            Response::Ok { ok } => Ok(ok),
            Response::Err { error } => Err(error),
        }
    }
}

/// A structured plugin error. `code` is a free-form string; the host relays it
/// verbatim to whoever called the plugin. Known there as `PluginEnvelopeError`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    /// Mark this error as worth retrying. The host surfaces the flag; it does
    /// not retry on the plugin's behalf.
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// Input the plugin could not make sense of. Code `BAD_INPUT`.
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new("BAD_INPUT", message)
    }

    /// The caller named an export this plugin does not have. Code `UNKNOWN_FUNCTION`.
    pub fn unknown_function(name: &str) -> Self {
        Self::new("UNKNOWN_FUNCTION", format!("no such function: {name}"))
    }

    /// A host call failed in a way that is not the caller's fault. Code `HOST_ERROR`.
    pub fn host(message: impl Into<String>) -> Self {
        Self::new("HOST_ERROR", message)
    }
}

impl From<serde_json::Error> for PluginError {
    fn from(err: serde_json::Error) -> Self {
        Self::bad_input(format!("{err}"))
    }
}

impl core::fmt::Display for PluginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// `core::error::Error` is `std::error::Error`, so this is what lets the host
/// carry a plugin's error as a `#[source]`.
impl core::error::Error for PluginError {}

// ---------------------------------------------------------------------------
// Identity and call envelope
// ---------------------------------------------------------------------------

/// What `plugin_info` returns.
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
/// library, so a plugin may narrow what it does but never claim more. The host
/// calls this `LibraryCallContext`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallContext {
    #[serde(default)]
    pub caller_did: Option<String>,
    #[serde(default)]
    pub has_pds_auth: bool,
    /// `"sqlite"` or `"postgres"`. A guest has no other way to learn which
    /// placeholder syntax raw SQL needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_backend: Option<String>,
}

/// The `call` export's input. Every field but `function` defaults, so an
/// abbreviated envelope still dispatches. The host calls this
/// `LibraryCallInput`.
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

/// One entry in an [`ApiSurface`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiExport {
    pub name: String,
    /// `"function"`, `"constant"` or `"constructor"`; interpreters may agree
    /// on others.
    #[serde(default = "default_export_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Value>,
    /// Present on a `constructor` export: the methods of the object it
    /// returns. A `lazy` method accumulates a step; an `immediate` method
    /// sends the object's arguments, its steps and the call in one library
    /// call named after the constructor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<ApiMethod>,
    /// Everything else the library said about the export, preserved for
    /// interpreters. A builder leaves it empty.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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
            methods: Vec::new(),
            extra: Map::new(),
        }
    }

    pub fn constant(name: impl Into<String>) -> Self {
        Self {
            kind: String::from("constant"),
            ..Self::function(name)
        }
    }

    pub fn constructor(name: impl Into<String>) -> Self {
        Self {
            kind: String::from("constructor"),
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

    pub fn lazy(mut self, name: impl Into<String>) -> Self {
        self.methods.push(ApiMethod::lazy(name));
        self
    }

    pub fn immediate(mut self, name: impl Into<String>) -> Self {
        self.methods.push(ApiMethod::immediate(name));
        self
    }

    pub fn is_function(&self) -> bool {
        self.kind == "function"
    }

    pub fn is_constructor(&self) -> bool {
        self.kind == "constructor"
    }
}

/// A method on the object a `constructor` export returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiMethod {
    pub name: String,
    /// `"lazy"` or `"immediate"`.
    pub mode: String,
}

impl ApiMethod {
    pub fn lazy(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mode: String::from("lazy"),
        }
    }

    pub fn immediate(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mode: String::from("immediate"),
        }
    }

    pub fn is_lazy(&self) -> bool {
        self.mode == "lazy"
    }

    pub fn is_immediate(&self) -> bool {
        self.mode == "immediate"
    }
}

/// One accumulated lazy call, on the wire as a one-key object such as
/// `{"where": ["a", "=", "1"]}`, so a document reads as the chain that
/// produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    pub args: Vec<Value>,
}

impl Serialize for Step {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(&self.name, &self.args)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = Map::<String, Value>::deserialize(deserializer)?;
        if map.len() != 1 {
            return Err(serde::de::Error::custom(
                "a step is an object with exactly one key",
            ));
        }
        let (name, args) = map.into_iter().next().expect("length checked");
        let args = match args {
            Value::Array(a) => a,
            _ => return Err(serde::de::Error::custom("step arguments must be an array")),
        };
        Ok(Step { name, args })
    }
}

/// The immediate method that ended a chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodCall {
    pub name: String,
    #[serde(default)]
    pub args: Vec<Value>,
}

/// What an immediate method sends: the constructor's arguments, every lazy
/// step since, and the call itself. Objects have no guest memory between
/// calls, so this document is the whole object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectCall {
    #[serde(default)]
    pub args: Vec<Value>,
    #[serde(default)]
    pub steps: Vec<Step>,
    pub call: MethodCall,
}

impl ObjectCall {
    /// The single argument a constructor-named library call receives.
    pub fn from_args(args: &[Value]) -> Result<Self, PluginError> {
        let [doc] = args else {
            return Err(PluginError::bad_input(
                "expected exactly one object document",
            ));
        };
        serde_json::from_value(doc.clone())
            .map_err(|e| PluginError::bad_input(format!("invalid object document: {e}")))
    }
}

/// One comparison in a record or table filter. `value` is the JSON value the
/// script passed — a string, number or boolean. A record filter binds it as
/// text; a table filter binds it by JSON type against the column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub field: String,
    pub op: String,
    pub value: Value,
}

/// A filter tree. Untagged so a bare condition and a group both read
/// naturally; a group's `conditions` may nest further groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Filter {
    Condition(Condition),
    Group {
        combine: String,
        conditions: Vec<Filter>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sort {
    pub field: String,
    pub direction: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordsQuery {
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<Sort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordsCount {
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordsSearch {
    pub collection: String,
    pub field: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableQuery {
    pub table: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<Sort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default)]
    pub count: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacklinksQuery {
    pub uri: String,
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordsPage {
    pub records: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// A strong reference to an AT Protocol record, as `host_lookup_record` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrongRef {
    pub uri: String,
    pub cid: String,
}

// ---------------------------------------------------------------------------
// Caller-acting writes, index writes, and lexicon lookup
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

/// What a successful create or put on the caller's own repo returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordRef {
    pub uri: String,
    pub cid: String,
}

/// A record create issued as the calling script's user. Needs `caller:write`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerRecordCreate {
    pub collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rkey: Option<String>,
    /// The repo to write to. Absent means the caller's own repo — the only
    /// repo a caller-acting write can ever target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub record: Value,
    /// Whether the PDS should validate the record against its lexicon.
    /// Defaults to `true`, matching `com.atproto.repo.createRecord`.
    #[serde(default = "default_true")]
    pub validate: bool,
}

/// A record put (upsert) issued as the calling script's user. Needs
/// `caller:write`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerRecordPut {
    pub uri: String,
    pub record: Value,
    /// A no-create guarantee when set: the PDS refuses unless this CID is the
    /// record's current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_cid: Option<String>,
    #[serde(default = "default_true")]
    pub validate: bool,
}

/// A record delete issued as the calling script's user. Needs `caller:write`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerRecordDelete {
    pub uri: String,
}

/// A blob upload issued as the calling script's user. Needs `caller:write`.
///
/// `bytes` travels the same way an [`HttpRequest`] body does: a JSON string
/// when it is valid UTF-8, a byte array otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerBlobUpload {
    #[serde(
        serialize_with = "serialize_body",
        deserialize_with = "deserialize_body_flexible_required"
    )]
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

/// An XRPC query issued as the calling script's user. Needs `caller:read`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerXrpcQuery {
    pub method: String,
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// An XRPC procedure issued as the calling script's user. Needs `caller:call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerXrpcProcedure {
    pub method: String,
    pub input: Value,
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// A write to the local record index, bypassing the PDS. Needs
/// `records:write`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexPut {
    pub collection: String,
    pub rkey: String,
    /// The record's author, and half of the URI the row is keyed by. The host
    /// has no default for it and refuses a spec without one; pass the DID from
    /// the call context, or the repo the record was written to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    pub record: Value,
    /// The CID the network assigned this version, when the caller has one —
    /// from a PDS create, say. It is recorded only when the row is new: a CID
    /// describes what a PDS holds, so an index write may introduce one but
    /// never overwrite one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
}

/// An index removal, bypassing the PDS. Needs `records:write`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDelete {
    pub uri: String,
}

/// A lookup of an uploaded lexicon's raw JSON, by NSID. Needs no capability —
/// a lexicon is public schema, not user data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexiconGet {
    pub nsid: String,
}

// ---------------------------------------------------------------------------
// Auth plugin inputs and outputs
// ---------------------------------------------------------------------------

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
/// `expires_at` is an RFC 3339 timestamp. A guest has no clock, so it cannot
/// turn a provider's duration (what Microsoft and Xbox return) into an absolute
/// instant; prefer [`expires_in`](Self::expires_in) and use `expires_at` only
/// when the provider itself gives an absolute instant. The host resolves the
/// two through `TokenSetExt::resolved_expires_at`, which is where an
/// unparseable `expires_at` is refused.
///
/// Absent fields are omitted rather than sent as null.
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

// ---------------------------------------------------------------------------
// Host calls
// ---------------------------------------------------------------------------

/// An outbound HTTP request. `headers` are sent as the host expects them,
/// `[[name, value], ...]`.
///
/// The body is bytes, because an HTTP body is bytes; it travels as a JSON
/// string whenever it is valid UTF-8, which is what every deployed plugin
/// sends, and as a byte array otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(
        default,
        serialize_with = "serialize_optional_body",
        deserialize_with = "deserialize_body_flexible"
    )]
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    /// A request with no headers and no body.
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// A text body. The wire form is a JSON string.
    pub fn body_text(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into().into_bytes());
        self
    }

    /// A body that is not text — an image, say. The wire form is a byte array
    /// unless the bytes happen to be valid UTF-8.
    pub fn body_bytes(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }
}

/// A response from `host::http_request`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Sent as a string when the body is valid UTF-8 and as a byte array when
    /// it is not, so a plugin that declares a string body still decodes it.
    #[serde(
        default,
        serialize_with = "serialize_body",
        deserialize_with = "deserialize_body_flexible_required"
    )]
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Look up a response header, ignoring case. The host does not normalise
    /// header names, so a case-sensitive match would miss most of them.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text, with invalid UTF-8 replaced rather than refused.
    /// Borrows when the body is already valid UTF-8.
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// A body as the wire carries it: a string when it is valid UTF-8, a byte array
/// when it is not.
struct BodyRepr<'a>(&'a [u8]);

impl Serialize for BodyRepr<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match core::str::from_utf8(self.0) {
            Ok(text) => serializer.serialize_str(text),
            Err(_) => serializer.serialize_bytes(self.0),
        }
    }
}

fn serialize_body<S: Serializer>(body: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    BodyRepr(body).serialize(serializer)
}

fn serialize_optional_body<S: Serializer>(
    body: &Option<Vec<u8>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match body {
        Some(body) => serializer.serialize_some(&BodyRepr(body)),
        None => serializer.serialize_none(),
    }
}

/// Accepts a JSON string, a byte array, or null, so either side can send
/// `"body": "text"` or `"body": [1,2,3]`.
fn deserialize_body_flexible<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct BodyVisitor;

    impl<'de> de::Visitor<'de> for BodyVisitor {
        type Value = Option<Vec<u8>>;

        fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            f.write_str("a string, a byte array, or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.as_bytes().to_vec()))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v.into_bytes()))
        }

        fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            Ok(Some(v.to_vec()))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            let bytes: Vec<u8> =
                de::Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
            Ok(Some(bytes))
        }

        fn visit_some<D2: serde::Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Self::Value, D2::Error> {
            deserializer.deserialize_any(BodyVisitor)
        }
    }

    deserializer.deserialize_any(BodyVisitor)
}

/// As [`deserialize_body_flexible`], where an absent or null body is empty
/// rather than missing.
fn deserialize_body_flexible_required<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(deserialize_body_flexible(deserializer)?.unwrap_or_default())
}

/// Which indexed record to look for, by a value nested inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupRequest {
    pub collection: String,
    /// Dotted path into the record, e.g. `externalIds.steam`.
    pub external_id_field: String,
    pub external_id_value: String,
}

impl LookupRequest {
    pub fn new(
        collection: impl Into<String>,
        external_id_field: impl Into<String>,
        external_id_value: impl Into<String>,
    ) -> Self {
        Self {
            collection: collection.into(),
            external_id_field: external_id_field.into(),
            external_id_value: external_id_value.into(),
        }
    }
}

/// Severity for `host::log`. The host parses these exact strings, and calls the
/// type `LogLevel`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Level {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

impl core::fmt::Display for Level {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a level the host does not know parses to. The host logs such a line at
/// `info` rather than dropping it, so this rarely reaches a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLevelError;

impl core::fmt::Display for ParseLevelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("expected one of: debug, info, warn, error")
    }
}

impl core::error::Error for ParseLevelError {}

impl core::str::FromStr for Level {
    type Err = ParseLevelError;

    /// Case-insensitive. `warning` is accepted for `warn`, which is what a
    /// plugin writing its own level string is most likely to send.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("debug") {
            Ok(Level::Debug)
        } else if s.eq_ignore_ascii_case("info") {
            Ok(Level::Info)
        } else if s.eq_ignore_ascii_case("warn") || s.eq_ignore_ascii_case("warning") {
            Ok(Level::Warn)
        } else if s.eq_ignore_ascii_case("error") {
            Ok(Level::Error)
        } else {
            Err(ParseLevelError)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;
    use core::str::FromStr;

    use super::*;

    #[test]
    fn ok_envelope_round_trips() {
        let envelope = Response::Ok {
            ok: serde_json::json!({"status": 200}),
        };
        let text = serde_json::to_string(&envelope).unwrap();
        assert_eq!(text, r#"{"ok":{"status":200}}"#);
        let parsed: Response<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed.into_result().unwrap(),
            serde_json::json!({"status": 200})
        );
    }

    #[test]
    fn error_envelope_round_trips() {
        let envelope: Response<Value> = Response::Err {
            error: PluginError::new("HTTP_ERROR", "boom").retryable(),
        };
        let text = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            text,
            r#"{"error":{"code":"HTTP_ERROR","message":"boom","retryable":true}}"#
        );
        let parsed: Response<Value> = serde_json::from_str(&text).unwrap();
        let err = parsed.into_result().unwrap_err();
        assert_eq!(err.code, "HTTP_ERROR");
        assert!(err.retryable);
    }

    #[test]
    fn retryable_defaults_to_false_when_the_host_omits_it() {
        let parsed: Response<Value> =
            serde_json::from_str(r#"{"error":{"code":"X","message":"y"}}"#).unwrap();
        assert!(!parsed.into_result().unwrap_err().retryable);
    }

    #[test]
    fn error_constructors_use_the_codes_the_host_expects() {
        assert_eq!(PluginError::bad_input("nope").code, "BAD_INPUT");
        assert_eq!(PluginError::host("nope").code, "HOST_ERROR");

        let unknown = PluginError::unknown_function("frobnicate");
        assert_eq!(unknown.code, "UNKNOWN_FUNCTION");
        assert_eq!(unknown.message, "no such function: frobnicate");
        assert!(!unknown.retryable);

        let parse_error = serde_json::from_str::<Value>("{oops").unwrap_err();
        assert_eq!(PluginError::from(parse_error).code, "BAD_INPUT");
    }

    #[test]
    fn an_ok_envelope_holding_an_error_key_still_parses_as_ok() {
        // `Response` is untagged, so ordering matters: `{"ok": ...}` must win.
        let parsed: Response<Value> = serde_json::from_str(r#"{"ok":{"error":"inner"}}"#).unwrap();
        assert_eq!(
            parsed.into_result().unwrap(),
            serde_json::json!({"error": "inner"})
        );
    }

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
        assert_eq!(input.args, vec![Value::from(1)]);
        assert_eq!(input.context.caller_did, None);
        assert!(input.context.has_pds_auth);
    }

    #[test]
    fn call_input_round_trips_as_the_host_sends_it() {
        let input = CallInput {
            function: "get".to_string(),
            args: vec![Value::from("https://example.test")],
            context: CallContext {
                caller_did: Some("did:plc:abc".to_string()),
                has_pds_auth: true,
                db_backend: None,
            },
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["function"], "get");
        assert_eq!(json["args"][0], "https://example.test");
        assert_eq!(json["context"]["caller_did"], "did:plc:abc");
        assert_eq!(json["context"]["has_pds_auth"], true);
        assert_eq!(serde_json::from_value::<CallInput>(json).unwrap(), input);
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
    fn an_export_keeps_whatever_else_the_library_said_about_it() {
        // An interpreter may read fields the SDK has never heard of, so an
        // unknown key survives the round trip rather than being dropped.
        let export: ApiExport = serde_json::from_str(
            r#"{"name":"get","kind":"function","deprecated":true,"since":"1.2.0"}"#,
        )
        .unwrap();
        assert!(export.is_function());
        assert_eq!(export.extra["deprecated"], true);
        assert_eq!(export.extra["since"], "1.2.0");

        let json = serde_json::to_value(&export).unwrap();
        assert_eq!(json["deprecated"], true);
        assert_eq!(json["since"], "1.2.0");
        assert!(json.get("extra").is_none(), "flattened, not nested");
    }

    #[test]
    fn a_builder_export_carries_no_extra_fields() {
        let json = serde_json::to_value(ApiExport::constant("VERSION")).unwrap();
        assert_eq!(json["kind"], "constant");
        assert!(!ApiExport::constant("VERSION").is_function());
        assert_eq!(json.as_object().unwrap().len(), 3, "name, kind, params");
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

    #[test]
    fn response_header_lookup_ignores_case() {
        let response = HttpResponse {
            status: 200,
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Rate-Limit".to_string(), "60".to_string()),
            ],
            body: Vec::new(),
        };
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.header("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(response.header("x-rate-limit"), Some("60"));
        assert_eq!(response.header("accept"), None);
    }

    #[test]
    fn response_body_accepts_a_string_or_a_byte_array() {
        let text: HttpResponse =
            serde_json::from_str(r#"{"status":200,"headers":[],"body":"hi"}"#).unwrap();
        assert_eq!(text.body, b"hi");
        assert_eq!(text.text(), "hi");

        let bytes: HttpResponse =
            serde_json::from_str(r#"{"status":204,"headers":[],"body":[104,105]}"#).unwrap();
        assert_eq!(bytes.body, b"hi");

        let missing: HttpResponse = serde_json::from_str(r#"{"status":204,"headers":[]}"#).unwrap();
        assert!(missing.body.is_empty());
        assert_eq!(missing.text(), "");

        let null: HttpResponse =
            serde_json::from_str(r#"{"status":204,"headers":[],"body":null}"#).unwrap();
        assert!(null.body.is_empty());
    }

    #[test]
    fn a_utf8_response_body_travels_as_a_string_and_a_binary_one_as_bytes() {
        let text = HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: b"hi".to_vec(),
        };
        assert_eq!(
            serde_json::to_string(&text).unwrap(),
            r#"{"status":200,"headers":[],"body":"hi"}"#
        );
        assert_eq!(
            serde_json::from_str::<HttpResponse>(&serde_json::to_string(&text).unwrap()).unwrap(),
            text
        );

        let binary = HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: vec![0xff, 0xfe],
        };
        assert_eq!(
            serde_json::to_string(&binary).unwrap(),
            r#"{"status":200,"headers":[],"body":[255,254]}"#
        );
        assert_eq!(
            serde_json::from_str::<HttpResponse>(&serde_json::to_string(&binary).unwrap()).unwrap(),
            binary
        );
        // Not valid UTF-8, so reading it as text replaces rather than fails.
        assert_eq!(binary.text(), "\u{fffd}\u{fffd}");
    }

    #[test]
    fn a_request_body_travels_the_same_way_and_stays_null_when_absent() {
        let none = HttpRequest::new("GET", "https://example.test");
        assert_eq!(
            serde_json::to_string(&none).unwrap(),
            r#"{"method":"GET","url":"https://example.test","headers":[],"body":null}"#
        );
        assert_eq!(
            serde_json::from_str::<HttpRequest>(&serde_json::to_string(&none).unwrap()).unwrap(),
            none
        );

        let text = HttpRequest::new("POST", "https://example.test").body_text("{}");
        assert_eq!(
            serde_json::to_value(&text).unwrap()["body"],
            Value::from("{}")
        );
        assert_eq!(text.body.as_deref(), Some(&b"{}"[..]));

        let binary = HttpRequest::new("POST", "https://example.test").body_bytes(vec![0xff, 0xfe]);
        assert_eq!(
            serde_json::to_value(&binary).unwrap()["body"],
            serde_json::json!([255, 254])
        );
        assert_eq!(
            serde_json::from_value::<HttpRequest>(serde_json::to_value(&binary).unwrap()).unwrap(),
            binary
        );
    }

    #[test]
    fn the_original_body_builder_still_sets_a_text_body() {
        assert_eq!(
            HttpRequest::new("POST", "https://example.test").body_text("hi"),
            HttpRequest::new("POST", "https://example.test").body_text("hi")
        );
    }

    #[test]
    fn a_request_with_no_headers_and_no_body_key_still_decodes() {
        let request: HttpRequest =
            serde_json::from_str(r#"{"method":"GET","url":"https://example.test"}"#).unwrap();
        assert!(request.headers.is_empty());
        assert_eq!(request.body, None);
    }

    #[test]
    fn log_levels_parse_case_insensitively_and_refuse_anything_else() {
        assert_eq!(Level::from_str("debug"), Ok(Level::Debug));
        assert_eq!(Level::from_str("INFO"), Ok(Level::Info));
        assert_eq!(Level::from_str("Warn"), Ok(Level::Warn));
        // The host has always accepted `warning` for `warn`.
        assert_eq!(Level::from_str("warning"), Ok(Level::Warn));
        assert_eq!(Level::from_str("error"), Ok(Level::Error));

        assert_eq!(Level::from_str("trace"), Err(ParseLevelError));
        assert_eq!(Level::from_str(""), Err(ParseLevelError));
        // Which is what makes `unwrap_or_default()` at the host's call site
        // log an unknown level at `info` rather than dropping the line.
        assert_eq!(Level::from_str("nonsense").unwrap_or_default(), Level::Info);
    }

    #[test]
    fn a_log_level_round_trips_through_its_own_string() {
        for level in [Level::Debug, Level::Info, Level::Warn, Level::Error] {
            assert_eq!(Level::from_str(level.as_str()), Ok(level));
            assert_eq!(alloc::format!("{level}"), level.as_str());
        }
    }
}

#[cfg(test)]
mod object_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn constructor_export_carries_methods() {
        let export = ApiExport::constructor("records")
            .lazy("where")
            .immediate("run");
        assert!(export.is_constructor());
        assert!(!export.is_function());
        let json = serde_json::to_value(&export).unwrap();
        assert_eq!(json["kind"], "constructor");
        assert_eq!(
            json["methods"],
            json!([
                {"name": "where", "mode": "lazy"},
                {"name": "run", "mode": "immediate"}
            ])
        );
    }

    #[test]
    fn function_export_omits_methods() {
        let json = serde_json::to_value(ApiExport::function("get")).unwrap();
        assert!(json.get("methods").is_none());
        let back: ApiExport = serde_json::from_value(json).unwrap();
        assert!(back.methods.is_empty());
    }

    #[test]
    fn step_is_a_one_key_object() {
        let step = Step {
            name: "where".into(),
            args: vec![json!("a"), json!("="), json!(1)],
        };
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            json!({"where": ["a", "=", 1]})
        );
        let back: Step = serde_json::from_value(json!({"limit": [20]})).unwrap();
        assert_eq!(back.name, "limit");
        assert_eq!(back.args, vec![json!(20)]);
    }

    #[test]
    fn step_rejects_zero_or_two_keys() {
        assert!(serde_json::from_value::<Step>(json!({})).is_err());
        assert!(serde_json::from_value::<Step>(json!({"a": [], "b": []})).is_err());
        assert!(serde_json::from_value::<Step>(json!({"a": "not an array"})).is_err());
    }

    #[test]
    fn object_call_round_trips() {
        let doc = json!({
            "args": ["app.bsky.feed.post"],
            "steps": [{"where": ["author", "=", "did:plc:abc"]}, {"limit": [20]}],
            "call": {"name": "run", "args": []}
        });
        let call = ObjectCall::from_args(&[doc.clone()]).unwrap();
        assert_eq!(call.args, vec![json!("app.bsky.feed.post")]);
        assert_eq!(call.steps.len(), 2);
        assert_eq!(call.call.name, "run");
        assert_eq!(serde_json::to_value(&call).unwrap(), doc);
    }

    #[test]
    fn object_call_needs_exactly_one_document() {
        let err = ObjectCall::from_args(&[]).unwrap_err();
        assert_eq!(err.code, "BAD_INPUT");
        let err = ObjectCall::from_args(&[json!({}), json!({})]).unwrap_err();
        assert_eq!(err.code, "BAD_INPUT");
        let err = ObjectCall::from_args(&[json!({"args": []})]).unwrap_err();
        assert_eq!(err.code, "BAD_INPUT");
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filter_deserialises_conditions_and_groups() {
        let f: Filter = serde_json::from_value(json!({
            "combine": "and",
            "conditions": [
                {"field": "a", "op": "=", "value": "1"},
                {"combine": "or", "conditions": [{"field": "b", "op": "LIKE", "value": "%x%"}]}
            ]
        }))
        .unwrap();
        match f {
            Filter::Group {
                combine,
                conditions,
            } => {
                assert_eq!(combine, "and");
                assert_eq!(conditions.len(), 2);
                assert!(matches!(conditions[0], Filter::Condition(_)));
                assert!(matches!(conditions[1], Filter::Group { .. }));
            }
            Filter::Condition(_) => panic!("expected a group"),
        }
    }

    #[test]
    fn records_query_defaults() {
        let q: RecordsQuery = serde_json::from_value(json!({"collection": "c"})).unwrap();
        assert_eq!(q.collection, "c");
        assert!(q.did.is_none() && q.filter.is_none() && q.sort.is_none());
        assert!(q.limit.is_none() && q.cursor.is_none());
    }

    #[test]
    fn table_query_count_defaults_false() {
        let q: TableQuery = serde_json::from_value(json!({"table": "t"})).unwrap();
        assert!(!q.count);
    }

    #[test]
    fn call_context_backend_defaults_to_none() {
        let ctx: CallContext = serde_json::from_value(json!({})).unwrap();
        assert!(ctx.db_backend.is_none());
        let ctx: CallContext = serde_json::from_value(json!({"db_backend": "sqlite"})).unwrap();
        assert_eq!(ctx.db_backend.as_deref(), Some("sqlite"));
    }

    #[test]
    fn records_page_omits_absent_cursor() {
        let page = RecordsPage {
            records: vec![json!({"uri": "at://x"})],
            cursor: None,
        };
        let json = serde_json::to_value(&page).unwrap();
        assert!(json.get("cursor").is_none());
        assert_eq!(json["records"][0]["uri"], "at://x");
    }
}

#[cfg(test)]
mod caller_tests {
    use super::*;
    use alloc::string::ToString;
    use serde_json::json;

    #[test]
    fn record_ref_round_trips() {
        let r = RecordRef {
            uri: "at://did:plc:abc/app.bsky.feed.post/1".to_string(),
            cid: "bafy123".to_string(),
        };
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(value["uri"], "at://did:plc:abc/app.bsky.feed.post/1");
        assert_eq!(value["cid"], "bafy123");
        assert_eq!(serde_json::from_value::<RecordRef>(value).unwrap(), r);
    }

    #[test]
    fn caller_record_create_defaults_validate_true_and_omits_optionals() {
        let create: CallerRecordCreate = serde_json::from_value(json!({
            "collection": "app.bsky.feed.post",
            "record": {"text": "hi"},
        }))
        .unwrap();
        assert_eq!(create.collection, "app.bsky.feed.post");
        assert_eq!(create.rkey, None);
        assert_eq!(create.repo, None);
        assert_eq!(create.record, json!({"text": "hi"}));
        assert!(create.validate);

        let value = serde_json::to_value(&create).unwrap();
        assert!(value.get("rkey").is_none());
        assert!(value.get("repo").is_none());
        assert_eq!(value["validate"], true);
    }

    #[test]
    fn caller_record_create_round_trips_with_every_field_set() {
        let create = CallerRecordCreate {
            collection: "app.bsky.feed.post".to_string(),
            rkey: Some("abc123".to_string()),
            repo: Some("did:plc:abc".to_string()),
            record: json!({"text": "hi"}),
            validate: false,
        };
        let value = serde_json::to_value(&create).unwrap();
        assert_eq!(value["rkey"], "abc123");
        assert_eq!(value["repo"], "did:plc:abc");
        assert_eq!(value["validate"], false);
        assert_eq!(
            serde_json::from_value::<CallerRecordCreate>(value).unwrap(),
            create
        );
    }

    #[test]
    fn caller_record_put_defaults_validate_true_and_omits_swap_cid() {
        let put: CallerRecordPut = serde_json::from_value(json!({
            "uri": "at://did:plc:abc/app.bsky.feed.post/1",
            "record": {"text": "hi"},
        }))
        .unwrap();
        assert_eq!(put.swap_cid, None);
        assert!(put.validate);

        let value = serde_json::to_value(&put).unwrap();
        assert!(value.get("swap_cid").is_none());
        assert_eq!(value["validate"], true);
    }

    #[test]
    fn caller_record_put_round_trips_with_swap_cid_and_validate_false() {
        let put = CallerRecordPut {
            uri: "at://did:plc:abc/app.bsky.feed.post/1".to_string(),
            record: json!({"text": "hi"}),
            swap_cid: Some("bafy123".to_string()),
            validate: false,
        };
        let value = serde_json::to_value(&put).unwrap();
        assert_eq!(value["swap_cid"], "bafy123");
        assert_eq!(value["validate"], false);
        assert_eq!(
            serde_json::from_value::<CallerRecordPut>(value).unwrap(),
            put
        );
    }

    #[test]
    fn caller_record_delete_round_trips() {
        let delete = CallerRecordDelete {
            uri: "at://did:plc:abc/app.bsky.feed.post/1".to_string(),
        };
        let value = serde_json::to_value(&delete).unwrap();
        assert_eq!(value["uri"], "at://did:plc:abc/app.bsky.feed.post/1");
        assert_eq!(
            serde_json::from_value::<CallerRecordDelete>(value).unwrap(),
            delete
        );
    }

    #[test]
    fn caller_blob_upload_bytes_travel_like_an_http_body() {
        // A UTF-8 payload serializes as a JSON string...
        let text = CallerBlobUpload {
            bytes: b"hello".to_vec(),
            mime_type: "text/plain".to_string(),
        };
        let value = serde_json::to_value(&text).unwrap();
        assert_eq!(value["bytes"], json!("hello"));
        assert_eq!(
            serde_json::from_value::<CallerBlobUpload>(value).unwrap(),
            text
        );

        // ...and a non-UTF-8 payload serializes as a byte array, both ways.
        let binary = CallerBlobUpload {
            bytes: vec![0xff, 0xfe],
            mime_type: "image/png".to_string(),
        };
        let value = serde_json::to_value(&binary).unwrap();
        assert_eq!(value["bytes"], json!([255, 254]));
        assert_eq!(
            serde_json::from_value::<CallerBlobUpload>(value).unwrap(),
            binary
        );

        // A caller may also send a byte array for text content.
        let from_array: CallerBlobUpload = serde_json::from_value(json!({
            "bytes": [104, 105],
            "mime_type": "text/plain",
        }))
        .unwrap();
        assert_eq!(from_array.bytes, b"hi");
    }

    #[test]
    fn caller_xrpc_query_defaults_params_to_empty() {
        let query: CallerXrpcQuery = serde_json::from_value(json!({
            "method": "app.bsky.feed.getPosts",
        }))
        .unwrap();
        assert_eq!(query.method, "app.bsky.feed.getPosts");
        assert!(query.params.is_empty());

        let with_params: CallerXrpcQuery = serde_json::from_value(json!({
            "method": "app.bsky.feed.getPosts",
            "params": {"uris": ["at://x"]},
        }))
        .unwrap();
        assert_eq!(with_params.params["uris"], json!(["at://x"]));
    }

    #[test]
    fn caller_xrpc_procedure_defaults_params_to_empty() {
        let procedure: CallerXrpcProcedure = serde_json::from_value(json!({
            "method": "com.atproto.repo.createRecord",
            "input": {"collection": "app.bsky.feed.post"},
        }))
        .unwrap();
        assert!(procedure.params.is_empty());
        assert_eq!(procedure.input["collection"], "app.bsky.feed.post");
    }

    #[test]
    fn index_put_defaults_did_and_cid_to_none() {
        let put: IndexPut = serde_json::from_value(json!({
            "collection": "app.bsky.feed.post",
            "rkey": "abc123",
            "record": {"text": "hi"},
        }))
        .unwrap();
        assert_eq!(put.did, None);
        assert_eq!(put.cid, None);

        let value = serde_json::to_value(&put).unwrap();
        assert!(value.get("did").is_none());
        assert!(value.get("cid").is_none());

        let full = IndexPut {
            collection: "app.bsky.feed.post".to_string(),
            rkey: "abc123".to_string(),
            did: Some("did:plc:abc".to_string()),
            record: json!({"text": "hi"}),
            cid: Some("bafy123".to_string()),
        };
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(value["did"], "did:plc:abc");
        assert_eq!(value["cid"], "bafy123");
        assert_eq!(serde_json::from_value::<IndexPut>(value).unwrap(), full);
    }

    #[test]
    fn index_delete_round_trips() {
        let delete = IndexDelete {
            uri: "at://did:plc:abc/app.bsky.feed.post/1".to_string(),
        };
        let value = serde_json::to_value(&delete).unwrap();
        assert_eq!(
            serde_json::from_value::<IndexDelete>(value).unwrap(),
            delete
        );
    }

    #[test]
    fn lexicon_get_round_trips() {
        let get = LexiconGet {
            nsid: "app.bsky.feed.post".to_string(),
        };
        let value = serde_json::to_value(&get).unwrap();
        assert_eq!(value["nsid"], "app.bsky.feed.post");
        assert_eq!(serde_json::from_value::<LexiconGet>(value).unwrap(), get);
    }
}
