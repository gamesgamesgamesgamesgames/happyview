//! What an instance may hold or do, as whoever hosts it decides.
//!
//! HappyView asks a [`LimitsProvider`] for a [`Limits`] snapshot and checks
//! against it locally: a check never calls out to the host, so an instance
//! keeps answering while whatever set its limits is down. Where the snapshot
//! comes from (a file the host writes, variables it sets, or nothing at all
//! on a self-hosted instance) is the provider's business. A limit the
//! snapshot leaves out is unlimited, which is every limit unless something
//! sets it.
//!
//! A host can word each limit for its own users: a `message` to show in
//! place of the generic one, and an action (`actionUrl`, `actionLabel`) such
//! as a link to change plan. Nothing here knows what those say.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// One thing a host can limit. The names are the wire names, in the
/// snapshot and in a [`LimitReached`] refusal alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Limit {
    /// Admin users, the super user included.
    Seats,
    /// API clients that can call the instance.
    ApiClients,
    /// Whether XRPC can be called without an API client.
    AnonymousXrpc,
    /// A ceiling on how many days the instance's retention settings keep logs.
    LogRetentionDays,
}

impl Limit {
    pub const ALL: [Limit; 4] = [
        Limit::Seats,
        Limit::ApiClients,
        Limit::AnonymousXrpc,
        Limit::LogRetentionDays,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Limit::Seats => "seats",
            Limit::ApiClients => "api_clients",
            Limit::AnonymousXrpc => "anonymous_xrpc",
            Limit::LogRetentionDays => "log_retention_days",
        }
    }
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The host's own words for a limit. Each part is optional: without a
/// `message` a refusal says the generic sentence, and an action is shown
/// only when it has both a URL and a label.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Wording {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
}

/// A limit with a number: how many, or how many days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cap {
    pub max: u32,
    #[serde(flatten)]
    pub wording: Wording,
}

/// A limit that allows something or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gate {
    pub allowed: bool,
    #[serde(flatten)]
    pub wording: Wording,
}

/// A point-in-time view of every limit. `None` is unlimited (or, for a
/// gate, allowed).
///
/// This is also the shape of a limits file: a JSON object with any of these
/// keys, e.g. `{"seats": {"max": 5, "message": "…"}, "anonymous_xrpc":
/// {"allowed": false}}`. An unknown key is refused rather than ignored, so a
/// misspelt limit can't quietly read as unlimited.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seats: Option<Cap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_clients: Option<Cap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anonymous_xrpc: Option<Gate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_retention_days: Option<Cap>,
}

/// A refusal: the instance's limits don't allow what was asked.
///
/// `max` and `current` are there for a counted limit (`max` alone for a
/// ceiling, neither for a gate). `message` is the host's wording when it
/// gave one, otherwise the generic sentence, so it is always something to
/// show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitReached {
    pub limit: Limit,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<u32>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
}

impl LimitReached {
    fn new(limit: Limit, max: Option<u32>, current: Option<u32>, wording: &Wording) -> Self {
        let message = wording
            .message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| generic_message(limit, max));
        // An action needs both halves: a label with nowhere to go, or a
        // bare URL, is worse than none.
        let (action_url, action_label) = match (&wording.action_url, &wording.action_label) {
            (Some(url), Some(label)) if !url.trim().is_empty() && !label.trim().is_empty() => {
                (Some(url.trim().to_string()), Some(label.trim().to_string()))
            }
            _ => (None, None),
        };
        LimitReached {
            limit,
            max,
            current,
            message,
            action_url,
            action_label,
        }
    }
}

/// The sentence a refusal says when the host gave none.
fn generic_message(limit: Limit, max: Option<u32>) -> String {
    let n = max.unwrap_or(0);
    let plural = |one: &str, many: &str| {
        if n == 1 {
            one.to_string()
        } else {
            many.to_string()
        }
    };
    match limit {
        Limit::Seats => format!("This instance allows {n} {}.", plural("admin", "admins")),
        Limit::ApiClients => format!(
            "This instance allows {n} {}.",
            plural("API client", "API clients")
        ),
        Limit::AnonymousXrpc => {
            "This instance only answers XRPC calls made with an API client.".to_string()
        }
        Limit::LogRetentionDays => format!(
            "This instance keeps logs for at most {n} {}.",
            plural("day", "days")
        ),
    }
}

impl std::fmt::Display for LimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LimitReached {}

impl Limits {
    /// The cap on a counted or ceiling limit, `None` when unlimited. A gate
    /// has no cap: see [`Limits::allows_anonymous_xrpc`].
    pub fn cap_of(&self, limit: Limit) -> Option<&Cap> {
        match limit {
            Limit::Seats => self.seats.as_ref(),
            Limit::ApiClients => self.api_clients.as_ref(),
            Limit::LogRetentionDays => self.log_retention_days.as_ref(),
            Limit::AnonymousXrpc => None,
        }
    }

