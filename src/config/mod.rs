use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::db::DatabaseBackend;

pub mod declarations;
pub mod project;
pub use happyview_provider::vars;

pub use project::ConfigMode;
use project::ProjectConfig;
use std::collections::BTreeMap;
use std::env;
use vars::var;

/// Placeholder session secrets that have shipped in this repo's docs/examples.
/// Booting with any of them is refused — the cookie signing key is derived from
/// `SESSION_SECRET`, so a known value lets anyone forge a validly-signed admin
/// session cookie.
const INSECURE_SESSION_SECRETS: &[&str] = &[
    "change-me-in-production-not-secure",
    "change-me-in-production",
];

/// Minimum acceptable `SESSION_SECRET` length in bytes. `Key::derive_from` also
/// requires at least 32 bytes; enforcing it here yields a clear error instead of
/// a downstream panic.
const MIN_SESSION_SECRET_BYTES: usize = 32;

/// Validate a session secret, rejecting known placeholder values and anything
/// too short to be secure. Returns a human-readable reason on failure.
pub(crate) fn validate_session_secret(secret: &str) -> Result<(), String> {
    if secret.is_empty() {
        return Err(
            "SESSION_SECRET is not set. Generate a random value of at least 32 bytes \
             (e.g. `openssl rand -base64 48`) and set SESSION_SECRET."
                .into(),
        );
    }
    if INSECURE_SESSION_SECRETS.contains(&secret) {
        return Err(
            "SESSION_SECRET is set to a known insecure default. Generate a random \
             value of at least 32 bytes (e.g. `openssl rand -base64 48`)."
                .into(),
        );
    }
    if secret.len() < MIN_SESSION_SECRET_BYTES {
        return Err(format!(
            "SESSION_SECRET must be at least {MIN_SESSION_SECRET_BYTES} bytes (got {}). \
             Generate a random value (e.g. `openssl rand -base64 48`).",
            secret.len()
        ));
    }
    Ok(())
}

/// Build the outbound `User-Agent`.
///
/// Something issuing requests to hundreds of strangers' servers should say who
/// it is. `HAPPYVIEW_USER_AGENT` overrides entirely, so an operator can add a
/// contact address or suppress the instance URL.
pub fn build_user_agent(override_value: Option<String>, public_url: &str) -> String {
    if let Some(ua) = override_value {
        let trimmed = ua.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let version = crate::version::version();
    let public_url = public_url.trim().trim_end_matches('/');
    if public_url.is_empty() {
        format!("HappyView/{version}")
    } else {
        format!("HappyView/{version} (+{public_url})")
    }
}

/// Parse the optional `TOKEN_ENCRYPTION_KEY`, distinguishing "unset"
/// (`Ok(None)` — encryption-dependent features are simply off) from "set but
/// invalid" (`Err`), so a botched key is reported loudly at startup instead of
/// being silently discarded and failing per-call later (M12).
pub fn parse_token_encryption_key(raw: Option<&str>) -> Result<Option<[u8; 32]>, String> {
    use base64::Engine;
    let raw = match raw {
        None | Some("") => return Ok(None),
        Some(s) => s,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| format!("TOKEN_ENCRYPTION_KEY is not valid base64: {e}"))?;
    let len = bytes.len();
    <[u8; 32]>::try_from(bytes)
        .map(Some)
        .map_err(|_| format!("TOKEN_ENCRYPTION_KEY must decode to exactly 32 bytes (got {len})"))
}

/// Parse `PLATFORM_API_KEY_HASH`: the hex SHA-256 of the key a managed-hosting
/// provisioner uses to administer this instance. Unset or blank means the
/// platform principal is disabled. Set-but-invalid is an operator error and is
/// returned as `Err` so it can be reported loudly at startup.
pub fn parse_platform_api_key_hash(raw: Option<&str>) -> Result<Option<String>, String> {
    let trimmed = match raw.map(str::trim) {
        None | Some("") => return Ok(None),
        Some(s) => s,
    };
    let lower = trimmed.to_ascii_lowercase();
    if lower.len() != 64 || !lower.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "PLATFORM_API_KEY_HASH must be 64 hex characters (a SHA-256 digest), got {} characters",
            lower.len()
        ));
    }
    Ok(Some(lower))
}

/// Upper bound on `JOB_WORKER_CONCURRENCY`. The cap bounds concurrent job load
/// on the database pool, so an unbounded value could starve the request path.
const MAX_JOB_WORKERS: usize = 32;

/// Parse `HAPPYVIEW_DEV_BACKFILL`. Off when unset: in dev, a record lexicon
/// saved with `backfill` would otherwise start a relay crawl on every save.
/// Only the two values the project file's layer and `hpy` write are accepted,
/// so a typo is reported instead of silently meaning "off".
pub fn parse_dev_backfill(raw: Option<&str>) -> Result<bool, String> {
    match raw {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(other) => Err(format!(
            "HAPPYVIEW_DEV_BACKFILL must be `true` or `false`, got {other:?}"
        )),
    }
}

/// Parse `JOB_WORKER_CONCURRENCY`. Defaults to 1 (the historical behaviour);
/// anything unparseable or below 1 falls back to 1; values above
/// `MAX_JOB_WORKERS` clamp to it.
pub fn parse_job_worker_concurrency(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n >= 1)
        .map(|n| (n as usize).min(MAX_JOB_WORKERS))
        .unwrap_or(1)
}

/// Parse `SUPERADMIN_DID`, the account seeded as a super user at boot.
/// Unset or blank seeds nobody and leaves the first-login bootstrap in
/// charge.
pub fn parse_superadmin_did(raw: Option<&str>) -> Result<Option<String>, String> {
    let trimmed = match raw.map(str::trim) {
        None | Some("") => return Ok(None),
        Some(s) => s,
    };
    atrium_api::types::string::Did::new(trimmed.to_string())
        .map(|did| Some(did.as_str().to_string()))
        .map_err(|e| format!("SUPERADMIN_DID is not a valid DID ('{trimmed}'): {e}"))
}

