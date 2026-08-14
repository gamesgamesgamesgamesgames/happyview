use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Disabled,
    Open,
    Allowlist,
    Blocklist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    pub nsids: Vec<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            mode: ProxyMode::Open,
            nsids: vec![],
        }
    }
}

impl ProxyConfig {
    pub fn allows(&self, nsid: &str) -> bool {
        match self.mode {
            ProxyMode::Disabled => false,
            ProxyMode::Open => true,
            ProxyMode::Allowlist => self.nsids.iter().any(|pattern| nsid_matches(pattern, nsid)),
            ProxyMode::Blocklist => !self.nsids.iter().any(|pattern| nsid_matches(pattern, nsid)),
        }
    }
}

fn nsid_matches(pattern: &str, nsid: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        nsid.starts_with(prefix)
            && nsid.len() > prefix.len()
            && nsid.as_bytes()[prefix.len()] == b'.'
    } else {
        pattern == nsid
    }
}

/// Re-exported so `admin::proxy_config` keeps a stable import path. The rules
/// live in `happyview-nsid` alongside every other NSID rule.
pub fn validate_nsid_pattern(pattern: &str) -> Result<(), String> {
    happyview_nsid::validate_nsid_pattern(pattern).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_open_with_empty_nsids() {
        let config = ProxyConfig::default();
        assert_eq!(config.mode, ProxyMode::Open);
        assert!(config.nsids.is_empty());
    }

    #[test]
    fn disabled_blocks_everything() {
        let config = ProxyConfig {
            mode: ProxyMode::Disabled,
            nsids: vec![],
        };
        assert!(!config.allows("com.example.feed.getHot"));
        assert!(!config.allows("anything.at.all"));
    }

    #[test]
    fn open_allows_everything() {
        let config = ProxyConfig {
            mode: ProxyMode::Open,
            nsids: vec![],
        };
        assert!(config.allows("com.example.feed.getHot"));
        assert!(config.allows("anything.at.all"));
    }

    #[test]
    fn allowlist_exact_match() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.feed.getHot".into()],
        };
        assert!(config.allows("com.example.feed.getHot"));
        assert!(!config.allows("com.example.feed.getCold"));
    }

    #[test]
    fn allowlist_wildcard() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.*".into()],
        };
        assert!(config.allows("com.example.feed.getHot"));
        assert!(config.allows("com.example.anything"));
        assert!(!config.allows("com.other.feed.getHot"));
    }

    #[test]
    fn blocklist_exact_match() {
        let config = ProxyConfig {
            mode: ProxyMode::Blocklist,
            nsids: vec!["com.example.feed.getHot".into()],
        };
        assert!(!config.allows("com.example.feed.getHot"));
        assert!(config.allows("com.example.feed.getCold"));
    }

    #[test]
    fn blocklist_wildcard() {
        let config = ProxyConfig {
            mode: ProxyMode::Blocklist,
            nsids: vec!["com.example.*".into()],
        };
        assert!(!config.allows("com.example.feed.getHot"));
        assert!(config.allows("com.other.feed.getHot"));
    }

    #[test]
    fn wildcard_does_not_match_prefix_without_dot() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.*".into()],
        };
        assert!(
            !config.allows("com.example"),
            "bare prefix should not match wildcard"
        );
    }

    #[test]
    fn validate_rejects_invalid_characters() {
        assert!(validate_nsid_pattern("com.ex@mple.foo").is_err());
        assert!(validate_nsid_pattern("com.ex mple.foo").is_err());
        assert!(validate_nsid_pattern("com.ex_mple.foo").is_err());
    }

    #[test]
    fn validate_valid_nsids() {
        assert!(validate_nsid_pattern("com.example.feed.getHot").is_ok());
        assert!(validate_nsid_pattern("com.example.*").is_ok());
        assert!(validate_nsid_pattern("games.gamesgamesgamesgames.*").is_ok());
        assert!(validate_nsid_pattern("a.b.c").is_ok());
    }

    #[test]
    fn validate_invalid_nsids() {
        assert!(validate_nsid_pattern("").is_err());
        assert!(validate_nsid_pattern("*").is_err());
        assert!(validate_nsid_pattern("com").is_err());
        assert!(validate_nsid_pattern("com.example.*.foo").is_err());
        assert!(validate_nsid_pattern("com..example").is_err());
        assert!(validate_nsid_pattern(".com.example").is_err());
        assert!(validate_nsid_pattern("com.example.").is_err());
    }

    #[test]
    fn validate_digit_leading_authority_pattern() {
        assert!(validate_nsid_pattern("pics.2bit.*").is_ok());
        assert!(validate_nsid_pattern("pics.2bit.feed.getPhotos").is_ok());
    }

    #[test]
    fn roundtrip_json() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.*".into()],
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mode, ProxyMode::Allowlist);
        assert_eq!(parsed.nsids, vec!["com.example.*"]);
    }
}
