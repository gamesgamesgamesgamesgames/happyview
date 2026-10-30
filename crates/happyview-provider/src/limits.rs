//! What an instance's plan allows: how many of a thing it may hold, or how
//! long it may keep it. HappyView asks a [`LimitsProvider`] for a [`Limits`]
//! snapshot and checks against it; where the numbers come from (the
//! environment a hosting platform renders, or nothing at all on a
//! self-hosted instance) is the provider's business.

use serde::Serialize;

/// One thing a plan can limit. The names are the wire names, in the
/// snapshot and in a `LimitReached` refusal alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Limit {
    /// Admin users, the super user included.
    Seats,
    /// API clients that can call the instance.
    ApiClients,
    /// How many days event logs are kept.
    LogRetentionDays,
}

impl Limit {
    pub const ALL: [Limit; 3] = [Limit::Seats, Limit::ApiClients, Limit::LogRetentionDays];

    pub fn as_str(self) -> &'static str {
        match self {
            Limit::Seats => "seats",
            Limit::ApiClients => "api_clients",
            Limit::LogRetentionDays => "log_retention_days",
        }
    }
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A point-in-time view of every limit. `None` is unlimited, and is what
/// every limit is unless something sets it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Limits {
    pub seats: Option<u32>,
    pub api_clients: Option<u32>,
    pub log_retention_days: Option<u32>,
}

/// A refusal: adding one more would pass `limit`'s `max`, with `current`
/// already in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LimitReached {
    pub limit: Limit,
    pub max: u32,
    pub current: u32,
}

impl std::fmt::Display for LimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let noun = match (self.limit, self.max) {
            (Limit::Seats, 1) => "seat",
            (Limit::Seats, _) => "seats",
            (Limit::ApiClients, 1) => "API client",
            (Limit::ApiClients, _) => "API clients",
            (Limit::LogRetentionDays, 1) => "day of logs",
            (Limit::LogRetentionDays, _) => "days of logs",
        };
        write!(f, "this instance's plan allows {} {noun}", self.max)
    }
}

impl std::error::Error for LimitReached {}

impl Limits {
    /// The limit on `limit`, `None` when unlimited.
    pub fn get(&self, limit: Limit) -> Option<u32> {
        match limit {
            Limit::Seats => self.seats,
            Limit::ApiClients => self.api_clients,
            Limit::LogRetentionDays => self.log_retention_days,
        }
    }

    /// `Err` when adding one more to `current` would pass the limit. A count
    /// already over it (a plan moved down under it) refuses too, and keeps
    /// what is there: a limit never takes anything away.
    pub fn check(&self, limit: Limit, current: u32) -> Result<(), LimitReached> {
        match self.get(limit) {
            Some(max) if current >= max => Err(LimitReached {
                limit,
                max,
                current,
            }),
            _ => Ok(()),
        }
    }

    /// For a limit that caps a setting rather than counting things: the
    /// smaller of `configured` and the limit. A configured `0`, which means
    /// "no limit" in the settings it caps, becomes the limit when there is one.
    pub fn cap(&self, limit: Limit, configured: u32) -> u32 {
        match self.get(limit) {
            Some(max) if configured == 0 => max,
            Some(max) => configured.min(max),
            None => configured,
        }
    }
}

/// Where HappyView reads its limits from. Asked for a fresh snapshot each
/// time, so a provider whose numbers can change without a restart needs no
/// other hook.
pub trait LimitsProvider: Send + Sync {
    fn limits(&self) -> Limits;
}

/// Limits fixed for the life of the process: those read once at boot.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixedLimits(pub Limits);

impl LimitsProvider for FixedLimits {
    fn limits(&self) -> Limits {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITED: Limits = Limits {
        seats: Some(3),
        api_clients: Some(1),
        log_retention_days: Some(7),
    };

    #[test]
    fn check_refuses_at_and_over_the_limit() {
        assert_eq!(LIMITED.check(Limit::Seats, 2), Ok(()));
        assert_eq!(
            LIMITED.check(Limit::Seats, 3),
            Err(LimitReached {
                limit: Limit::Seats,
                max: 3,
                current: 3
            })
        );
        assert!(
            LIMITED.check(Limit::Seats, 5).is_err(),
            "over after a downgrade"
        );
        assert!(LIMITED.check(Limit::ApiClients, 1).is_err());
    }

    #[test]
    fn unlimited_never_refuses() {
        let none = Limits::default();
        for limit in Limit::ALL {
            assert_eq!(none.check(limit, u32::MAX), Ok(()));
        }
    }

    #[test]
    fn cap_takes_the_smaller_and_turns_forever_into_the_limit() {
        assert_eq!(LIMITED.cap(Limit::LogRetentionDays, 30), 7);
        assert_eq!(LIMITED.cap(Limit::LogRetentionDays, 3), 3);
        assert_eq!(LIMITED.cap(Limit::LogRetentionDays, 0), 7);
        assert_eq!(Limits::default().cap(Limit::LogRetentionDays, 0), 0);
        assert_eq!(Limits::default().cap(Limit::LogRetentionDays, 90), 90);
    }

    #[test]
    fn the_refusal_reads_as_a_sentence() {
        let one = LimitReached {
            limit: Limit::Seats,
            max: 1,
            current: 1,
        };
        assert_eq!(one.to_string(), "this instance's plan allows 1 seat");
        let many = LimitReached {
            limit: Limit::ApiClients,
            max: 3,
            current: 3,
        };
        assert_eq!(
            many.to_string(),
            "this instance's plan allows 3 API clients"
        );
    }

    #[test]
    fn wire_names_are_snake_case() {
        assert_eq!(
            serde_json::to_value(Limit::ApiClients).unwrap(),
            "api_clients"
        );
        for limit in Limit::ALL {
            assert_eq!(serde_json::to_value(limit).unwrap(), limit.as_str());
        }
    }
}
