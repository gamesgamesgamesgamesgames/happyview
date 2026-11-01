use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Hardcoded seed values for the per-instance default token costs. These are
/// only used by the startup seeding step in `main.rs` to populate fresh
/// `instance_settings` rows; at runtime the values are read from the DB into
/// `RateLimitDefaults`.
pub const SEED_DEFAULT_QUERY_COST: u32 = 1;
pub const SEED_DEFAULT_PROCEDURE_COST: u32 = 1;
pub const SEED_DEFAULT_PROXY_COST: u32 = 1;

/// `instance_settings` keys for the seeded defaults.
pub const SETTING_DEFAULT_QUERY_COST: &str = "rate_limit.default_query_cost";
pub const SETTING_DEFAULT_PROCEDURE_COST: &str = "rate_limit.default_procedure_cost";
pub const SETTING_DEFAULT_PROXY_COST: &str = "rate_limit.default_proxy_cost";

/// Default token costs per XRPC request type, loaded from `instance_settings`
/// at startup. Owned by the `RateLimiter`.
#[derive(Clone, Copy)]
pub struct RateLimitDefaults {
    pub query_cost: u32,
    pub procedure_cost: u32,
    pub proxy_cost: u32,
}

pub struct RateLimitConfig {
    pub capacity: u32,
    pub refill_rate: f64,
    pub default_query_cost: u32,
    pub default_procedure_cost: u32,
    pub default_proxy_cost: u32,
}

pub enum CheckResult {
    Allowed {
        remaining: u32,
        limit: u32,
        reset: u64,
    },
    Limited {
        retry_after: u64,
        limit: u32,
        reset: u64,
    },
    Disabled,
}

struct TokenBucket {
    tokens: f64,
    capacity: u32,
    refill_rate: f64,
    last_refill: Instant,
    last_access: Instant,
}

/// Metadata about a registered API client, used for request validation.
pub struct ClientIdentity {
    /// SHA-256 hash of the client secret
    pub secret_hash: String,
    /// The client's registered URI (for Origin header validation)
    pub client_uri: String,
}

pub struct RateLimiter {
    defaults: RateLimitDefaults,
    buckets: DashMap<String, TokenBucket>,
    /// Per-client config, keyed by client_key (e.g. "hvc_..."). Presence in
    /// this map is the *only* thing that enables rate limiting for a key —
    /// unregistered keys are always allowed.
    client_configs: DashMap<String, RateLimitConfig>,
    /// Registered client identities, keyed by client_key
    client_identities: DashMap<String, ClientIdentity>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl RateLimiter {
    pub fn new(defaults: RateLimitDefaults) -> Arc<Self> {
        Arc::new(Self {
            defaults,
            buckets: DashMap::new(),
            client_configs: DashMap::new(),
            client_identities: DashMap::new(),
        })
    }

    pub fn defaults(&self) -> RateLimitDefaults {
        self.defaults
    }

    pub fn check(&self, key: &str, cost: u32) -> CheckResult {
        let (capacity, refill_rate) = match self.client_configs.get(key) {
            Some(cfg) => (cfg.capacity, cfg.refill_rate),
            None => return CheckResult::Disabled,
        };
        let cost_f64 = cost as f64;

        let now = Instant::now();

        let mut bucket = self
            .buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket {
                tokens: capacity as f64,
                capacity,
                refill_rate,
                last_refill: now,
                last_access: now,
            });

        bucket.capacity = capacity;
        bucket.refill_rate = refill_rate;

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_rate).min(capacity as f64);
        bucket.last_refill = now;
        bucket.last_access = now;

        let reset_secs = if bucket.tokens < capacity as f64 {
            ((capacity as f64 - bucket.tokens) / refill_rate).ceil() as u64
        } else {
            0
        };
        let reset = now_unix() + reset_secs;

