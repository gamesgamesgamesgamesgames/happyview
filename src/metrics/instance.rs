//! Which process wrote a stored metrics row.
//!
//! Several instances can share one database, and each writes its own minute
//! rows, folds its own minutes and reads its own history back at boot. The
//! id is the process's, never the database's: `telemetry.instance_id` and
//! everything in `instance_settings` are shared by every instance on the
//! database, so keying rows on one of them would put two writers on the same
//! rows again.

use std::sync::OnceLock;

/// The variable that names this instance.
pub const ENV: &str = "HAPPYVIEW_INSTANCE_ID";

/// The longest id kept, in characters.
pub const MAX_LEN: usize = 128;

/// Trimmed and capped at [`MAX_LEN`] characters; `None` when nothing is left.
pub fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let capped: String = trimmed.chars().take(MAX_LEN).collect();
    // Cutting can leave whitespace at the new end.
    Some(capped.trim_end().to_string())
}

/// The id from what the process was given: the configured one, then the
/// hostname, then a random one, which a restart does not get back.
pub fn resolve(configured: Option<&str>, hostname: Option<&str>) -> String {
    configured
        .and_then(normalize)
        .or_else(|| hostname.and_then(normalize))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// This process's id, resolved once, so a random one stays the same for the
/// life of the process.
pub fn this_process() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let configured = crate::config::vars::var(ENV);
        resolve(configured.as_deref(), hostname().as_deref())
    })
}

/// `HOSTNAME` when the shell exported it, else the system's hostname.
fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(system_hostname)
}

#[cfg(unix)]
fn system_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes, and gethostname
    // writes at most that many.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok()
}

#[cfg(not(unix))]
fn system_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_id_wins() {
        assert_eq!(resolve(Some("  replica-a "), Some("host")), "replica-a");
    }

    #[test]
    fn a_blank_configured_id_falls_to_the_hostname() {
        assert_eq!(resolve(Some("   "), Some("host-1")), "host-1");
        assert_eq!(resolve(None, Some(" host-1\n")), "host-1");
    }

    #[test]
    fn with_neither_the_id_is_random() {
        let a = resolve(None, None);
        let b = resolve(Some(""), Some(" "));
        assert!(uuid::Uuid::parse_str(&a).is_ok(), "{a}");
        assert!(uuid::Uuid::parse_str(&b).is_ok(), "{b}");
        assert_ne!(a, b);
    }

    #[test]
    fn an_id_is_capped() {
        let long = "é".repeat(MAX_LEN + 10);
        let id = resolve(Some(&long), None);
        assert_eq!(id.chars().count(), MAX_LEN);
        let spaced = format!("{} tail", "a".repeat(MAX_LEN - 1));
        assert_eq!(normalize(&spaced).unwrap(), "a".repeat(MAX_LEN - 1));
    }

    #[test]
    fn this_process_is_stable() {
        assert_eq!(this_process(), this_process());
        assert!(!this_process().is_empty());
    }
}
