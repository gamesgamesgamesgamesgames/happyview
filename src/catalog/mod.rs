//! The only readers and writers of the lexicon and script tables.
//!
//! Each catalog layers its sources in a fixed order, project then manual then
//! network, and a read answers from the first layer holding the id. The
//! project layer is the installed snapshot and has no write methods: the only
//! way to change it is to swap the snapshot.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

pub mod lexicon_rows;
pub mod lexicons;
pub mod script_rows;
pub mod scripts;

pub use lexicons::{
    LexiconCatalog, LexiconListing, LexiconWrite, NetworkLexicon, NetworkWriteError,
};
pub use scripts::{DASHBOARD_CHUNK_NAME, ManualScript, ScriptCatalog, ScriptListing, ScriptWrite};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Project,
    Manual,
    Network,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Project => "project",
            Layer::Manual => "manual",
            Layer::Network => "network",
        }
    }

    /// The layer a stored row's `source` column names. Anything other than
    /// `network` is manual, which is what the column's default has always meant.
    pub fn from_source(source: &str) -> Self {
        if source == "network" {
            Layer::Network
        } else {
            Layer::Manual
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry<T> {
    pub value: T,
    pub layer: Layer,
    /// Project-relative file path, for `Layer::Project` only.
    pub path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The id is owned by a project file.
    #[error(
        "defined by the project file {}; change that file instead",
        path_string(path)
    )]
    ProjectOwned { path: PathBuf },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Db(String),
}

impl From<sqlx::Error> for CatalogError {
    fn from(e: sqlx::Error) -> Self {
        CatalogError::Db(e.to_string())
    }
}

impl From<CatalogError> for AppError {
    fn from(e: CatalogError) -> Self {
        let message = e.to_string();
        match e {
            CatalogError::ProjectOwned { .. } => AppError::Conflict(message),
            CatalogError::Invalid(_) => AppError::BadRequest(message),
            CatalogError::Db(_) => AppError::Internal(message),
        }
    }
}

/// A project path as clients and error messages show it. Joined by hand
/// because `Path::display` uses the platform separator, and these strings
/// must read the same on every platform.
pub fn path_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn a_project_owned_entry_answers_409_naming_the_file() {
        let err: AppError = CatalogError::ProjectOwned {
            path: PathBuf::from("src/lexicons/com/example/thing.json"),
        }
        .into();
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("src/lexicons/com/example/thing.json"),
            "{body}"
        );
    }

    #[test]
    fn invalid_is_a_400_and_db_is_a_500() {
        assert!(matches!(
            AppError::from(CatalogError::Invalid("bad".into())),
            AppError::BadRequest(m) if m == "bad"
        ));
        assert!(matches!(
            AppError::from(CatalogError::Db("down".into())),
            AppError::Internal(m) if m == "down"
        ));
    }

    #[test]
    fn layers_order_project_first() {
        assert!(Layer::Project < Layer::Manual);
        assert!(Layer::Manual < Layer::Network);
    }

    #[test]
    fn a_network_source_is_the_network_layer_and_anything_else_manual() {
        assert_eq!(Layer::from_source("network"), Layer::Network);
        assert_eq!(Layer::from_source("manual"), Layer::Manual);
        assert_eq!(Layer::from_source(""), Layer::Manual);
    }

    #[test]
    fn a_path_string_uses_forward_slashes_whatever_the_platform() {
        let path: PathBuf = ["src", "lexicons", "com", "example", "thing.json"]
            .iter()
            .collect();
        assert_eq!(path_string(&path), "src/lexicons/com/example/thing.json");
    }

    #[test]
    fn layers_serialise_lowercase() {
        assert_eq!(serde_json::to_value(Layer::Project).unwrap(), "project");
    }
}