        if bucket.tokens >= cost_f64 {
            bucket.tokens -= cost_f64;
            CheckResult::Allowed {
                remaining: bucket.tokens.floor() as u32,
                limit: capacity,
                reset,
            }
        } else {
            let retry_after = ((cost_f64 - bucket.tokens) / refill_rate).ceil() as u64;
            CheckResult::Limited {
                retry_after,
                limit: capacity,
                reset: now_unix() + ((capacity as f64) / refill_rate).ceil() as u64,
            }
        }
    }

    /// Get the default cost for a request type. Looks up the per-client
    /// override if one is registered, otherwise falls back to the seeded
    /// instance defaults.
    pub fn default_cost_for_type(&self, client_key: &str, request_type: &str) -> u32 {
        if let Some(cfg) = self.client_configs.get(client_key) {
            return match request_type {
                "query" => cfg.default_query_cost,
                "procedure" => cfg.default_procedure_cost,
                "proxy" => cfg.default_proxy_cost,
                _ => 1,
            };
        }
        match request_type {
            "query" => self.defaults.query_cost,
            "procedure" => self.defaults.procedure_cost,
            "proxy" => self.defaults.proxy_cost,
            _ => 1,
        }
    }

    pub fn register_client_config(&self, client_key: String, config: RateLimitConfig) {
        self.client_configs.insert(client_key, config);
    }

    /// Give `client_key` its bucket: the client's own capacity and refill rate
    /// when it sets both, and the instance's (`instance_capacity`,
    /// `instance_refill_rate`) otherwise.
    pub fn register_client_limit(
        &self,
        client_key: String,
        capacity: Option<i32>,
        refill_rate: Option<f64>,
        instance_capacity: u32,
        instance_refill_rate: f64,
    ) {
        let (capacity, refill_rate) = match (capacity, refill_rate) {
            (Some(capacity), Some(refill_rate)) => (capacity as u32, refill_rate),
            _ => (instance_capacity, instance_refill_rate),
        };
        self.register_client_config(
            client_key,
            RateLimitConfig {
                capacity,
                refill_rate,
                default_query_cost: self.defaults.query_cost,
                default_procedure_cost: self.defaults.procedure_cost,
                default_proxy_cost: self.defaults.proxy_cost,
            },
        );
    }

    pub fn get_client_config(&self, client_key: &str) -> Option<RateLimitConfig> {
        self.client_configs
            .get(client_key)
            .map(|cfg| RateLimitConfig {
                capacity: cfg.capacity,
                refill_rate: cfg.refill_rate,
                default_query_cost: cfg.default_query_cost,
                default_procedure_cost: cfg.default_procedure_cost,
                default_proxy_cost: cfg.default_proxy_cost,
            })
    }

    pub fn remove_client_config(&self, client_key: &str) {
        self.client_configs.remove(client_key);
    }

    pub fn register_client_identity(&self, client_key: String, identity: ClientIdentity) {
        self.client_identities.insert(client_key, identity);
    }

    pub fn remove_client_identity(&self, client_key: &str) {
        self.client_identities.remove(client_key);
    }

    pub fn validate_client_secret(&self, client_key: &str, secret: &str) -> bool {
        use sha2::{Digest, Sha256};
        if let Some(identity) = self.client_identities.get(client_key) {
            let hash = hex::encode(Sha256::digest(secret.as_bytes()));
            crate::constant_time::ct_eq_str(&hash, &identity.secret_hash)
        } else {
            false
        }
    }

    pub fn validate_client_origin(&self, client_key: &str, origin: &str) -> bool {
        if let Some(identity) = self.client_identities.get(client_key) {
            let registered = identity.client_uri.trim_end_matches('/');
            let provided = origin.trim_end_matches('/');
            registered == provided
        } else {
            false
        }
    }

    pub fn is_valid_client_key(&self, client_key: &str) -> bool {
        self.client_identities.contains_key(client_key)
    }

    pub async fn spawn_cleanup(self: Arc<Self>) {
        let interval = tokio::time::Duration::from_secs(60);
        let stale_threshold = std::time::Duration::from_secs(300);
        loop {
            tokio::time::sleep(interval).await;
            let now = Instant::now();
            self.buckets
                .retain(|_, bucket| now.duration_since(bucket.last_access) < stale_threshold);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> RateLimitDefaults {
        RateLimitDefaults {
            query_cost: 1,
            procedure_cost: 1,
            proxy_cost: 1,
        }
    }

    fn cfg(capacity: u32, refill_rate: f64) -> RateLimitConfig {
        RateLimitConfig {
            capacity,
            refill_rate,
            default_query_cost: 1,
            default_procedure_cost: 1,
            default_proxy_cost: 1,
        }
    }

    #[test]
    fn unregistered_key_is_not_rate_limited() {
        let rl = RateLimiter::new(defaults());
        for _ in 0..1000 {
            assert!(matches!(rl.check("anything", 1), CheckResult::Disabled));
        }
    }

    #[test]
    fn registered_client_is_rate_limited() {
        let rl = RateLimiter::new(defaults());
        rl.register_client_config("hvc_a".to_string(), cfg(3, 0.001));

        for _ in 0..3 {
            assert!(matches!(rl.check("hvc_a", 1), CheckResult::Allowed { .. }));
        }
        assert!(matches!(rl.check("hvc_a", 1), CheckResult::Limited { .. }));
    }

    #[test]
    fn cost_deducts_multiple_tokens() {
        let rl = RateLimiter::new(defaults());
        rl.register_client_config("hvc_a".to_string(), cfg(10, 0.001));

        assert!(matches!(
            rl.check("hvc_a", 5),
            CheckResult::Allowed { remaining: 5, .. }
        ));
        assert!(matches!(
            rl.check("hvc_a", 5),
            CheckResult::Allowed { remaining: 0, .. }
        ));
        assert!(matches!(rl.check("hvc_a", 5), CheckResult::Limited { .. }));
    }

    #[test]
    fn different_clients_get_separate_buckets() {
        let rl = RateLimiter::new(defaults());
        rl.register_client_config("hvc_a".to_string(), cfg(2, 0.001));
        rl.register_client_config("hvc_b".to_string(), cfg(2, 0.001));

        assert!(matches!(rl.check("hvc_a", 1), CheckResult::Allowed { .. }));
        assert!(matches!(rl.check("hvc_a", 1), CheckResult::Allowed { .. }));
        assert!(matches!(rl.check("hvc_a", 1), CheckResult::Limited { .. }));

        assert!(matches!(rl.check("hvc_b", 1), CheckResult::Allowed { .. }));
        assert!(matches!(rl.check("hvc_b", 1), CheckResult::Allowed { .. }));
        assert!(matches!(rl.check("hvc_b", 1), CheckResult::Limited { .. }));
    }

    #[test]
    fn a_client_limit_uses_its_overrides_only_when_both_are_set() {
        let rl = RateLimiter::new(defaults());
        for (key, capacity, refill, expected) in [
            ("hvc_both", Some(7), Some(0.5), (7, 0.5)),
            ("hvc_neither", None, None, (100, 2.0)),
            ("hvc_capacity", Some(7), None, (100, 2.0)),
            ("hvc_refill", None, Some(0.5), (100, 2.0)),
        ] {
            rl.register_client_limit(key.into(), capacity, refill, 100, 2.0);
            let cfg = rl.get_client_config(key).expect(key);
            assert_eq!((cfg.capacity, cfg.refill_rate), expected, "{key}");
        }
    }

    #[test]
    fn remove_client_config_disables_limiting() {
        let rl = RateLimiter::new(defaults());
        rl.register_client_config("hvc_temp".to_string(), cfg(1, 0.001));
        assert!(matches!(
            rl.check("hvc_temp", 1),
            CheckResult::Allowed { .. }
        ));
        assert!(matches!(
            rl.check("hvc_temp", 1),
            CheckResult::Limited { .. }
        ));

        rl.remove_client_config("hvc_temp");
        assert!(matches!(rl.check("hvc_temp", 1), CheckResult::Disabled));
    }

    #[test]
    fn default_cost_for_type_uses_seeded_defaults_when_no_client_override() {
        let rl = RateLimiter::new(RateLimitDefaults {
            query_cost: 2,
            procedure_cost: 5,
            proxy_cost: 3,
        });
        assert_eq!(rl.default_cost_for_type("nope", "query"), 2);
        assert_eq!(rl.default_cost_for_type("nope", "procedure"), 5);
        assert_eq!(rl.default_cost_for_type("nope", "proxy"), 3);
    }

    #[test]
    fn default_cost_for_type_uses_per_client_override() {
        let rl = RateLimiter::new(RateLimitDefaults {
            query_cost: 2,
            procedure_cost: 5,
            proxy_cost: 3,
        });
        rl.register_client_config(
            "hvc_a".to_string(),
            RateLimitConfig {
                capacity: 100,
                refill_rate: 1.0,
                default_query_cost: 7,
                default_procedure_cost: 8,
                default_proxy_cost: 9,
            },
        );
        assert_eq!(rl.default_cost_for_type("hvc_a", "query"), 7);
        assert_eq!(rl.default_cost_for_type("hvc_a", "procedure"), 8);
        assert_eq!(rl.default_cost_for_type("hvc_a", "proxy"), 9);
    }
}