    /// `Err` when adding one more to `current` would pass the limit. A count
    /// already over it (the limit was lowered under it) refuses too, and
    /// keeps what is there: a limit never takes anything away.
    pub fn check(&self, limit: Limit, current: u32) -> Result<(), LimitReached> {
        match self.cap_of(limit) {
            Some(cap) if current >= cap.max => Err(LimitReached::new(
                limit,
                Some(cap.max),
                Some(current),
                &cap.wording,
            )),
            _ => Ok(()),
        }
    }

    /// `Err` when XRPC without an API client is refused.
    pub fn allows_anonymous_xrpc(&self) -> Result<(), LimitReached> {
        match &self.anonymous_xrpc {
            Some(gate) if !gate.allowed => Err(LimitReached::new(
                Limit::AnonymousXrpc,
                None,
                None,
                &gate.wording,
            )),
            _ => Ok(()),
        }
    }

    /// For a ceiling on a setting: the smaller of `configured` and the
    /// limit. A configured `0`, which means "forever" in the settings it
    /// caps, becomes the limit when there is one.
    pub fn cap(&self, limit: Limit, configured: u32) -> u32 {
        match self.cap_of(limit) {
            Some(cap) if configured == 0 => cap.max,
            Some(cap) => configured.min(cap.max),
            None => configured,
        }
    }

    /// `Err` when a setting asks for more than the ceiling allows. For a
    /// write that sets the value outright, where quietly lowering it would
    /// leave the owner thinking it took.
    pub fn check_ceiling(&self, limit: Limit, requested: u32) -> Result<(), LimitReached> {
        match self.cap_of(limit) {
            Some(cap) if requested == 0 || requested > cap.max => {
                Err(LimitReached::new(limit, Some(cap.max), None, &cap.wording))
            }
            _ => Ok(()),
        }
    }
}

/// Where HappyView gets its limits. Asked for a snapshot each time it
/// checks, so a provider whose limits change while the instance runs needs
/// no other hook. It must answer from what it already holds: this is called
/// on hot paths, including every XRPC request.
pub trait LimitsProvider: Send + Sync {
    fn limits(&self) -> Arc<Limits>;
}

/// Limits fixed for the life of the process. The default is unlimited.
#[derive(Debug, Clone, Default)]
pub struct FixedLimits(pub Arc<Limits>);

impl FixedLimits {
    pub fn new(limits: Limits) -> Self {
        FixedLimits(Arc::new(limits))
    }
}

impl LimitsProvider for FixedLimits {
    fn limits(&self) -> Arc<Limits> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(max: u32) -> Option<Cap> {
        Some(Cap {
            max,
            wording: Wording::default(),
        })
    }

    fn limited() -> Limits {
        Limits {
            seats: cap(3),
            api_clients: cap(1),
            anonymous_xrpc: Some(Gate {
                allowed: false,
                wording: Wording::default(),
            }),
            log_retention_days: cap(7),
        }
    }

    #[test]
    fn check_refuses_at_and_over_the_limit() {
        let l = limited();
        assert_eq!(l.check(Limit::Seats, 2), Ok(()));
        let at = l.check(Limit::Seats, 3).unwrap_err();
        assert_eq!(
            (at.limit, at.max, at.current),
            (Limit::Seats, Some(3), Some(3))
        );
        assert!(
            l.check(Limit::Seats, 5).is_err(),
            "over after the limit was lowered"
        );
        assert!(l.check(Limit::ApiClients, 1).is_err());
    }

    #[test]
    fn unlimited_never_refuses() {
        let none = Limits::default();
        for limit in Limit::ALL {
            assert_eq!(none.check(limit, u32::MAX), Ok(()));
            assert_eq!(none.check_ceiling(limit, 0), Ok(()));
        }
        assert_eq!(none.allows_anonymous_xrpc(), Ok(()));
    }

    #[test]
    fn a_gate_refuses_only_when_it_says_so() {
        assert!(limited().allows_anonymous_xrpc().is_err());
        let open = Limits {
            anonymous_xrpc: Some(Gate {
                allowed: true,
                wording: Wording::default(),
            }),
            ..Limits::default()
        };
        assert_eq!(open.allows_anonymous_xrpc(), Ok(()));
        let refused = limited().allows_anonymous_xrpc().unwrap_err();
        assert_eq!((refused.max, refused.current), (None, None));
    }

    #[test]
    fn a_gate_is_never_counted() {
        assert_eq!(limited().check(Limit::AnonymousXrpc, u32::MAX), Ok(()));
    }

    #[test]
    fn cap_takes_the_smaller_and_turns_forever_into_the_limit() {
        let l = limited();
        assert_eq!(l.cap(Limit::LogRetentionDays, 30), 7);
        assert_eq!(l.cap(Limit::LogRetentionDays, 3), 3);
        assert_eq!(l.cap(Limit::LogRetentionDays, 0), 7);
        assert_eq!(Limits::default().cap(Limit::LogRetentionDays, 0), 0);
        assert_eq!(Limits::default().cap(Limit::LogRetentionDays, 90), 90);
    }

