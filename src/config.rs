use std::env;
use std::net::SocketAddr;

use crate::db::DatabaseBackend;

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
fn validate_session_secret(secret: &str) -> Result<(), String> {
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
    let version = env!("CARGO_PKG_VERSION");
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
fn parse_token_encryption_key(raw: Option<&str>) -> Result<Option<[u8; 32]>, String> {
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
    pub relay_url: String,
    pub plc_url: String,
    pub static_dir: String,
    pub base_path: Option<String>,
    pub event_log_retention_days: u32,
    pub app_name: Option<String>,
    pub logo_uri: Option<String>,
    pub tos_uri: Option<String>,
    pub policy_uri: Option<String>,
    pub token_encryption_key: Option<[u8; 32]>,
    pub default_rate_limit_capacity: u32,
    pub default_rate_limit_refill_rate: f64,
}

impl Config {
    pub fn from_env() -> Self {
        let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let database_backend = env::var("DATABASE_BACKEND")
            .ok()
            .and_then(|s| DatabaseBackend::from_str(&s))
            .unwrap_or_else(|| DatabaseBackend::from_url(&database_url));

        let public_url = env::var("PUBLIC_URL").expect("PUBLIC_URL must be set");

        Self {
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000),
            database_url,
            database_backend,
            sqlite_journal_size_limit: crate::db::journal_size_limit_bytes(),
            user_agent: build_user_agent(env::var("HAPPYVIEW_USER_AGENT").ok(), &public_url),
            public_url,
            // Not required and never defaulted to a placeholder: an unset,
            // insecure, or too-short value is surfaced via `config_errors()` and
            // disables cookie auth rather than aborting boot. See C3.
            session_secret: env::var("SESSION_SECRET").unwrap_or_default(),
            jetstream_url: env::var("JETSTREAM_URL")
                .unwrap_or_else(|_| "wss://jetstream1.us-east.bsky.network".into()),
            relay_url: env::var("RELAY_URL").unwrap_or_else(|_| "https://bsky.network".into()),
            plc_url: env::var("PLC_URL").unwrap_or_else(|_| "https://plc.directory".into()),
            static_dir: env::var("STATIC_DIR").unwrap_or_else(|_| "./web/out".into()),
            base_path: env::var("BASE_PATH").ok().and_then(|s| {
                let s = s.trim_end_matches('/').to_string();
                if s.is_empty() {
                    None
                } else if !s.starts_with('/') {
                    panic!("BASE_PATH must start with '/' (got: {s})");
                } else {
                    Some(s)
                }
            }),
            event_log_retention_days: std::env::var("EVENT_LOG_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            app_name: env::var("APP_NAME").ok(),
            logo_uri: env::var("LOGO_URI").ok(),
            tos_uri: env::var("TOS_URI").ok(),
            policy_uri: env::var("POLICY_URI").ok(),
            token_encryption_key: match parse_token_encryption_key(
                env::var("TOKEN_ENCRYPTION_KEY").ok().as_deref(),
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
            default_rate_limit_capacity: env::var("DEFAULT_RATE_LIMIT_CAPACITY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
            default_rate_limit_refill_rate: env::var("DEFAULT_RATE_LIMIT_REFILL_RATE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2.0),
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
        ] {
            unsafe {
                env::remove_var(key);
            }
        }
    }

    unsafe fn set_required_env() {
        unsafe {
            env::set_var("DATABASE_URL", "postgres://localhost/test");
            env::set_var("PUBLIC_URL", "http://127.0.0.1:3000");
        }
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
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        assert_eq!(
            config.listen_addr(),
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
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
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
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
            static_dir: String::new(),
            base_path: Some("/hv".into()),
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
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
            static_dir: String::new(),
            base_path: Some("/hv".into()),
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
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
            static_dir: String::new(),
            base_path: Some("/hv".into()),
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
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
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn user_agent_omits_url_when_public_url_is_empty() {
        assert_eq!(
            build_user_agent(None, ""),
            format!("HappyView/{}", env!("CARGO_PKG_VERSION"))
        );
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
            static_dir: String::new(),
            base_path: None,
            event_log_retention_days: 30,
            app_name: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            token_encryption_key: None,
            default_rate_limit_capacity: 100,
            default_rate_limit_refill_rate: 2.0,
        };
        assert_eq!(
            config.url_with_base_path("https://otherdomain.com"),
            "https://otherdomain.com"
        );
    }
}
