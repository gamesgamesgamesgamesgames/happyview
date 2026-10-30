//! The instance's plan limits, read once at boot (see
//! `happyview_provider::limits` for the contract).
//!
//! A hosting platform renders `INSTANCE_LIMIT_*` into the instance's
//! environment from its plan; a plan change restarts the instance, so a
//! value read at boot is current for the process's life. Unset is
//! unlimited, which is every self-hosted instance unless its operator says
//! otherwise.

use happyview_provider::{Limit, Limits};

/// The variable that sets `limit`.
pub fn var_name(limit: Limit) -> &'static str {
    match limit {
        Limit::Seats => "INSTANCE_LIMIT_SEATS",
        Limit::ApiClients => "INSTANCE_LIMIT_API_CLIENTS",
        Limit::LogRetentionDays => "INSTANCE_LIMIT_LOG_RETENTION_DAYS",
    }
}

/// Limits from `config::vars::var`. A value that is not a whole number is
/// refused: dropping it would read as unlimited, the opposite of what was
/// meant.
pub fn from_vars() -> Result<Limits, String> {
    from_lookup(crate::config::vars::var)
}

pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Limits, String> {
    let read = |limit: Limit| -> Result<Option<u32>, String> {
        let name = var_name(limit);
        match get(name).as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(raw) => raw.parse::<u32>().map(Some).map_err(|_| {
                format!(
                    "{name} must be a whole number of {}, got '{raw}'",
                    unit(limit)
                )
            }),
        }
    };
    Ok(Limits {
        seats: read(Limit::Seats)?,
        api_clients: read(Limit::ApiClients)?,
        log_retention_days: read(Limit::LogRetentionDays)?,
    })
}

fn unit(limit: Limit) -> &'static str {
    match limit {
        Limit::Seats => "seats",
        Limit::ApiClients => "API clients",
        Limit::LogRetentionDays => "days",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn from(pairs: &[(&str, &str)]) -> Result<Limits, String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn unset_and_blank_are_unlimited() {
        assert_eq!(from(&[]), Ok(Limits::default()));
        assert_eq!(
            from(&[("INSTANCE_LIMIT_SEATS", "  ")]),
            Ok(Limits::default())
        );
    }

    #[test]
    fn each_variable_sets_its_limit() {
        assert_eq!(
            from(&[
                ("INSTANCE_LIMIT_SEATS", "5"),
                ("INSTANCE_LIMIT_API_CLIENTS", " 3 "),
                ("INSTANCE_LIMIT_LOG_RETENTION_DAYS", "7"),
            ]),
            Ok(Limits {
                seats: Some(5),
                api_clients: Some(3),
                log_retention_days: Some(7),
            })
        );
    }

    #[test]
    fn a_value_that_is_not_a_whole_number_is_refused_by_name() {
        for bad in ["three", "-1", "2.5"] {
            let err = from(&[("INSTANCE_LIMIT_API_CLIENTS", bad)]).unwrap_err();
            assert!(err.contains("INSTANCE_LIMIT_API_CLIENTS"), "{err}");
            assert!(err.contains(bad), "{err}");
        }
    }
}