    #[test]
    fn a_ceiling_refuses_more_and_forever() {
        let l = limited();
        assert_eq!(l.check_ceiling(Limit::LogRetentionDays, 7), Ok(()));
        let over = l.check_ceiling(Limit::LogRetentionDays, 30).unwrap_err();
        assert_eq!((over.max, over.current), (Some(7), None));
        assert!(l.check_ceiling(Limit::LogRetentionDays, 0).is_err());
    }

    #[test]
    fn the_generic_message_reads_as_a_sentence() {
        let l = Limits {
            seats: cap(1),
            api_clients: cap(3),
            log_retention_days: cap(1),
            ..limited()
        };
        assert_eq!(
            l.check(Limit::Seats, 1).unwrap_err().message,
            "This instance allows 1 admin."
        );
        assert_eq!(
            l.check(Limit::ApiClients, 3).unwrap_err().message,
            "This instance allows 3 API clients."
        );
        assert_eq!(
            l.check_ceiling(Limit::LogRetentionDays, 2)
                .unwrap_err()
                .message,
            "This instance keeps logs for at most 1 day."
        );
        assert_eq!(
            l.allows_anonymous_xrpc().unwrap_err().message,
            "This instance only answers XRPC calls made with an API client."
        );
    }

    #[test]
    fn the_hosts_wording_is_used_and_an_action_needs_both_halves() {
        let worded = |url: Option<&str>, label: Option<&str>| Limits {
            seats: Some(Cap {
                max: 1,
                wording: Wording {
                    message: Some("Your plan includes 1 seat.".into()),
                    action_url: url.map(Into::into),
                    action_label: label.map(Into::into),
                },
            }),
            ..Limits::default()
        };
        let both = worded(Some("https://host.example/plan"), Some("Add a seat"))
            .check(Limit::Seats, 1)
            .unwrap_err();
        assert_eq!(both.message, "Your plan includes 1 seat.");
        assert_eq!(
            both.action_url.as_deref(),
            Some("https://host.example/plan")
        );
        assert_eq!(both.action_label.as_deref(), Some("Add a seat"));

        for (url, label) in [
            (Some("https://host.example/plan"), None),
            (None, Some("Add a seat")),
        ] {
            let half = worded(url, label).check(Limit::Seats, 1).unwrap_err();
            assert_eq!((half.action_url, half.action_label), (None, None));
        }
    }

    #[test]
    fn a_blank_message_falls_back_to_the_generic_one() {
        let l = Limits {
            seats: Some(Cap {
                max: 2,
                wording: Wording {
                    message: Some("  ".into()),
                    ..Wording::default()
                },
            }),
            ..Limits::default()
        };
        assert_eq!(
            l.check(Limit::Seats, 2).unwrap_err().message,
            "This instance allows 2 admins."
        );
    }

    #[test]
    fn the_refusal_serialises_with_wire_names() {
        let reached = Limits {
            api_clients: Some(Cap {
                max: 1,
                wording: Wording {
                    message: None,
                    action_url: Some("https://host.example/plan".into()),
                    action_label: Some("Upgrade".into()),
                },
            }),
            ..Limits::default()
        }
        .check(Limit::ApiClients, 1)
        .unwrap_err();
        assert_eq!(
            serde_json::to_value(&reached).unwrap(),
            serde_json::json!({
                "limit": "api_clients",
                "max": 1,
                "current": 1,
                "message": "This instance allows 1 API client.",
                "actionUrl": "https://host.example/plan",
                "actionLabel": "Upgrade",
            })
        );
    }

    #[test]
    fn a_snapshot_reads_from_json_and_refuses_unknown_keys() {
        let parsed: Limits = serde_json::from_str(
            r#"{"seats": {"max": 5, "message": "Five seats.", "actionUrl": "https://h.example", "actionLabel": "More"},
                "anonymous_xrpc": {"allowed": false}}"#,
        )
        .unwrap();
        assert_eq!(parsed.seats.as_ref().unwrap().max, 5);
        assert_eq!(
            parsed.seats.as_ref().unwrap().wording.message.as_deref(),
            Some("Five seats.")
        );
        assert_eq!(parsed.api_clients, None);
        assert!(!parsed.anonymous_xrpc.as_ref().unwrap().allowed);

        assert!(
            serde_json::from_str::<Limits>(r#"{"seat": {"max": 5}}"#).is_err(),
            "misspelt limit"
        );
        assert!(
            serde_json::from_str::<Limits>(r#"{"seats": {"max": 5, "msg": "x"}}"#).is_err(),
            "misspelt field"
        );
        assert!(serde_json::from_str::<Limits>(r#"{"seats": {"max": -1}}"#).is_err());
        assert_eq!(
            serde_json::from_str::<Limits>("{}").unwrap(),
            Limits::default()
        );
    }

    #[test]
    fn wire_names_are_snake_case() {
        for limit in Limit::ALL {
            assert_eq!(serde_json::to_value(limit).unwrap(), limit.as_str());
        }
    }

    #[test]
    fn fixed_limits_hand_back_the_same_snapshot() {
        let fixed = FixedLimits::new(limited());
        assert_eq!(*fixed.limits(), limited());
        assert_eq!(*FixedLimits::default().limits(), Limits::default());
    }
}
