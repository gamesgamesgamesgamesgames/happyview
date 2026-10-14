//! What `happyview dev` supplies when neither the environment nor
//! `happyview.toml` does.

use std::collections::BTreeMap;
use std::path::Path;

use crate::dev::secrets::Secrets;

pub const DEFAULT_PORT: u16 = 3000;

/// Quieter than the server default: in dev the build lines are the output a
/// person reads, and per-request tracing buries them.
pub const DEV_LOG_FILTER: &str = "happyview=info,tower_http=warn,sqlx=warn";

pub fn sqlite_url(path: &Path) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        path.to_string_lossy().replace('\\', "/")
    )
}

/// `view` is the environment merged over the file, so the port it holds is
/// already the one in use.
pub fn defaults(
    root: &Path,
    secrets: &Secrets,
    view: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let state_dir = root.join(".happyview");
    let port = view
        .get("PORT")
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    [
        ("DATABASE_URL", sqlite_url(&state_dir.join("happyview.db"))),
        // Loopback only: a dev instance has generated secrets and a
        // config-named superadmin, and should not be reachable from the LAN.
        ("HOST", "127.0.0.1".to_string()),
        ("PORT", port.to_string()),
        // 127.0.0.1, not localhost: atrium's loopback OAuth client requires it.
        ("PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("SESSION_SECRET", secrets.session_secret.clone()),
        ("TOKEN_ENCRYPTION_KEY", secrets.token_encryption_key.clone()),
        (
            "PLUGIN_DIR",
            state_dir.join("plugins").to_string_lossy().into_owned(),
        ),
        (
            "HAPPYVIEW_SCRIPT_ENV_FILE",
            root.join(".env").to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn secrets() -> Secrets {
        Secrets {
            session_secret: "s".repeat(64),
            token_encryption_key: "k".into(),
        }
    }

    fn map(vars: Vec<(String, String)>) -> HashMap<String, String> {
        vars.into_iter().collect()
    }

    #[test]
    fn every_default_from_an_empty_view() {
        let d = map(defaults(
            Path::new("/work/app"),
            &secrets(),
            &BTreeMap::new(),
        ));
        assert_eq!(
            d["DATABASE_URL"],
            "sqlite:///work/app/.happyview/happyview.db?mode=rwc"
        );
        assert_eq!(d["HOST"], "127.0.0.1");
        assert_eq!(d["PORT"], "3000");
        assert_eq!(d["PUBLIC_URL"], "http://127.0.0.1:3000");
        assert_eq!(d["SESSION_SECRET"], "s".repeat(64));
        assert_eq!(d["TOKEN_ENCRYPTION_KEY"], "k");
        assert_eq!(d["PLUGIN_DIR"], "/work/app/.happyview/plugins");
        assert_eq!(d["HAPPYVIEW_SCRIPT_ENV_FILE"], "/work/app/.env");
    }

    #[test]
    fn the_public_url_follows_the_port_in_use() {
        for port in ["4321", "5000"] {
            let view = BTreeMap::from([("PORT".to_string(), port.to_string())]);
            let d = map(defaults(Path::new("/p"), &secrets(), &view));
            assert_eq!(d["PUBLIC_URL"], format!("http://127.0.0.1:{port}"));
        }
    }

    #[test]
    fn sqlite_urls_use_forward_slashes() {
        assert_eq!(
            sqlite_url(Path::new("/a/b.db")),
            "sqlite:///a/b.db?mode=rwc"
        );
        assert_eq!(
            sqlite_url(Path::new(r"C:\a\b.db")),
            "sqlite://C:/a/b.db?mode=rwc"
        );
    }
}
