//! `happyview.toml`, the configuration a project directory carries.
//!
//! The file is optional, and every environment variable wins over it: the
//! values it supplies reach `Config` as the variables they stand in for,
//! through `config::vars`, so precedence is decided in one place.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

pub const PROJECT_FILE: &str = "happyview.toml";

/// Which binary entry point is reading the file. `[dev]` applies only to
/// `happyview dev`, so a production deploy of the same directory never binds
/// the port a developer picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMode {
    Serve,
    Dev,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub happyview: HappyviewSection,
    pub instance: InstanceSection,
    #[serde(deserialize_with = "de_interpreter_versions")]
    pub interpreters: BTreeMap<String, String>,
    #[serde(deserialize_with = "de_library_versions")]
    pub libraries: BTreeMap<String, String>,
    pub dev: DevSection,
    pub network_lexicons: NetworkLexiconsSection,
    pub labels: LabelsSection,
    pub jetstream: JetstreamSection,
    #[serde(deserialize_with = "de_keyed::<_, NsidRule, _>")]
    pub lexicons: BTreeMap<String, LexiconOverride>,
    #[serde(deserialize_with = "de_keyed::<_, TriggerRule, _>")]
    pub scripts: BTreeMap<String, ScriptOverride>,
    #[serde(deserialize_with = "de_keyed::<_, IdentifierRule, _>")]
    pub modules: BTreeMap<String, ModuleDecl>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HappyviewSection {
    #[serde(deserialize_with = "de_some::<_, NonEmptyRule>")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstanceSection {
    #[serde(deserialize_with = "de_some::<_, NonEmptyRule>")]
    pub app_name: Option<String>,
    #[serde(deserialize_with = "de_some::<_, DidRule>")]
    pub superadmin: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevSection {
    #[serde(deserialize_with = "de_port")]
    pub port: Option<u16>,
    pub backfill: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkLexiconsSection {
    #[serde(deserialize_with = "de_list::<_, NsidRule>")]
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LabelsSection {
    #[serde(deserialize_with = "de_list::<_, DidRule>")]
    pub subscribe: Vec<String>,
}

/// Typed rather than passed through as text so a bad value is refused with
/// its line here; `jetstream::config::parse_protocol` would only warn and
/// fall back to `auto` at connect time.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JetstreamSection {
    pub protocol: Option<JetstreamProtocol>,
    pub compression: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JetstreamProtocol {
    Auto,
    V2,
    V1,
}

impl JetstreamProtocol {
    /// The `JETSTREAM_PROTOCOL` spelling `parse_protocol` reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::V2 => "v2",
            Self::V1 => "v1",
        }
    }
}

/// Row metadata a lexicon document has no place for. Every field is optional
/// so the snapshot build can tell "not set" from "set to the default".
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LexiconOverride {
    pub backfill: Option<bool>,
    #[serde(deserialize_with = "de_some::<_, NsidRule>")]
    pub target_collection: Option<String>,
    pub action: Option<OverrideAction>,
    pub token_cost: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideAction {
    Create,
    Update,
    Delete,
    Upsert,
}

impl From<OverrideAction> for crate::lexicon::ProcedureAction {
    fn from(action: OverrideAction) -> Self {
        match action {
            OverrideAction::Create => Self::Create,
            OverrideAction::Update => Self::Update,
            OverrideAction::Delete => Self::Delete,
            OverrideAction::Upsert => Self::Upsert,
        }
    }
}

impl ProjectConfig {
    /// The file's settings as the environment variables they stand in for.
    /// Expressing them this way is what lets an environment variable win
    /// without each reader knowing the file exists.
    pub fn env_layer(&self, mode: ConfigMode) -> BTreeMap<String, String> {
        let mut layer = BTreeMap::new();
        if let Some(name) = &self.instance.app_name {
            layer.insert("APP_NAME".to_string(), name.clone());
        }
        if let Some(did) = &self.instance.superadmin {
            layer.insert("SUPERADMIN_DID".to_string(), did.clone());
        }
        if let Some(protocol) = self.jetstream.protocol {
            layer.insert(
                "JETSTREAM_PROTOCOL".to_string(),
                protocol.as_str().to_string(),
            );
        }
        if let Some(compression) = self.jetstream.compression {
            layer.insert("JETSTREAM_COMPRESSION".to_string(), compression.to_string());
        }
        if mode == ConfigMode::Dev {
            if let Some(port) = self.dev.port {
                layer.insert("PORT".to_string(), port.to_string());
            }
            layer.insert(
                "HAPPYVIEW_DEV_BACKFILL".to_string(),
                self.dev.backfill.to_string(),
            );
        }
        layer
    }

    /// `hpy` runs the pinned binary, so a mismatch means the binary was
    /// started some other way, typically an image tag that moved on. That is
    /// worth saying, not worth refusing to boot over.
    pub fn version_mismatch(&self, running: &str) -> Option<String> {
        let pinned = self
            .happyview
            .version
            .as_deref()?
            .trim()
            .trim_start_matches('v');
        (pinned != running).then(|| {
            format!(
                "{PROJECT_FILE} pins HappyView {pinned}, but this binary is {running}; \
                 `hpy happyview dev` runs the pinned version"
            )
        })
    }

    pub fn declares_labeler(&self, did: &str) -> bool {
        self.labels.subscribe.iter().any(|d| d == did)
    }

    pub fn declares_network_lexicon(&self, nsid: &str) -> bool {
        self.network_lexicons.include.iter().any(|n| n == nsid)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptOverride {
    pub description: Option<String>,
    pub outbound_xrpcs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleDecl {
    #[serde(deserialize_with = "de_one::<_, IdentifierRule>")]
    pub language: String,
    #[serde(deserialize_with = "de_module_path")]
    pub path: PathBuf,
    #[serde(default)]
    pub capabilities: Vec<crate::plugin::capabilities::PluginCapability>,
}

/// A check a string must pass to be accepted where it is written. Raising
/// it from inside deserialisation is what lets the parser report the
/// offending value's own line.
trait Rule {
    fn check(value: &str) -> Result<(), String>;
}

struct NonEmptyRule;
impl Rule for NonEmptyRule {
    fn check(value: &str) -> Result<(), String> {
        if value.trim().is_empty() {
            Err("must not be empty".into())
        } else {
            Ok(())
        }
    }
}

struct DidRule;
impl Rule for DidRule {
    fn check(value: &str) -> Result<(), String> {
        atrium_api::types::string::Did::new(value.to_string())
            .map(|_| ())
            .map_err(|e| format!("invalid DID '{value}': {e}"))
    }
}

struct NsidRule;
impl Rule for NsidRule {
    fn check(value: &str) -> Result<(), String> {
        happyview_nsid::validate_nsid(value).map_err(|e| e.to_string())
    }
}

/// The same grammar, reserved prefixes included, that the dashboard's
/// script editor enforces, so a file cannot name a trigger the dashboard
/// would refuse.
struct TriggerRule;
impl Rule for TriggerRule {
    fn check(value: &str) -> Result<(), String> {
        crate::lua::scripts::ParsedTrigger::parse(value).map(|_| ())
    }
}

/// Interpreter language ids and module names end up in columns, URLs and
/// build output paths, so they take the conservative shape an
/// interpreter's `language_id` already has.
struct IdentifierRule;
impl Rule for IdentifierRule {
    fn check(value: &str) -> Result<(), String> {
        if crate::plugin::loader::is_valid_language_id(value) {
            Ok(())
        } else {
            Err(format!(
                "'{value}' must be lowercase letters, digits, '-' or '_', starting with a letter"
            ))
        }
    }
}

struct Checked<R>(String, PhantomData<R>);

impl<'de, R: Rule> Deserialize<'de> for Checked<R> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The check runs inside the visitor, where the toml deserializer
        // stamps the value's own span on a failure; one run after
        // `String::deserialize` returns would get the enclosing array's.
        struct CheckedVisitor<R>(PhantomData<R>);

        impl<R: Rule> serde::de::Visitor<'_> for CheckedVisitor<R> {
            type Value = Checked<R>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                R::check(value).map_err(E::custom)?;
                Ok(Checked(value.to_string(), PhantomData))
            }
        }

        d.deserialize_str(CheckedVisitor(PhantomData))
    }
}

// Map keys need an order; `R` is a marker and takes no part in it.
impl<R> PartialEq for Checked<R> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<R> Eq for Checked<R> {}
impl<R> PartialOrd for Checked<R> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<R> Ord for Checked<R> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

fn de_one<'de, D: Deserializer<'de>, R: Rule>(d: D) -> Result<String, D::Error> {
    Checked::<R>::deserialize(d).map(|c| c.0)
}

fn de_some<'de, D: Deserializer<'de>, R: Rule>(d: D) -> Result<Option<String>, D::Error> {
    de_one::<D, R>(d).map(Some)
}

fn de_list<'de, D: Deserializer<'de>, R: Rule>(d: D) -> Result<Vec<String>, D::Error> {
    Vec::<Checked<R>>::deserialize(d).map(|items| items.into_iter().map(|c| c.0).collect())
}

fn de_keyed<'de, D, R, V>(d: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    R: Rule,
    V: Deserialize<'de>,
{
    BTreeMap::<Checked<R>, V>::deserialize(d)
        .map(|map| map.into_iter().map(|(k, v)| (k.0, v)).collect())
}

fn de_versions<'de, D: Deserializer<'de>, K: Rule>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    BTreeMap::<Checked<K>, Checked<NonEmptyRule>>::deserialize(d)
        .map(|map| map.into_iter().map(|(k, v)| (k.0, v.0)).collect())
}

