//! Where this instance's limits come from (see `happyview_provider::limits`
//! for the contract).
//!
//! Two ways to set them, never both:
//!
//! - `HAPPYVIEW_LIMITS_FILE`: a JSON file in the snapshot's shape, with the
//!   host's wording. It is re-read when it changes, so a host can raise or
//!   lower a limit without restarting the instance. A file that stops
//!   parsing keeps the last good limits, and says so in the log.
//! - `HAPPYVIEW_LIMIT_*` variables, one per limit, read once at boot. No
//!   wording: refusals say the generic sentence.
//!
//! With neither, everything is unlimited, which is every self-hosted
//! instance unless its operator says otherwise.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use happyview_provider::{Cap, FixedLimits, Gate, Limit, Limits, LimitsProvider, Wording};

/// The variable naming the limits file.
pub const FILE_VAR: &str = "HAPPYVIEW_LIMITS_FILE";

/// How often the file is looked at for a change.
pub const RELOAD_EVERY: Duration = Duration::from_secs(2);

/// The variable that sets `limit` when there is no file.
pub fn var_name(limit: Limit) -> &'static str {
    match limit {
        Limit::Seats => "HAPPYVIEW_LIMIT_SEATS",
        Limit::ApiClients => "HAPPYVIEW_LIMIT_API_CLIENTS",
        Limit::AnonymousXrpc => "HAPPYVIEW_LIMIT_ANONYMOUS_XRPC",
        Limit::LogRetentionDays => "HAPPYVIEW_LIMIT_LOG_RETENTION_DAYS",
    }
}

/// What boot reads the limits from: a provider, and the file to watch when
/// there is one.
pub struct Loaded {
    pub provider: Arc<dyn LimitsProvider>,
    pub file: Option<Arc<FileLimits>>,
}

/// The limits from `config::vars::var`. An error names what is wrong; boot
/// stops on it, because dropping a limit it can't read would make the
/// instance unlimited, the opposite of what was meant.
pub fn load() -> Result<Loaded, String> {
    load_from(happyview_provider::vars::var)
}

pub fn load_from(get: impl Fn(&str) -> Option<String>) -> Result<Loaded, String> {
    let set = |name: &str| get(name).filter(|v| !v.trim().is_empty());
    let vars: Vec<&str> = Limit::ALL
        .iter()
        .map(|l| var_name(*l))
        .filter(|name| set(name).is_some())
        .collect();
    match set(FILE_VAR) {
        Some(path) => {
            if !vars.is_empty() {
                return Err(format!(
                    "set limits in {FILE_VAR} or in the HAPPYVIEW_LIMIT_* variables, not both (also set: {})",
                    vars.join(", ")
                ));
            }
            let file = Arc::new(FileLimits::open(PathBuf::from(path.trim()))?);
            Ok(Loaded {
                provider: file.clone(),
                file: Some(file),
            })
        }
        None => Ok(Loaded {
            provider: Arc::new(FixedLimits::new(from_vars(&get)?)),
            file: None,
        }),
    }
}

/// The limits the `HAPPYVIEW_LIMIT_*` variables set. Unset or blank is
/// unlimited.
fn from_vars(get: &impl Fn(&str) -> Option<String>) -> Result<Limits, String> {
    let raw = |limit: Limit| {
        get(var_name(limit))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let number = |limit: Limit| -> Result<Option<Cap>, String> {
        raw(limit)
            .map(|v| {
                v.parse::<u32>()
                    .map(|max| Cap {
                        max,
                        wording: Wording::default(),
                    })
                    .map_err(|_| format!("{} must be a whole number, got '{v}'", var_name(limit)))
            })
            .transpose()
    };
    let anonymous = raw(Limit::AnonymousXrpc)
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(format!(
                "{} must be true or false, got '{v}'",
                var_name(Limit::AnonymousXrpc)
            )),
        })
        .transpose()?
        .map(|allowed| Gate {
            allowed,
            wording: Wording::default(),
        });
    Ok(Limits {
        seats: number(Limit::Seats)?,
        api_clients: number(Limit::ApiClients)?,
        anonymous_xrpc: anonymous,
        log_retention_days: number(Limit::LogRetentionDays)?,
    })
}

/// Parse a limits file's text.
fn parse(path: &Path, text: &str) -> Result<Limits, String> {
    serde_json::from_str(text)
        .map_err(|e| format!("the limits file {} is not valid: {e}", path.display()))
}