/// Environment entries whose name and value are both UTF-8; the rest cannot
/// name a configuration variable and must not abort boot.
pub fn utf8_env(
    entries: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> impl Iterator<Item = (String, String)> {
    entries.filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
}

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub database_backend: DatabaseBackend,
    pub sqlite_journal_size_limit: u64,
    pub public_url: String,
    pub user_agent: String,
    pub session_secret: String,
    pub jetstream_url: String,
    pub telemetry_collector_url: String,
    pub relay_url: String,
    pub plc_url: String,
    pub static_dir: Option<String>,
    pub base_path: Option<String>,
    pub event_log_retention_days: u32,
    pub app_name: Option<String>,
    pub logo_uri: Option<String>,
    pub tos_uri: Option<String>,
    pub policy_uri: Option<String>,
    pub token_encryption_key: Option<[u8; 32]>,
    pub default_rate_limit_capacity: u32,
    pub default_rate_limit_refill_rate: f64,
    /// Where compiled plugin modules are kept between restarts, when there
    /// is a directory this instance owns. `None` keeps the cache in memory.
    pub plugin_cache_dir: Option<PathBuf>,
    /// Hex SHA-256 of the managed-hosting platform key. `None` disables the
    /// platform principal entirely.
    pub platform_api_key_hash: Option<String>,
    /// Number of background job workers. 1 unless `JOB_WORKER_CONCURRENCY` says
    /// otherwise.
    pub job_worker_concurrency: usize,
    /// The account seeded as a super user at boot, from `SUPERADMIN_DID` or
    /// the project's `[instance] superadmin`.
    pub superadmin_did: Option<String>,
    /// The project directory, when this instance serves one.
    pub project_root: Option<PathBuf>,
    /// The project's `happyview.toml`, when it has one.
    pub project: Option<Arc<ProjectConfig>>,
    /// Where boot looks for plugin directories (`<dir>/*/manifest.json`).
    pub plugin_dir: PathBuf,
    /// Whether a backfill may start as a side effect: of uploading or
    /// tracking a record lexicon with `backfill` set, or of a project's
    /// declared backfill. A backfill created directly (`POST /admin/backfill`,
    /// the dashboard's Backfill page) always runs.
    pub auto_backfill: bool,
    /// A dotenv file whose variables reach scripts as `ctx.env`, over the
    /// database's. Read on every run, so an edit needs no restart.
    pub script_env_file: Option<PathBuf>,
}

/// `PLUGIN_CACHE_DIR` when set; otherwise a `plugin-cache` directory beside
/// the SQLite database, since that is where this instance already keeps
/// state; otherwise none. There is no temp-directory fallback: the cache
/// feeds `Module::deserialize`, which runs whatever it is handed as native
/// code, so the directory has to be one nobody else can write to, and a
/// shared temp directory can be pre-created and owned by any local user.
pub fn plugin_cache_dir(override_dir: Option<String>, database_url: &str) -> Option<PathBuf> {
    if let Some(dir) = override_dir.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let db_path = crate::db::sqlite_path_from_url(database_url)?;
    let parent = db_path.parent().map(Path::to_path_buf).unwrap_or_default();
    Some(parent.join("plugin-cache"))
}

impl Config {
    /// Build the configuration for this process. `HAPPYVIEW_PROJECT` names
    /// a project root; without it the instance runs from the environment
    /// alone, as it always has.
    pub fn load() -> Result<Self, project::ProjectConfigError> {
        let root = env::var_os("HAPPYVIEW_PROJECT")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        Self::load_from(root.as_deref(), ConfigMode::Serve)
    }

    pub fn load_from(
        root: Option<&Path>,
        mode: ConfigMode,
    ) -> Result<Self, project::ProjectConfigError> {
        Self::load_from_with(root, mode, |_| Vec::new())
    }

    /// Install the project's settings as the layer beneath the environment,
    /// then read the configuration through it. `defaults` sees the merged
    /// environment-and-file view and answers values to place beneath both:
    /// they live in the layer, never in the process environment, so
    /// environment > file > defaults holds for every reader.
    pub fn load_from_with(
        root: Option<&Path>,
        mode: ConfigMode,
        defaults: impl FnOnce(&BTreeMap<String, String>) -> Vec<(String, String)>,
    ) -> Result<Self, project::ProjectConfigError> {
        let project = match root {
            Some(root) => project::load_project_config(root)?,
            None => None,
        };
        let mut layer = project
            .as_ref()
            .map(|p| p.env_layer(mode))
            .unwrap_or_default();
        let mut view: BTreeMap<String, String> = layer.clone();
        view.extend(utf8_env(env::vars_os()));
        for (name, value) in defaults(&view) {
            layer.entry(name).or_insert(value);
        }
        // The checks below read through the layer, so the candidate has to be
        // installed to be validated. A refused load restores the previous
        // layer: a failed dev reload must not leave the rejected edit live
        // for request-time readers. Validating against a private copy would
        // mean a second lookup path beside `var`.
        let previous = vars::project_layer();
        vars::set_project_layer(layer);
        let result = Self::finish_load(root, mode, project);
        if result.is_err() {
            vars::set_project_layer(previous);
        }
        result
    }

    fn finish_load(
        root: Option<&Path>,
        mode: ConfigMode,
        project: Option<project::ProjectConfig>,
    ) -> Result<Self, project::ProjectConfigError> {
        // An unusable superadmin must not read as "none configured": that
        // would hand the first login the admin role.
        if let Err(message) = parse_superadmin_did(var("SUPERADMIN_DID").as_deref()) {
            return Err(project::ProjectConfigError::InvalidSetting { message });
        }
        if let Some(message) = project
            .as_ref()
            .and_then(|p| p.version_mismatch(crate::version::version()))
        {
            tracing::warn!("{message}");
        }
        let mut config = Self::from_env();
        config.auto_backfill = match mode {
            project::ConfigMode::Serve => true,
            project::ConfigMode::Dev => {
                parse_dev_backfill(var("HAPPYVIEW_DEV_BACKFILL").as_deref())
                    .map_err(|message| project::ProjectConfigError::InvalidSetting { message })?
            }
        };
        config.project_root = root.map(Path::to_path_buf);
        config.project = project.map(Arc::new);
        Ok(config)
    }

