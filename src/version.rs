//! The single source of truth for the running HappyView version.
pub fn version() -> &'static str {
    match option_env!("HAPPYVIEW_VERSION") {
        Some(v) if !v.trim().is_empty() => v.trim().trim_start_matches('v'),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// The `User-Agent` HappyView sends on outbound requests.
pub fn user_agent() -> String {
    format!("HappyView/{}", version())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_never_empty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn version_has_no_leading_v() {
        assert!(!version().starts_with('v'), "got {}", version());
    }

    #[test]
    fn user_agent_carries_the_resolved_version() {
        assert_eq!(user_agent(), format!("HappyView/{}", version()));
    }

    #[test]
    fn stamped_builds_do_not_report_the_package_version() {
        if let Some(stamped) = option_env!("HAPPYVIEW_VERSION")
            && !stamped.trim().is_empty()
        {
            assert_eq!(version(), stamped.trim().trim_start_matches('v'));
        }
    }
}
