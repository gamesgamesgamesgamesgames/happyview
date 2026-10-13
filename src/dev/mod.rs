//! `happyview dev`: the production binary with local defaults.

pub mod env;
pub mod instance;
pub mod report;
pub mod secrets;

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::config::project::ConfigMode;

pub struct DevSession {
    pub root: PathBuf,
    pub public_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DevError {
    #[error("{} is not a project directory: {source}", path.display())]
    Root {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Secrets(String),
}

/// Resolve the project and its secrets, and load the configuration with the
/// dev defaults beneath the file and the environment.
pub fn prepare(root: &Path) -> Result<(DevSession, Config), DevError> {
    let root = std::fs::canonicalize(root).map_err(|source| DevError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    if !root.is_dir() {
        return Err(DevError::Root {
            path: root.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory"),
        });
    }
    let secrets = secrets::load_or_create(&root.join(".happyview")).map_err(DevError::Secrets)?;
    let config = load_config(&root, &secrets)?;
    let session = DevSession {
        public_url: config.public_url.clone(),
        root,
    };
    Ok((session, config))
}

/// Also what a `happyview.toml` edit reloads through, so a reload sees the
/// same defaults the boot did.
pub fn load_config(root: &Path, secrets: &secrets::Secrets) -> Result<Config, DevError> {
    Config::load_from_with(Some(root), ConfigMode::Dev, |view| {
        env::defaults(root, secrets, view)
    })
    .map_err(|e| DevError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::vars;
    use serial_test::serial;

    /// The file's port beats the default, the defaults fill the rest, and the
    /// secrets generated for the project reach the config.
    #[test]
    #[serial]
    fn prepare_layers_defaults_beneath_the_file() {
        let _reset = vars::LayerReset;
        for key in [
            "HOST",
            "PORT",
            "DATABASE_URL",
            "PUBLIC_URL",
            "SESSION_SECRET",
        ] {
            unsafe { std::env::remove_var(key) };
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("happyview.toml"), "[dev]\nport = 4321\n").unwrap();

        let (session, config) = prepare(dir.path()).unwrap();

        assert_eq!(config.port, 4321);
        assert_eq!(session.public_url, "http://127.0.0.1:4321");
        assert!(config.database_url.starts_with("sqlite://"));
        assert!(config.database_url.contains("/.happyview/happyview.db"));
        let secrets = secrets::load_or_create(&session.root.join(".happyview")).unwrap();
        assert_eq!(config.session_secret, secrets.session_secret);
    }

    #[test]
    fn a_missing_root_names_the_path() {
        let err = prepare(Path::new("/definitely/not/here")).err().unwrap();
        assert!(
            err.to_string()
                .starts_with("/definitely/not/here is not a project directory")
        );
    }
}