    pub fn declares_labeler(&self, did: &str) -> bool {
        self.project
            .as_deref()
            .is_some_and(|p| p.declares_labeler(did))
    }

    pub fn declares_network_lexicon(&self, nsid: &str) -> bool {
        self.project
            .as_deref()
            .is_some_and(|p| p.declares_network_lexicon(nsid))
    }

    pub fn from_env() -> Self {
        let database_url = var("DATABASE_URL").expect("DATABASE_URL must be set");
        let database_backend = var("DATABASE_BACKEND")
            .and_then(|s| DatabaseBackend::from_str(&s))
            .unwrap_or_else(|| DatabaseBackend::from_url(&database_url));

        let public_url = var("PUBLIC_URL").expect("PUBLIC_URL must be set");

        Self {
            host: var("HOST").unwrap_or_else(|| "0.0.0.0".into()),
            port: var("PORT").and_then(|p| p.parse().ok()).unwrap_or(3000),
            plugin_cache_dir: plugin_cache_dir(var("PLUGIN_CACHE_DIR"), &database_url),
            database_url,
            database_backend,
            sqlite_journal_size_limit: crate::db::journal_size_limit_bytes(),
            user_agent: build_user_agent(var("HAPPYVIEW_USER_AGENT"), &public_url),
            public_url,
            // Not required and never defaulted to a placeholder: an unset,
            // insecure, or too-short value is surfaced via `config_errors()` and
            // disables cookie auth rather than aborting boot. See C3.
            session_secret: var("SESSION_SECRET").unwrap_or_default(),
            jetstream_url: var("JETSTREAM_URL")
                .unwrap_or_else(|| "wss://jetstream1.us-east.bsky.network".into()),
            telemetry_collector_url: var("TELEMETRY_COLLECTOR_URL")
                .unwrap_or_else(|| "https://telemetry.happyview.dev/v1/snapshot".to_string()),
            relay_url: var("RELAY_URL").unwrap_or_else(|| "https://bsky.network".into()),
            plc_url: var("PLC_URL").unwrap_or_else(|| "https://plc.directory".into()),
            static_dir: var("STATIC_DIR").filter(|s| !s.is_empty()),
            base_path: var("BASE_PATH").and_then(|s| {
                let s = s.trim_end_matches('/').to_string();
                if s.is_empty() {
                    None
                } else if !s.starts_with('/') {
                    panic!("BASE_PATH must start with '/' (got: {s})");
                } else {
                    Some(s)
                }
            }),
            event_log_retention_days: var("EVENT_LOG_RETENTION_DAYS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            app_name: var("APP_NAME"),
            logo_uri: var("LOGO_URI"),
            tos_uri: var("TOS_URI"),
            policy_uri: var("POLICY_URI"),
            token_encryption_key: match parse_token_encryption_key(
                var("TOKEN_ENCRYPTION_KEY").as_deref(),
            ) {
                Ok(key) => key,
                Err(reason) => {
                    // Set-but-invalid is an operator mistake: surface it loudly at
                    // startup instead of silently disabling encryption.
                    tracing::error!(
                        "{reason}. The key is being IGNORED — encryption-dependent features \
                         (DPoP sessions, spaces, service identity) are DISABLED until it is fixed."
                    );
                    None
                }
            },
            default_rate_limit_capacity: var("DEFAULT_RATE_LIMIT_CAPACITY")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
            default_rate_limit_refill_rate: var("DEFAULT_RATE_LIMIT_REFILL_RATE")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2.0),
            platform_api_key_hash: match parse_platform_api_key_hash(
                var("PLATFORM_API_KEY_HASH").as_deref(),
            ) {
                Ok(hash) => hash,
                Err(reason) => {
                    tracing::error!(
                        "{reason}. The platform principal is DISABLED until it is fixed."
                    );
                    None
                }
            },
            job_worker_concurrency: parse_job_worker_concurrency(
                var("JOB_WORKER_CONCURRENCY").as_deref(),
            ),
            superadmin_did: match parse_superadmin_did(var("SUPERADMIN_DID").as_deref()) {
                Ok(did) => did,
                Err(reason) => {
                    tracing::error!("{reason}. No superadmin is seeded until it is fixed.");
                    None
                }
            },
            project_root: None,
            project: None,
            plugin_dir: var("PLUGIN_DIR")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./plugins")),
            auto_backfill: true,
            script_env_file: var("HAPPYVIEW_SCRIPT_ENV_FILE")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        }
    }

    /// Whether the configured `SESSION_SECRET` is safe to derive the cookie
    /// signing key from. When `false`, cookie-based auth is disabled (see the
    /// auth extractors and login handlers) because the signing key would be
    /// forgeable.
    pub fn session_secret_secure(&self) -> bool {
        validate_session_secret(&self.session_secret).is_ok()
    }