fn de_interpreter_versions<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    de_versions::<D, IdentifierRule>(d)
}

fn de_library_versions<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    de_versions::<D, NonEmptyRule>(d)
}

fn de_port<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    match u16::deserialize(d)? {
        0 => Err(D::Error::custom("port must be between 1 and 65535")),
        port => Ok(Some(port)),
    }
}

/// A module is built from a directory the project owns, so the path can't
/// be absolute or climb out of the root.
fn de_module_path<'de, D: Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
    let raw = String::deserialize(d)?;
    let path = PathBuf::from(&raw);
    let escapes = path.has_root()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)));
    if raw.trim().is_empty() || escapes {
        return Err(D::Error::custom(format!(
            "module path '{raw}' must be relative to the project root and stay inside it"
        )));
    }
    Ok(path)
}

#[derive(Debug)]
pub enum ProjectConfigError {
    RootNotFound {
        root: PathBuf,
    },
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        line: Option<usize>,
        column: Option<usize>,
        message: String,
    },
}

impl fmt::Display for ProjectConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootNotFound { root } => write!(
                f,
                "project root {} does not exist or is not a directory",
                root.display()
            ),
            Self::Read { path, source } => write!(f, "could not read {}: {source}", path.display()),
            // `path:line:column: message` is the shape terminals and editors
            // turn into a link to the offending line.
            Self::Parse {
                path,
                line: Some(line),
                column: Some(column),
                message,
            } => {
                write!(f, "{}:{line}:{column}: {message}", path.display())
            }
            Self::Parse { path, message, .. } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for ProjectConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// `Ok(None)` when the root exists but holds no `happyview.toml`: the file is
