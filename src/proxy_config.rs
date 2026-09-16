use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Disabled,
    Open,
    Allowlist,
    Blocklist,
}

/// Where an unrecognized XRPC method is sent.
///
/// This is a separate axis from [`ProxyMode`], which only decides *whether* a
/// method may be proxied at all. Conflating them was the shape of the original
/// bug: the proxy had exactly one routing rule and no way to express another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyRouting {
    /// Resolve the NSID's lexicon-publishing authority via `_lexicon` DNS and
    /// forward there, unauthenticated.
    ///
    /// This answers "who defines this schema", not "who serves this request",
    /// and the two have different answers. For `com.atproto.repo.*` it resolves
    /// to the account that publishes the `com.atproto` lexicons, which has no
    /// relationship to the caller's repo — the request arrives with no
    /// credentials and is refused. Retained as the default so existing
    /// instances are not changed underneath their clients.
    Authority,
    /// Forward to the caller's own PDS, authenticated as the caller, per the
    /// AT Protocol service proxying model.
    ///
    /// With no `atproto-proxy` header the PDS handles the request itself, which
    /// is what makes `com.atproto.repo.createRecord` work. With one, the header
    /// is passed through and the PDS resolves the destination and mints the
    /// inter-service token — HappyView cannot, since that token is signed by
    /// the user's own identity key.
    ServiceProxy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    pub nsids: Vec<String>,
    /// Defaulted so configurations stored before this field existed keep the
    /// behaviour they were saved with.
    #[serde(default = "default_routing")]
    pub routing: ProxyRouting,
}

fn default_routing() -> ProxyRouting {
    ProxyRouting::Authority
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            mode: ProxyMode::Open,
            nsids: vec![],
            routing: default_routing(),
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

    /// Routing must default to the legacy behaviour, including for configs
    /// serialised before the field existed — an instance that never opted in
    /// should not have its clients redirected by an upgrade.
    #[test]
    fn routing_defaults_to_authority() {
        assert_eq!(ProxyConfig::default().routing, ProxyRouting::Authority);

        let stored = r#"{"mode":"open","nsids":[]}"#;
        let parsed: ProxyConfig = serde_json::from_str(stored).unwrap();
        assert_eq!(parsed.routing, ProxyRouting::Authority);
    }

    #[test]
    fn routing_roundtrips() {
        let config = ProxyConfig {
            mode: ProxyMode::Open,
            nsids: vec![],
            routing: ProxyRouting::ServiceProxy,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.routing, ProxyRouting::ServiceProxy);
    }

    #[test]
    fn disabled_blocks_everything() {
        let config = ProxyConfig {
            mode: ProxyMode::Disabled,
            nsids: vec![],
            routing: ProxyRouting::Authority,
        };
        assert!(!config.allows("com.example.feed.getHot"));
        assert!(!config.allows("anything.at.all"));
    }

    #[test]
    fn open_allows_everything() {
        let config = ProxyConfig {
            mode: ProxyMode::Open,
            nsids: vec![],
            routing: ProxyRouting::Authority,
        };
        assert!(config.allows("com.example.feed.getHot"));
        assert!(config.allows("anything.at.all"));
    }

    #[test]
    fn allowlist_exact_match() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.feed.getHot".into()],
            routing: ProxyRouting::Authority,
        };
        assert!(config.allows("com.example.feed.getHot"));
        assert!(!config.allows("com.example.feed.getCold"));
    }

    #[test]
    fn allowlist_wildcard() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.*".into()],
            routing: ProxyRouting::Authority,
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
            routing: ProxyRouting::Authority,
        };
        assert!(!config.allows("com.example.feed.getHot"));
        assert!(config.allows("com.example.feed.getCold"));
    }

    #[test]
    fn blocklist_wildcard() {
        let config = ProxyConfig {
            mode: ProxyMode::Blocklist,
            nsids: vec!["com.example.*".into()],
            routing: ProxyRouting::Authority,
        };
        assert!(!config.allows("com.example.feed.getHot"));
        assert!(config.allows("com.other.feed.getHot"));
    }

    #[test]
    fn wildcard_does_not_match_prefix_without_dot() {
        let config = ProxyConfig {
            mode: ProxyMode::Allowlist,
            nsids: vec!["com.example.*".into()],
            routing: ProxyRouting::Authority,
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
            routing: ProxyRouting::Authority,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mode, ProxyMode::Allowlist);
        assert_eq!(parsed.nsids, vec!["com.example.*"]);
    }
}