/// Read and parse a limits file.
pub fn read_file(path: &Path) -> Result<Limits, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read the limits file {}: {e}", path.display()))?;
    parse(path, &text)
}

/// What happened when the file was looked at.
#[derive(Debug, PartialEq, Eq)]
pub enum Reload {
    Unchanged,
    Applied,
    /// The file changed but can't be used; the last good limits stay.
    Kept(String),
}

/// Limits from a file the host writes, kept in memory and replaced when the
/// file changes. A check reads the copy in memory; only the watcher touches
/// the file.
pub struct FileLimits {
    path: PathBuf,
    current: RwLock<Arc<Limits>>,
    /// The text last read, `None` while the file can't be read. Compared
    /// whole rather than by modification time, which can stay the same
    /// across a quick rewrite of a file this small.
    seen: RwLock<Option<String>>,
}

impl FileLimits {
    /// Read the file once. Boot needs it to be there and valid: a host that
    /// names a file means the instance to be limited by it.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read the limits file {}: {e}", path.display()))?;
        let limits = parse(&path, &text)?;
        Ok(FileLimits {
            path,
            current: RwLock::new(Arc::new(limits)),
            seen: RwLock::new(Some(text)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Look at the file, and take its limits if it changed and parses.
    pub fn reload(&self) -> Reload {
        let now = std::fs::read_to_string(&self.path);
        let mut seen = self.seen.write().unwrap_or_else(|e| e.into_inner());
        let text = match now {
            Ok(text) => text,
            Err(e) => {
                // Said once, when it goes, rather than every tick.
                return if seen.take().is_some() {
                    Reload::Kept(format!(
                        "the limits file {} can't be read ({e}); keeping the last limits",
                        self.path.display()
                    ))
                } else {
                    Reload::Unchanged
                };
            }
        };
        if seen.as_deref() == Some(text.as_str()) {
            return Reload::Unchanged;
        }
        let parsed = parse(&self.path, &text);
        *seen = Some(text);
        match parsed {
            Ok(limits) => {
                *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(limits);
                Reload::Applied
            }
            Err(e) => Reload::Kept(format!("{e}; keeping the last limits")),
        }
    }

    /// Re-read the file every [`RELOAD_EVERY`] for the life of the process.
    pub async fn watch(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RELOAD_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match self.reload() {
                Reload::Unchanged => {}
                Reload::Applied => tracing::info!(file = %self.path.display(), "limits reloaded"),
                Reload::Kept(why) => tracing::warn!("{why}"),
            }
        }
    }
}

impl LimitsProvider for FileLimits {
    fn limits(&self) -> Arc<Limits> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Admin users, the super user included: what the `seats` limit counts.
pub async fn seats_in_use(
    db: &sqlx::AnyPool,
    _backend: crate::db::DatabaseBackend,
) -> Result<u32, crate::error::AppError> {
    let (n,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_users")
        .fetch_one(db)
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("user count failed: {e}")))?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The API clients the `api_clients` limit counts: active ones the
/// instance's admins registered. A client an app registers under its own
/// (a child, with a parent) isn't one of them, nor is hpy, built into every
/// instance.
pub async fn api_clients_in_use(
    db: &sqlx::AnyPool,
    backend: crate::db::DatabaseBackend,
) -> Result<u32, crate::error::AppError> {
    let (n,): (i64,) = crate::db::query_as(&crate::db::adapt_sql(
        "SELECT COUNT(*) FROM happyview_api_clients WHERE is_active = 1 AND parent_client_id IS NULL AND id <> ?",
        backend,
    ))
    .bind(crate::auth::cli_client::CLIENT_ID)
    .fetch_one(db)
    .await
    .map_err(|e| crate::error::AppError::Internal(format!("API client count failed: {e}")))?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn limits_of(pairs: &[(&str, &str)]) -> Result<Limits, String> {
        load_from(lookup(pairs)).map(|l| (*l.provider.limits()).clone())
    }

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn nothing_set_is_unlimited() {
        assert_eq!(limits_of(&[]), Ok(Limits::default()));
        assert_eq!(
            limits_of(&[("HAPPYVIEW_LIMIT_SEATS", "  ")]),
            Ok(Limits::default())
        );
    }

    #[test]
    fn each_variable_sets_its_limit() {
        let l = limits_of(&[
            ("HAPPYVIEW_LIMIT_SEATS", "5"),
            ("HAPPYVIEW_LIMIT_API_CLIENTS", " 3 "),
            ("HAPPYVIEW_LIMIT_ANONYMOUS_XRPC", "False"),
            ("HAPPYVIEW_LIMIT_LOG_RETENTION_DAYS", "7"),
        ])
        .unwrap();
        assert_eq!(l.seats.unwrap().max, 5);
        assert_eq!(l.api_clients.unwrap().max, 3);
        assert!(!l.anonymous_xrpc.unwrap().allowed);
        assert_eq!(l.log_retention_days.unwrap().max, 7);
    }

    #[test]
    fn a_value_it_cant_read_is_refused_by_name() {
        for bad in ["three", "-1", "2.5"] {
            let err = limits_of(&[("HAPPYVIEW_LIMIT_API_CLIENTS", bad)]).unwrap_err();
            assert!(err.contains("HAPPYVIEW_LIMIT_API_CLIENTS"), "{err}");
            assert!(err.contains(bad), "{err}");
        }
        let err = limits_of(&[("HAPPYVIEW_LIMIT_ANONYMOUS_XRPC", "no")]).unwrap_err();
        assert!(err.contains("true or false"), "{err}");
    }

    #[test]
    fn a_file_and_variables_together_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.json");
        write(&path, "{}");
        let err = load_from(lookup(&[
            (FILE_VAR, path.to_str().unwrap()),
            ("HAPPYVIEW_LIMIT_SEATS", "5"),
        ]))
        .err()
        .unwrap();
        assert!(
            err.contains("not both") && err.contains("HAPPYVIEW_LIMIT_SEATS"),
            "{err}"
        );
    }

    #[test]
    fn a_named_file_must_be_there_and_valid_at_boot() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let err = load_from(lookup(&[(FILE_VAR, missing.to_str().unwrap())]))
            .err()
            .unwrap();
        assert!(err.contains("could not read"), "{err}");

        let bad = dir.path().join("bad.json");
        write(&bad, r#"{"seat": {"max": 1}}"#);
        let err = load_from(lookup(&[(FILE_VAR, bad.to_str().unwrap())]))
            .err()
            .unwrap();
        assert!(err.contains("not valid"), "{err}");
    }

    #[test]
    fn the_file_carries_the_hosts_wording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.json");
        write(
            &path,
            r#"{"seats": {"max": 1, "message": "Your plan has 1 seat.", "actionUrl": "https://h.example/plan", "actionLabel": "Add a seat"}}"#,
        );
        let loaded = load_from(lookup(&[(FILE_VAR, path.to_str().unwrap())])).unwrap();
        let refused = loaded.provider.limits().check(Limit::Seats, 1).unwrap_err();
        assert_eq!(refused.message, "Your plan has 1 seat.");
        assert_eq!(refused.action_label.as_deref(), Some("Add a seat"));
        assert!(loaded.file.is_some());
    }

    #[test]
    fn a_changed_file_is_taken_and_a_broken_one_keeps_the_last_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limits.json");
        write(&path, r#"{"seats": {"max": 1}}"#);
        let file = FileLimits::open(path.clone()).unwrap();
        assert_eq!(file.reload(), Reload::Unchanged);

        write(&path, r#"{"seats": {"max": 10}}"#);
        assert_eq!(file.reload(), Reload::Applied);
        assert_eq!(file.limits().seats.as_ref().unwrap().max, 10);

        write(&path, r#"{"seats": {"max": "lots"}}"#);
        assert!(matches!(file.reload(), Reload::Kept(_)));
        assert_eq!(file.limits().seats.as_ref().unwrap().max, 10);

        std::fs::remove_file(&path).unwrap();
        assert!(matches!(file.reload(), Reload::Kept(_)));
        assert_eq!(
            file.reload(),
            Reload::Unchanged,
            "said once, not every tick"
        );
        assert_eq!(file.limits().seats.as_ref().unwrap().max, 10);

        write(&path, "{}");
        assert_eq!(file.reload(), Reload::Applied);
        assert_eq!(*file.limits(), Limits::default());

        // A rewrite of the same length, as fast as a host can make one.
        write(&path, r#"{"seats": {"max": 1}}"#);
        assert_eq!(file.reload(), Reload::Applied);
        write(&path, r#"{"seats": {"max": 9}}"#);
        assert_eq!(file.reload(), Reload::Applied);
        assert_eq!(file.limits().seats.as_ref().unwrap().max, 9);
    }
}