/// optional even in a project. A root that does not exist is an error, since
/// serving as a plain instance because of a typo in `HAPPYVIEW_PROJECT` would
/// look like a working deploy.
pub fn load_project_config(root: &Path) -> Result<Option<ProjectConfig>, ProjectConfigError> {
    if !root.is_dir() {
        return Err(ProjectConfigError::RootNotFound {
            root: root.to_path_buf(),
        });
    }
    let path = root.join(PROJECT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ProjectConfigError::Read { path, source }),
    };
    parse_project_config(&text, &path).map(Some)
}

/// Parse the file's text. `path` is only used to name the file in errors.
pub fn parse_project_config(text: &str, path: &Path) -> Result<ProjectConfig, ProjectConfigError> {
    toml::from_str(text).map_err(|e| {
        let (line, column) = e.span().map(|span| line_col(text, span.start)).unzip();
        ProjectConfigError::Parse {
            path: path.to_path_buf(),
            line,
            column,
            message: e.message().trim_end().to_string(),
        }
    })
}

/// One-based line and column of a byte offset, with the column counted in
/// characters so it matches what an editor shows.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset.min(text.len())).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

#[cfg(test)]
pub(crate) struct TempProject(PathBuf);

#[cfg(test)]
impl TempProject {
    pub(crate) fn new(toml: Option<&str>) -> Self {
        let dir = std::env::temp_dir().join(format!("hv-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create the project directory");
        if let Some(text) = toml {
            std::fs::write(dir.join(PROJECT_FILE), text).expect("write happyview.toml");
        }
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ProjectConfig, ProjectConfigError> {
        parse_project_config(text, Path::new(PROJECT_FILE))
    }

    fn parse_err(text: &str) -> ProjectConfigError {
        parse(text).expect_err("the file should be refused")
    }

    const FULL: &str = r#"
[happyview]
version = "3.0.0"

[instance]
app_name = "HappyProto Registry"
superadmin = "did:plc:ar7c4by46qjdydhdevvrndac"

[interpreters]
lua = "1.4.0"
typescript = "0.2.0"

[libraries]
"happyview.record" = "2.1.0"
http = "1.0.3"

[dev]
port = 3000
backfill = false

[network_lexicons]
include = ["app.bsky.actor.profile"]

[labels]
subscribe = ["did:plc:ar7c4by46qjdydhdevvrndac"]

[jetstream]
protocol = "v1"
compression = false

[lexicons."at.happyproto.registry.getArtifact"]
target_collection = "at.happyproto.registry.artifact"

[lexicons."at.happyproto.registry.artifact"]
backfill = true

[lexicons."at.happyproto.registry.putArtifact"]
action = "create"
token_cost = 5

[scripts."xrpc.query:at.happyproto.registry.getArtifact"]
description = "Fetch one artifact"
outbound_xrpcs = ["com.atproto.repo.getRecord"]

[modules.registry]
language = "rust"
path = "modules/registry"
capabilities = ["library:call"]
"#;

    #[test]
    fn an_empty_file_is_the_default_config() {
        assert_eq!(parse("").unwrap(), ProjectConfig::default());
        let config = parse("").unwrap();
        assert!(!config.dev.backfill, "backfill is off unless asked for");
        assert_eq!(config.dev.port, None);
    }

    #[test]
    fn every_section_of_the_spec_example_parses() {
        let config = parse(FULL).unwrap();
        assert_eq!(config.happyview.version.as_deref(), Some("3.0.0"));
        assert_eq!(
            config.instance.app_name.as_deref(),
            Some("HappyProto Registry")
        );
        assert_eq!(
            config.instance.superadmin.as_deref(),
            Some("did:plc:ar7c4by46qjdydhdevvrndac")
        );
        assert_eq!(config.interpreters["lua"], "1.4.0");
        assert_eq!(config.libraries["happyview.record"], "2.1.0");
        assert_eq!(config.dev.port, Some(3000));
        assert_eq!(
            config.network_lexicons.include,
            vec!["app.bsky.actor.profile"]
        );
        assert_eq!(
            config.labels.subscribe,
            vec!["did:plc:ar7c4by46qjdydhdevvrndac"]
        );
        assert_eq!(config.jetstream.protocol, Some(JetstreamProtocol::V1));
        assert_eq!(config.jetstream.compression, Some(false));

        let get = &config.lexicons["at.happyproto.registry.getArtifact"];
        assert_eq!(
            get.target_collection.as_deref(),
            Some("at.happyproto.registry.artifact")
        );
        assert_eq!(get.backfill, None, "an unset override stays unset");
        assert_eq!(
            config.lexicons["at.happyproto.registry.artifact"].backfill,
            Some(true)
        );
        let put = &config.lexicons["at.happyproto.registry.putArtifact"];
        assert_eq!(put.action, Some(OverrideAction::Create));
        assert_eq!(put.token_cost, Some(5));

        let script = &config.scripts["xrpc.query:at.happyproto.registry.getArtifact"];
        assert_eq!(script.description.as_deref(), Some("Fetch one artifact"));
        assert_eq!(
            script.outbound_xrpcs.as_deref(),
            Some(&["com.atproto.repo.getRecord".to_string()][..])
        );

        let module = &config.modules["registry"];
        assert_eq!(module.language, "rust");
        assert_eq!(module.path, PathBuf::from("modules/registry"));
        assert_eq!(
            module.capabilities,
            vec![crate::plugin::capabilities::PluginCapability::LibraryCall]
        );
    }

    #[test]
    fn an_unknown_key_is_refused_with_its_file_and_line() {
        let err = parse_err("[instance]\napp_name = \"x\"\nsuperadmn = \"did:plc:abc\"\n");
        let ProjectConfigError::Parse {
            line,
            column,
            message,
            ..
        } = &err
        else {
            panic!("expected a parse error, got {err}");
        };
        assert_eq!(*line, Some(3));
        assert_eq!(*column, Some(1));
        assert!(message.contains("superadmn"), "{message}");
        assert!(err.to_string().starts_with("happyview.toml:3:1: "), "{err}");
    }

    #[test]
    fn an_unknown_section_is_refused() {
        let err = parse_err("[instanse]\napp_name = \"x\"\n");
        assert!(err.to_string().starts_with("happyview.toml:1:"), "{err}");
        assert!(err.to_string().contains("instanse"), "{err}");
    }

    #[test]
    fn an_unknown_key_inside_an_override_table_names_its_own_line() {
        let err = parse_err(
            "[lexicons.\"at.example.thing\"]\nbackfill = true\ntarget = \"at.example.other\"\n",
        );
        assert!(err.to_string().starts_with("happyview.toml:3:"), "{err}");
        assert!(err.to_string().contains("target"), "{err}");
    }

    #[test]
    fn an_unknown_action_names_the_valid_ones() {
        let err = parse_err("[lexicons.\"at.example.thing\"]\naction = \"replace\"\n");
        let text = err.to_string();
        assert!(text.starts_with("happyview.toml:2:"), "{text}");
        assert!(
            text.contains("replace") && text.contains("upsert"),
            "{text}"
        );
    }

    #[test]
    fn an_unknown_jetstream_protocol_names_its_line() {
        let err = parse_err("[jetstream]\nprotocol = \"v3\"\n");
        let text = err.to_string();
        assert!(text.starts_with("happyview.toml:2:"), "{text}");
        assert!(text.contains("v3") && text.contains("auto"), "{text}");
    }

    #[test]
    fn a_module_without_a_path_is_refused() {
        let err = parse_err("[modules.registry]\nlanguage = \"rust\"\n");
        assert!(err.to_string().contains("path"), "{err}");
    }

    #[test]
    fn the_action_override_maps_onto_the_procedure_action() {
        use crate::lexicon::ProcedureAction;
        assert_eq!(
            ProcedureAction::from(OverrideAction::Create),
            ProcedureAction::Create
        );
        assert_eq!(
            ProcedureAction::from(OverrideAction::Update),
            ProcedureAction::Update
        );
        assert_eq!(
            ProcedureAction::from(OverrideAction::Delete),
            ProcedureAction::Delete
        );
        assert_eq!(
            ProcedureAction::from(OverrideAction::Upsert),
            ProcedureAction::Upsert
        );
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let root = std::env::temp_dir().join(format!("hv-missing-{}", uuid::Uuid::new_v4()));
        let err = load_project_config(&root).unwrap_err();
        assert!(
            matches!(err, ProjectConfigError::RootNotFound { .. }),
            "{err}"
        );
        assert!(
            err.to_string().contains(&root.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn a_root_without_the_file_has_no_config() {
        let project = TempProject::new(None);
        assert_eq!(load_project_config(project.path()).unwrap(), None);
    }

    #[test]
    fn a_root_with_the_file_is_parsed_and_errors_name_its_path() {
        let project = TempProject::new(Some("[instance]\napp_name = \"Mine\"\n"));
        let config = load_project_config(project.path()).unwrap().unwrap();
        assert_eq!(config.instance.app_name.as_deref(), Some("Mine"));

        let broken = TempProject::new(Some("[instance]\nnope = 1\n"));
        let err = load_project_config(broken.path()).unwrap_err();
        let expected = format!("{}:2:1: ", broken.path().join(PROJECT_FILE).display());
        assert!(err.to_string().starts_with(&expected), "{err}");
    }

    fn assert_refused_at(text: &str, line: usize, needle: &str) {
        let err = parse_err(text);
        let rendered = err.to_string();
        assert!(
            rendered.starts_with(&format!("happyview.toml:{line}:")),
            "expected line {line}: {rendered}"
        );
        assert!(
            rendered.contains(needle),
            "expected {needle:?} in: {rendered}"
        );
    }

    #[test]
    fn a_superadmin_that_is_not_a_did_is_refused() {
        assert_refused_at(
            "[instance]\nsuperadmin = \"alice.bsky.social\"\n",
            2,
            "alice.bsky.social",
        );
    }

    #[test]
    fn a_bad_subscription_names_the_element_line_not_the_array() {
        assert_refused_at(
            "[labels]\nsubscribe = [\n  \"did:plc:ar7c4by46qjdydhdevvrndac\",\n  \"not-a-did\",\n]\n",
            4,
            "not-a-did",
        );
    }

    #[test]
    fn a_network_lexicon_that_is_not_an_nsid_is_refused() {
        assert_refused_at(
            "[network_lexicons]\ninclude = [\"profile\"]\n",
            2,
            "profile",
        );
    }

    #[test]
    fn a_lexicon_override_keyed_by_a_non_nsid_is_refused() {
        assert_refused_at(
            "[lexicons.\"getArtifact\"]\nbackfill = true\n",
            1,
            "getArtifact",
        );
    }

    #[test]
    fn a_target_collection_that_is_not_an_nsid_is_refused() {
        assert_refused_at(
            "[lexicons.\"at.example.get\"]\ntarget_collection = \"artifact\"\n",
            2,
            "artifact",
        );
    }

    #[test]
    fn a_script_override_keyed_by_a_bad_trigger_is_refused() {
        assert_refused_at(
            "[scripts.\"query:at.example.get\"]\ndescription = \"x\"\n",
            1,
            "query",
        );
    }

    #[test]
    fn a_job_trigger_with_a_reserved_prefix_is_refused() {
        assert_refused_at(
            "[scripts.\"job.run:happyview.delete-collection\"]\ndescription = \"x\"\n",
            1,
            "happyview.",
        );
    }

    #[test]
    fn an_interpreter_named_outside_the_language_id_shape_is_refused() {
        assert_refused_at("[interpreters]\nLua = \"1.4.0\"\n", 2, "Lua");
    }

    #[test]
    fn an_empty_version_is_refused() {
        assert_refused_at("[libraries]\nhttp = \"\"\n", 2, "empty");
        assert_refused_at("[happyview]\nversion = \" \"\n", 2, "empty");
    }

    #[test]
    fn an_empty_app_name_is_refused() {
        assert_refused_at("[instance]\napp_name = \"\"\n", 2, "empty");
    }

    #[test]
    fn port_zero_is_refused() {
        assert_refused_at("[dev]\nport = 0\n", 2, "port");
    }

    #[test]
    fn a_module_path_must_stay_inside_the_project() {
        for (path, line) in [
            ("../elsewhere", 3),
            ("/opt/module", 3),
            ("modules/../../x", 3),
        ] {
            assert_refused_at(
                &format!("[modules.registry]\nlanguage = \"rust\"\npath = \"{path}\"\n"),
                line,
                "inside",
            );
        }
        assert!(
            parse("[modules.registry]\nlanguage = \"rust\"\npath = \"./modules/registry\"\n")
                .is_ok()
        );
    }

    #[test]
    fn a_module_name_must_be_usable_as_a_file_name() {
        assert_refused_at(
            "[modules.\"My Module\"]\nlanguage = \"rust\"\npath = \"m\"\n",
            1,
            "My Module",
        );
    }

    #[test]
    fn a_module_language_outside_the_identifier_shape_is_refused() {
        assert_refused_at(
            "[modules.m]\nlanguage = \"Rust\"\npath = \"m\"\n",
            2,
            "Rust",
        );
    }

    #[test]
    fn the_full_example_still_parses_under_validation() {
        parse(FULL).unwrap();
    }

    #[test]
    fn the_env_layer_names_the_variables_the_file_stands_in_for() {
        let config = parse(FULL).unwrap();
        let serve = config.env_layer(ConfigMode::Serve);
        assert_eq!(
            serve.get("APP_NAME").map(String::as_str),
            Some("HappyProto Registry")
        );
        assert_eq!(
            serve.get("SUPERADMIN_DID").map(String::as_str),
            Some("did:plc:ar7c4by46qjdydhdevvrndac")
        );
        assert_eq!(
            serve.get("PORT"),
            None,
            "[dev] never reaches a production deploy"
        );
        assert_eq!(
            serve.get("JETSTREAM_PROTOCOL").map(String::as_str),
            Some("v1")
        );
        assert_eq!(
            serve.get("JETSTREAM_COMPRESSION").map(String::as_str),
            Some("false")
        );

        let dev = config.env_layer(ConfigMode::Dev);
        assert_eq!(dev.get("PORT").map(String::as_str), Some("3000"));
        assert_eq!(
            dev.get("HAPPYVIEW_DEV_BACKFILL").map(String::as_str),
            Some("false")
        );
        assert_eq!(serve.get("HAPPYVIEW_DEV_BACKFILL"), None);
    }

    #[test]
    fn an_empty_file_contributes_no_serve_variables() {
        assert!(parse("").unwrap().env_layer(ConfigMode::Serve).is_empty());
    }

    #[test]
    fn a_pinned_version_that_differs_is_reported() {
        let config = parse("[happyview]\nversion = \"3.0.0\"\n").unwrap();
        let message = config.version_mismatch("3.1.0").unwrap();
        assert!(
            message.contains("3.0.0") && message.contains("3.1.0"),
            "{message}"
        );
        assert_eq!(config.version_mismatch("3.0.0"), None);
        let with_v = parse("[happyview]\nversion = \"v3.0.0\"\n").unwrap();
        assert_eq!(
            with_v.version_mismatch("3.0.0"),
            None,
            "a leading v is the same version"
        );
        assert_eq!(parse("").unwrap().version_mismatch("3.0.0"), None);
    }

    #[test]
    fn declared_layers_answer_membership() {
        let config = parse(FULL).unwrap();
        assert!(config.declares_labeler("did:plc:ar7c4by46qjdydhdevvrndac"));
        assert!(!config.declares_labeler("did:plc:other"));
        assert!(config.declares_network_lexicon("app.bsky.actor.profile"));
        assert!(!config.declares_network_lexicon("app.bsky.feed.post"));
    }
}