    /// Human-readable configuration problems detected at startup, surfaced to
    /// the dashboard (via `/config`) so an operator can fix them. Empty when the
    /// instance is configured correctly.
    pub fn config_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Err(e) = validate_session_secret(&self.session_secret) {
            errors.push(e);
        }
        errors
    }

    pub fn listen_addr(&self) -> SocketAddr {
        format!("{}:{}", self.host, self.port)
            .parse()
            .expect("invalid HOST/PORT")
    }

    pub fn effective_public_url(&self) -> String {
        match &self.base_path {
            Some(bp) => format!("{}{}", self.public_url.trim_end_matches('/'), bp),
            None => self.public_url.clone(),
        }
    }

    pub fn instance_client_id_url(&self) -> String {
        format!(
            "{}/oauth-client-metadata.json",
            self.effective_public_url().trim_end_matches('/')
        )
    }

    pub fn url_with_base_path(&self, domain_url: &str) -> String {
        match &self.base_path {
            Some(bp) => format!("{}{}", domain_url.trim_end_matches('/'), bp),
            None => domain_url.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe { env::remove_var(self.0) };
        }
    }

    unsafe fn clear_env() {
        for key in [
            "HOST",
            "PORT",
            "DATABASE_URL",
            "DATABASE_BACKEND",
            "PUBLIC_URL",
            "SESSION_SECRET",
            "JETSTREAM_URL",
            "RELAY_URL",
            "PLC_URL",
            "EVENT_LOG_RETENTION_DAYS",
            "APP_NAME",
            "LOGO_URI",
            "TOS_URI",
            "POLICY_URI",
            "BASE_PATH",
            "PLATFORM_API_KEY_HASH",
            "JOB_WORKER_CONCURRENCY",
            "SUPERADMIN_DID",
            "HAPPYVIEW_PROJECT",
            "PLUGIN_DIR",
            "HAPPYVIEW_SCRIPT_ENV_FILE",
            "HAPPYVIEW_DEV_BACKFILL",
        ] {
            unsafe {
                env::remove_var(key);
            }
        }
        vars::set_project_layer(std::collections::BTreeMap::new());
    }

    /// Exactly the two values every writer of this variable uses; a typo
    /// is refused rather than read as "off", which would hide it.
    #[test]
    fn dev_backfill_accepts_true_and_false_and_nothing_else() {
        assert_eq!(parse_dev_backfill(None), Ok(false));
        assert_eq!(parse_dev_backfill(Some("true")), Ok(true));
        assert_eq!(parse_dev_backfill(Some("false")), Ok(false));
        for bad in ["TRUE", "False", "1", "0", "yes", "on", "", " true"] {
            let err = parse_dev_backfill(Some(bad)).unwrap_err();
            assert!(err.contains("HAPPYVIEW_DEV_BACKFILL"), "{err}");
            assert!(err.contains("`true` or `false`"), "{err}");
        }
    }

    #[test]
    #[serial]
    fn from_env_reads_the_plugin_dir() {
        unsafe {
            clear_env();
            set_required_env();
        }
        assert_eq!(Config::from_env().plugin_dir, PathBuf::from("./plugins"));
        unsafe { env::set_var("PLUGIN_DIR", "/srv/project/.happyview/plugins") };
        assert_eq!(
            Config::from_env().plugin_dir,
            PathBuf::from("/srv/project/.happyview/plugins")
        );
        unsafe { clear_env() };
    }

    #[test]
    #[serial]
    fn from_env_reads_the_script_env_file() {
        unsafe {
            clear_env();
            set_required_env();
        }
        assert_eq!(Config::from_env().script_env_file, None);
        unsafe { env::set_var("HAPPYVIEW_SCRIPT_ENV_FILE", "") };
        assert_eq!(Config::from_env().script_env_file, None);
        unsafe { env::set_var("HAPPYVIEW_SCRIPT_ENV_FILE", "/srv/project/.env") };
        assert_eq!(
            Config::from_env().script_env_file,
            Some(PathBuf::from("/srv/project/.env"))
        );
        unsafe { clear_env() };
    }

    /// Serve always backfills on its own; dev only when the file or the
    /// environment says so, and the environment wins.
    #[test]
    #[serial]
    fn auto_backfill_follows_the_mode() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let on = project::TempProject::new(Some("[dev]\nbackfill = true\n"));
        let empty = project::TempProject::new(None);
        let load = |dir: &project::TempProject, mode| Config::load_from(Some(dir.path()), mode);

        assert!(
            load(&empty, project::ConfigMode::Serve)
                .unwrap()
                .auto_backfill
        );
        assert!(
            !load(&empty, project::ConfigMode::Dev)
                .unwrap()
                .auto_backfill
        );
        assert!(load(&on, project::ConfigMode::Dev).unwrap().auto_backfill);

        unsafe { env::set_var("HAPPYVIEW_DEV_BACKFILL", "false") };
        assert!(!load(&on, project::ConfigMode::Dev).unwrap().auto_backfill);

        unsafe { env::set_var("HAPPYVIEW_DEV_BACKFILL", "1") };
        let err = load(&on, project::ConfigMode::Dev).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("HAPPYVIEW_DEV_BACKFILL must be"),
            "{err}"
        );
        // Serve never reads it, so a stray value can't stop a production boot.
        assert!(load(&on, project::ConfigMode::Serve).unwrap().auto_backfill);
        unsafe { clear_env() };
    }

    unsafe fn set_required_env() {
        unsafe {
            env::set_var("DATABASE_URL", "postgres://localhost/test");
            env::set_var("PUBLIC_URL", "http://127.0.0.1:3000");
        }
    }

    /// The override wins; a SQLite instance keeps the cache beside its
    /// database; anything else gets a directory of its own under temp.
    #[test]
    fn plugin_cache_dir_resolves_in_order() {
        assert_eq!(
            plugin_cache_dir(Some("/var/cache/hv".into()), "sqlite://data/happyview.db"),
            Some(PathBuf::from("/var/cache/hv"))
        );
        assert_eq!(
            plugin_cache_dir(Some(String::new()), "sqlite://data/happyview.db?mode=rwc"),
            Some(PathBuf::from("data/plugin-cache"))
        );
        assert_eq!(
            plugin_cache_dir(None, "sqlite:/var/lib/hv.db"),
            Some(PathBuf::from("/var/lib/plugin-cache"))
        );
        assert_eq!(plugin_cache_dir(None, "postgres://localhost/hv"), None);
        assert_eq!(plugin_cache_dir(None, "sqlite://:memory:"), None);
    }

    /// The override wins on every backend; without it a Postgres instance
    /// has no directory of its own and keeps the cache in memory.
    #[test]
    fn plugin_cache_dir_has_no_temp_fallback() {
        assert_eq!(
            plugin_cache_dir(Some("/srv/cache".into()), "postgres://localhost/hv"),
            Some(PathBuf::from("/srv/cache"))
        );
        assert_eq!(plugin_cache_dir(None, "postgres://localhost/hv"), None);
    }

    #[test]
    fn listen_addr_combines_host_and_port() {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: String::new(),
            database_backend: DatabaseBackend::Postgres,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
            session_secret: String::new(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: None,
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
            superadmin_did: None,
            project_root: None,
            project: None,
            plugin_dir: std::path::PathBuf::from("./plugins"),
            auto_backfill: true,
            script_env_file: None,
        };
        assert_eq!(
            config.listen_addr(),
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    #[serial]
    fn load_from_a_missing_root_refuses() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let root = env::temp_dir().join(format!("hv-missing-{}", uuid::Uuid::new_v4()));
        let err = Config::load_from(Some(&root), ConfigMode::Serve).unwrap_err();
        assert!(
            matches!(err, project::ProjectConfigError::RootNotFound { .. }),
            "{err}"
        );
    }

    #[test]
    #[serial]
    fn load_from_a_root_without_the_file_has_no_project_config() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let dir = project::TempProject::new(None);
        let config = Config::load_from(Some(dir.path()), ConfigMode::Serve).unwrap();
        assert_eq!(config.project_root.as_deref(), Some(dir.path()));
        assert!(config.project.is_none());
    }

    #[test]
    #[serial]
    fn load_from_applies_the_file_and_keeps_it() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let dir = project::TempProject::new(Some(
            "[instance]\napp_name = \"Loaded\"\n[labels]\nsubscribe = [\"did:plc:lab1\"]\n",
        ));
        let config = Config::load_from(Some(dir.path()), ConfigMode::Serve).unwrap();
        assert_eq!(config.app_name.as_deref(), Some("Loaded"));
        assert!(config.declares_labeler("did:plc:lab1"));
        assert!(!config.declares_network_lexicon("app.bsky.actor.profile"));
    }

    #[test]
    #[serial]
    fn load_reads_the_root_from_happyview_project() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let dir = project::TempProject::new(Some("[instance]\napp_name = \"Via Env\"\n"));
        let _unset = EnvGuard("HAPPYVIEW_PROJECT");
        unsafe { env::set_var("HAPPYVIEW_PROJECT", dir.path()) };
        let config = Config::load().unwrap();
        assert_eq!(config.app_name.as_deref(), Some("Via Env"));
    }

    #[test]
    #[serial]
    fn load_without_a_project_is_from_env() {
        unsafe {
            clear_env();
            set_required_env();
            env::remove_var("HAPPYVIEW_PROJECT");
        }
        let _reset = vars::LayerReset;
        let config = Config::load().unwrap();
        assert!(config.project_root.is_none() && config.project.is_none());
        assert!(!config.declares_labeler("did:plc:anything"));
    }

    /// A refused load must not leave its layer installed: request-time
    /// readers would otherwise follow an edit the server rejected.
    #[test]
    #[serial]
    fn a_refused_load_leaves_the_previous_layer() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let _unset = EnvGuard("APP_NAME");
        let good = project::TempProject::new(Some("[instance]\napp_name = \"Before\"\n"));
        Config::load_from(Some(good.path()), ConfigMode::Serve).unwrap();
        assert_eq!(var("APP_NAME").as_deref(), Some("Before"));

        // Refused only once the layer is installed, since `var` is what reads it.
        let _superadmin = EnvGuard("SUPERADMIN_DID");
        unsafe { env::set_var("SUPERADMIN_DID", "alice.bsky.social") };
        let bad = project::TempProject::new(Some("[instance]\napp_name = \"After\"\n"));
        assert!(Config::load_from(Some(bad.path()), ConfigMode::Serve).is_err());
        assert_eq!(var("APP_NAME").as_deref(), Some("Before"));
    }

    #[test]
    #[serial]
    fn defaults_sit_beneath_the_file_and_the_environment() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let _unset = EnvGuard("APP_NAME");
        let dir = project::TempProject::new(Some("[instance]\napp_name = \"File\"\n"));
        let defaults = |_: &BTreeMap<String, String>| {
            vec![
                ("APP_NAME".to_string(), "Default".to_string()),
                ("BASE_PATH".to_string(), "/from-default".to_string()),
            ]
        };

        // A default is used when neither the environment nor the file sets it.
        let config = Config::load_from_with(Some(dir.path()), ConfigMode::Serve, defaults).unwrap();
        assert_eq!(config.base_path.as_deref(), Some("/from-default"));
        // The file beats a default.
        assert_eq!(config.app_name.as_deref(), Some("File"));
        // The environment beats both, and defaults never reach it.
        unsafe { env::set_var("APP_NAME", "Env") };
        let config = Config::load_from_with(Some(dir.path()), ConfigMode::Serve, defaults).unwrap();
        assert_eq!(config.app_name.as_deref(), Some("Env"));
        assert!(env::var("BASE_PATH").is_err());
    }

    #[test]
    #[serial]
    fn defaults_see_the_merged_view() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let dir = project::TempProject::new(Some("[instance]\napp_name = \"File\"\n"));
        let mut seen = None;
        Config::load_from_with(Some(dir.path()), ConfigMode::Serve, |view| {
            seen = Some((
                view.get("APP_NAME").cloned(),
                view.get("DATABASE_URL").cloned(),
            ));
            Vec::new()
        })
        .unwrap();
        let (app, db) = seen.unwrap();
        assert_eq!(app.as_deref(), Some("File"));
        assert!(db.is_some(), "the process environment is part of the view");
    }

    #[test]
    #[serial]
    fn from_env_reads_required_vars() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = Config::from_env();
        assert_eq!(config.database_url, "postgres://localhost/test");
        assert_eq!(config.public_url, "http://127.0.0.1:3000");
    }

    #[test]
    #[serial]
    fn from_env_applies_defaults() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = Config::from_env();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 3000);
        assert_eq!(
            config.jetstream_url,
            "wss://jetstream1.us-east.bsky.network"
        );
        assert_eq!(config.relay_url, "https://bsky.network");
        assert_eq!(config.plc_url, "https://plc.directory");
    }

    #[test]
    #[serial]
    fn from_env_reads_optional_overrides() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("HOST", "10.0.0.1");
            env::set_var("PORT", "9090");
            env::set_var("RELAY_URL", "https://relay.example.com");
            env::set_var("PLC_URL", "https://plc.example.com");
        }
        let config = Config::from_env();
        assert_eq!(config.host, "10.0.0.1");
        assert_eq!(config.port, 9090);
        assert_eq!(config.relay_url, "https://relay.example.com");
        assert_eq!(config.plc_url, "https://plc.example.com");
    }

    #[test]
    #[serial]
    #[should_panic(expected = "DATABASE_URL must be set")]
    fn from_env_panics_without_database_url() {
        unsafe {
            clear_env();
            env::set_var("PUBLIC_URL", "http://127.0.0.1:3000");
        }
        Config::from_env();
    }

    #[test]
    fn token_encryption_key_unset_or_empty_is_none() {
        assert!(parse_token_encryption_key(None).unwrap().is_none());
        assert!(parse_token_encryption_key(Some("")).unwrap().is_none());
    }

    #[test]
    fn token_encryption_key_valid_32_bytes_parses() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let parsed = parse_token_encryption_key(Some(&encoded)).unwrap().unwrap();
        assert_eq!(parsed, [7u8; 32]);
    }

    #[test]
    fn token_encryption_key_invalid_base64_is_err() {
        assert!(parse_token_encryption_key(Some("not valid base64 @@@")).is_err());
    }

    #[test]
    fn token_encryption_key_wrong_length_is_err() {
        use base64::Engine;
        // 16 bytes, not 32.
        let encoded = base64::engine::general_purpose::STANDARD.encode([7u8; 16]);
        let err = parse_token_encryption_key(Some(&encoded)).unwrap_err();
        assert!(err.contains("32 bytes"), "got: {err}");
    }

    #[test]
    fn validate_session_secret_accepts_strong_secret() {
        assert!(validate_session_secret("a-securely-generated-32plus-byte-secret!!").is_ok());
        // Exactly 32 bytes is accepted.
        assert!(validate_session_secret(&"x".repeat(32)).is_ok());
    }

    #[test]
    fn validate_session_secret_rejects_empty() {
        let err = validate_session_secret("").unwrap_err();
        assert!(err.contains("not set"), "got: {err}");
    }

    #[test]
    fn validate_session_secret_rejects_known_defaults() {
        // The code's historical sentinel is 34 bytes, so length alone would not
        // catch it — the explicit default list must.
        assert!(validate_session_secret("change-me-in-production-not-secure").is_err());
        assert!(validate_session_secret("change-me-in-production").is_err());
    }

    #[test]
    fn validate_session_secret_rejects_too_short() {
        let err = validate_session_secret(&"x".repeat(31)).unwrap_err();
        assert!(err.contains("at least 32 bytes"), "got: {err}");
    }

    #[test]
    #[serial]
    fn from_env_does_not_panic_without_session_secret() {
        unsafe {
            clear_env();
            set_required_env();
        }
        // Boot must succeed even with no SESSION_SECRET; the problem is surfaced
        // via config_errors() and disables cookie auth instead of aborting.
        let config = Config::from_env();
        assert!(!config.session_secret_secure());
        assert!(!config.config_errors().is_empty());
    }

    #[test]
    #[serial]
    fn from_env_with_strong_session_secret_is_secure() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var(
                "SESSION_SECRET",
                "a-securely-generated-32plus-byte-secret!!",
            );
        }
        let config = Config::from_env();
        assert!(config.session_secret_secure());
        assert!(config.config_errors().is_empty());
    }

    #[test]
    #[serial]
    #[should_panic(expected = "PUBLIC_URL must be set")]
    fn from_env_panics_without_public_url() {
        unsafe {
            clear_env();
            env::set_var("DATABASE_URL", "postgres://localhost/test");
        }
        Config::from_env();
    }

    #[test]
    #[serial]
    fn default_event_log_retention_days() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = Config::from_env();
        assert_eq!(config.event_log_retention_days, 30);
    }

    #[test]
    #[serial]
    fn custom_event_log_retention_days() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("EVENT_LOG_RETENTION_DAYS", "7");
        }
        let config = Config::from_env();
        assert_eq!(config.event_log_retention_days, 7);
    }

    #[test]
    #[serial]
    fn zero_event_log_retention_days_disables_cleanup() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("EVENT_LOG_RETENTION_DAYS", "0");
        }
        let config = Config::from_env();
        assert_eq!(config.event_log_retention_days, 0);
    }

    #[test]
    #[serial]
    fn database_backend_detected_from_url() {
        unsafe {
            clear_env();
            env::set_var("DATABASE_URL", "postgres://localhost/test");
            env::set_var("PUBLIC_URL", "http://127.0.0.1:3000");
        }
        let config = Config::from_env();
        assert_eq!(config.database_backend, DatabaseBackend::Postgres);
    }

    #[test]
    #[serial]
    fn database_backend_sqlite_detected_from_url() {
        unsafe {
            clear_env();
            env::set_var("DATABASE_URL", "sqlite://data/happyview.db?mode=rwc");
            env::set_var("PUBLIC_URL", "http://127.0.0.1:3000");
        }
        let config = Config::from_env();
        assert_eq!(config.database_backend, DatabaseBackend::Sqlite);
    }

    #[test]
    #[serial]
    fn database_backend_override_from_env() {
        unsafe {
            clear_env();
            env::set_var("DATABASE_URL", "postgres://localhost/test");
            env::set_var("DATABASE_BACKEND", "sqlite");
            env::set_var("PUBLIC_URL", "http://127.0.0.1:3000");
        }
        let config = Config::from_env();
        assert_eq!(config.database_backend, DatabaseBackend::Sqlite);
    }

    #[test]
    #[serial]
    fn from_env_base_path_none_by_default() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = Config::from_env();
        assert!(config.base_path.is_none());
    }

    #[test]
    #[serial]
    fn from_env_base_path_read_from_env() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("BASE_PATH", "/hv");
        }
        let config = Config::from_env();
        assert_eq!(config.base_path.as_deref(), Some("/hv"));
    }

    #[test]
    #[serial]
    fn from_env_base_path_strips_trailing_slash() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("BASE_PATH", "/hv/");
        }
        let config = Config::from_env();
        assert_eq!(config.base_path.as_deref(), Some("/hv"));
    }

    #[test]
    #[serial]
    fn from_env_base_path_empty_becomes_none() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("BASE_PATH", "");
        }
        let config = Config::from_env();
        assert!(config.base_path.is_none());
    }

    #[test]
    #[serial]
    fn from_env_base_path_slash_only_becomes_none() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("BASE_PATH", "/");
        }
        let config = Config::from_env();
        assert!(config.base_path.is_none());
    }

    #[test]
    #[serial]
    #[should_panic(expected = "BASE_PATH must start with '/'")]
    fn from_env_base_path_without_leading_slash_panics() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("BASE_PATH", "hv");
        }
        Config::from_env();
    }

    #[test]
    fn effective_public_url_without_base_path() {
        let config = Config {
            host: String::new(),
            port: 3000,
            database_url: String::new(),
            database_backend: DatabaseBackend::Postgres,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: "https://example.com".into(),
            user_agent: String::new(),
            session_secret: String::new(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: None,
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
            superadmin_did: None,
            project_root: None,
            project: None,
            plugin_dir: std::path::PathBuf::from("./plugins"),
            auto_backfill: true,
            script_env_file: None,
        };
        assert_eq!(config.effective_public_url(), "https://example.com");
    }

    #[test]
    fn effective_public_url_with_base_path() {
        let config = Config {
            host: String::new(),
            port: 3000,
            database_url: String::new(),
            database_backend: DatabaseBackend::Postgres,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: "https://example.com".into(),
            user_agent: String::new(),
            session_secret: String::new(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: None,
            base_path: Some("/hv".into()),
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
            superadmin_did: None,
            project_root: None,
            project: None,
            plugin_dir: std::path::PathBuf::from("./plugins"),
            auto_backfill: true,
            script_env_file: None,
        };
        assert_eq!(config.effective_public_url(), "https://example.com/hv");
    }

    #[test]
    fn effective_public_url_trims_trailing_slash() {
        let config = Config {
            host: String::new(),
            port: 3000,
            database_url: String::new(),
            database_backend: DatabaseBackend::Postgres,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: "https://example.com/".into(),
            user_agent: String::new(),
            session_secret: String::new(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: None,
            base_path: Some("/hv".into()),
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
            superadmin_did: None,
            project_root: None,
            project: None,
            plugin_dir: std::path::PathBuf::from("./plugins"),
            auto_backfill: true,
            script_env_file: None,
        };
        assert_eq!(config.effective_public_url(), "https://example.com/hv");
    }

    #[test]
    fn url_with_base_path_appends() {
        let config = Config {
            host: String::new(),
            port: 3000,
            database_url: String::new(),
            database_backend: DatabaseBackend::Postgres,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
            session_secret: String::new(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: None,
            base_path: Some("/hv".into()),
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
            superadmin_did: None,
            project_root: None,
            project: None,
            plugin_dir: std::path::PathBuf::from("./plugins"),
            auto_backfill: true,
            script_env_file: None,
        };
        assert_eq!(
            config.url_with_base_path("https://otherdomain.com"),
            "https://otherdomain.com/hv"
        );
    }

    #[test]
    fn user_agent_defaults_to_name_version_and_public_url() {
        assert_eq!(
            build_user_agent(None, "https://hv.example.com"),
            format!(
                "HappyView/{} (+https://hv.example.com)",
                crate::version::version()
            )
        );
    }

    #[test]
    fn user_agent_omits_url_when_public_url_is_empty() {
        assert_eq!(build_user_agent(None, ""), crate::version::user_agent());
    }

    #[test]
    fn user_agent_env_override_wins() {
        assert_eq!(
            build_user_agent(
                Some("Custom/1.0 (+mailto:a@b.c)".into()),
                "https://hv.example.com"
            ),
            "Custom/1.0 (+mailto:a@b.c)"
        );
    }

    #[test]
    fn url_with_base_path_without_base_path() {
        let config = Config {
            host: String::new(),
            port: 3000,
            database_url: String::new(),
            database_backend: DatabaseBackend::Postgres,
            sqlite_journal_size_limit: crate::db::DEFAULT_JOURNAL_SIZE_LIMIT,
            public_url: String::new(),
            user_agent: String::new(),
            session_secret: String::new(),
            jetstream_url: String::new(),
            relay_url: String::new(),
            plc_url: String::new(),
            static_dir: None,
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
            telemetry_collector_url: String::new(),
            plugin_cache_dir: None,
            platform_api_key_hash: None,
            job_worker_concurrency: 1,
            superadmin_did: None,
            project_root: None,
            project: None,
            plugin_dir: std::path::PathBuf::from("./plugins"),
            auto_backfill: true,
            script_env_file: None,
        };
        assert_eq!(
            config.url_with_base_path("https://otherdomain.com"),
            "https://otherdomain.com"
        );
    }

    #[test]
    fn platform_hash_unset_or_empty_is_none() {
        assert_eq!(parse_platform_api_key_hash(None), Ok(None));
        assert_eq!(parse_platform_api_key_hash(Some("")), Ok(None));
        assert_eq!(parse_platform_api_key_hash(Some("   ")), Ok(None));
    }

    #[test]
    fn platform_hash_is_normalised_to_lowercase_and_trimmed() {
        let upper = format!("  {}  ", "AB".repeat(32));
        assert_eq!(
            parse_platform_api_key_hash(Some(&upper)),
            Ok(Some("ab".repeat(32)))
        );
    }

    #[test]
    fn platform_hash_rejects_wrong_length_and_non_hex() {
        assert!(parse_platform_api_key_hash(Some(&"a".repeat(63))).is_err());
        assert!(parse_platform_api_key_hash(Some(&"a".repeat(65))).is_err());
        assert!(parse_platform_api_key_hash(Some(&"g".repeat(64))).is_err());
    }

    #[test]
    fn job_worker_concurrency_defaults_and_clamps() {
        assert_eq!(parse_job_worker_concurrency(None), 1);
        assert_eq!(parse_job_worker_concurrency(Some("")), 1);
        assert_eq!(parse_job_worker_concurrency(Some("garbage")), 1);
        assert_eq!(parse_job_worker_concurrency(Some("0")), 1);
        assert_eq!(parse_job_worker_concurrency(Some("-3")), 1);
        assert_eq!(parse_job_worker_concurrency(Some("4")), 4);
        assert_eq!(parse_job_worker_concurrency(Some(" 8 ")), 8);
        assert_eq!(parse_job_worker_concurrency(Some("1000")), 32);
    }

    #[test]
    #[serial]
    fn from_env_reads_platform_and_worker_settings() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("PLATFORM_API_KEY_HASH", "cd".repeat(32));
            env::set_var("JOB_WORKER_CONCURRENCY", "3");
        }
        let config = Config::from_env();
        assert_eq!(config.platform_api_key_hash, Some("cd".repeat(32)));
        assert_eq!(config.job_worker_concurrency, 3);
    }

    #[test]
    #[serial]
    fn from_env_platform_and_worker_defaults() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = Config::from_env();
        assert_eq!(config.platform_api_key_hash, None);
        assert_eq!(config.job_worker_concurrency, 1);
    }

    /// `Config::from_env` with `toml` installed as the project layer, then
    /// the layer cleared again.
    fn from_project(toml: &str, mode: project::ConfigMode) -> Config {
        let _reset = vars::LayerReset;
        let parsed = project::parse_project_config(toml, Path::new(project::PROJECT_FILE)).unwrap();
        vars::set_project_layer(parsed.env_layer(mode));
        Config::from_env()
    }

    const APP_NAME_TOML: &str = "[instance]\napp_name = \"From The File\"\n";
    const SUPERADMIN_TOML: &str = "[instance]\nsuperadmin = \"did:plc:fromthefile\"\n";
    const PORT_TOML: &str = "[dev]\nport = 4100\n";

    #[test]
    #[serial]
    fn app_name_comes_from_the_file_when_the_env_is_unset() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = from_project(APP_NAME_TOML, project::ConfigMode::Serve);
        assert_eq!(config.app_name.as_deref(), Some("From The File"));
    }

    #[test]
    #[serial]
    fn app_name_env_wins_over_the_file() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("APP_NAME", "From Env");
        }
        let config = from_project(APP_NAME_TOML, project::ConfigMode::Serve);
        assert_eq!(config.app_name.as_deref(), Some("From Env"));
    }

    #[test]
    #[serial]
    fn an_empty_app_name_env_still_wins_over_the_file() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("APP_NAME", "");
        }
        let config = from_project(APP_NAME_TOML, project::ConfigMode::Serve);
        assert_eq!(config.app_name.as_deref(), Some(""));
    }

    #[test]
    #[serial]
    fn superadmin_comes_from_the_file_when_the_env_is_unset() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let config = from_project(SUPERADMIN_TOML, project::ConfigMode::Serve);
        assert_eq!(
            config.superadmin_did.as_deref(),
            Some("did:plc:fromthefile")
        );
    }

    #[test]
    #[serial]
    fn superadmin_env_wins_over_the_file() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("SUPERADMIN_DID", "did:plc:fromenv");
        }
        let config = from_project(SUPERADMIN_TOML, project::ConfigMode::Serve);
        assert_eq!(config.superadmin_did.as_deref(), Some("did:plc:fromenv"));
    }

    #[test]
    #[serial]
    fn an_invalid_superadmin_env_refuses_boot() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("SUPERADMIN_DID", "alice.bsky.social");
        }
        let _reset = vars::LayerReset;
        let err = Config::load_from(None, ConfigMode::Serve).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("SUPERADMIN_DID"), "{message}");
        assert!(message.contains("alice.bsky.social"), "{message}");
    }

    #[test]
    #[serial]
    fn an_invalid_superadmin_in_the_file_refuses_boot() {
        unsafe {
            clear_env();
            set_required_env();
        }
        let _reset = vars::LayerReset;
        let root = env::temp_dir().join(format!("hv-sa-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(project::PROJECT_FILE),
            "[instance]\nsuperadmin = \"not-a-did\"\n",
        )
        .unwrap();
        let result = Config::load_from(Some(&root), ConfigMode::Serve);
        std::fs::remove_dir_all(&root).ok();
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn a_blank_superadmin_env_is_unset() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("SUPERADMIN_DID", "  ");
        }
        let _reset = vars::LayerReset;
        let config = Config::load_from(None, ConfigMode::Serve).unwrap();
        assert_eq!(config.superadmin_did, None);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_environment_entries_are_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x66, 0xff, 0x6f]);
        let kept: Vec<_> = utf8_env(
            vec![
                ("GOOD".into(), "yes".into()),
                (bad.clone(), "v".into()),
                ("BADVAL".into(), bad),
            ]
            .into_iter(),
        )
        .collect();
        assert_eq!(kept, vec![("GOOD".to_string(), "yes".to_string())]);
    }

    #[test]
    #[serial]
    fn port_comes_from_the_dev_section_only_in_dev() {
        unsafe {
            clear_env();
            set_required_env();
        }
        assert_eq!(from_project(PORT_TOML, project::ConfigMode::Dev).port, 4100);
        assert_eq!(
            from_project(PORT_TOML, project::ConfigMode::Serve).port,
            3000
        );
    }

    #[test]
    #[serial]
    fn port_env_wins_over_the_dev_section() {
        unsafe {
            clear_env();
            set_required_env();
            env::set_var("PORT", "5000");
        }
        assert_eq!(from_project(PORT_TOML, project::ConfigMode::Dev).port, 5000);
    }

    #[test]
    fn superadmin_did_parsing() {
        assert_eq!(parse_superadmin_did(None), Ok(None));
        assert_eq!(parse_superadmin_did(Some("  ")), Ok(None));
        assert_eq!(
            parse_superadmin_did(Some(" did:plc:abc ")),
            Ok(Some("did:plc:abc".to_string()))
        );
        assert!(parse_superadmin_did(Some("alice.bsky.social")).is_err());
    }
}
