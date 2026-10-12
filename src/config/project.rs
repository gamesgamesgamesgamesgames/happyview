//! `happyview.toml`, the configuration a project directory carries.
//!
//! The file is optional, and every environment variable wins over it: the
//! values it supplies reach `Config` as the variables they stand in for,
//! through `config::vars`, so precedence is decided in one place.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
    pub interpreters: BTreeMap<String, String>,
    pub libraries: BTreeMap<String, String>,
    pub dev: DevSection,
    pub network_lexicons: NetworkLexiconsSection,
    pub labels: LabelsSection,
    pub jetstream: JetstreamSection,
    pub lexicons: BTreeMap<String, LexiconOverride>,
    pub scripts: BTreeMap<String, ScriptOverride>,
    pub modules: BTreeMap<String, ModuleDecl>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HappyviewSection {
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstanceSection {
    pub app_name: Option<String>,
    pub superadmin: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevSection {
    pub port: Option<u16>,
    pub backfill: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkLexiconsSection {
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LabelsSection {
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

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptOverride {
    pub description: Option<String>,
    pub outbound_xrpcs: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleDecl {
    pub language: String,
    pub path: PathBuf,
    #[serde(default)]
    pub capabilities: Vec<crate::plugin::capabilities::PluginCapability>,
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
}
