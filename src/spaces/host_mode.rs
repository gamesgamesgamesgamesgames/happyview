//! Where a permissioned repo lives, and how it moves.
//!
//! HappyView's spaces storage is a **polyfill** for PDSes that do not support
//! spaces. Each repo moves off it independently, so the mode is per
//! `(space, author)` rather than per user. A user may be native in one space and
//! polyfill in another, since the space authority's support is independent of
//! their own.

use std::fmt;

/// Which host is authoritative for a repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostMode {
    /// HappyView is the repo host and the source of truth. Every repo starts
    /// here.
    #[default]
    Polyfill,
    /// Records are being replayed into the user's PDS. HappyView is still
    /// authoritative: the handoff has not verified yet.
    Migrating,
    /// The user's PDS is the source of truth; HappyView indexes it.
    Native,
}

impl HostMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Polyfill => "polyfill",
            Self::Migrating => "migrating",
            Self::Native => "native",
        }
    }

    /// Parse a stored value, falling back to `Polyfill`.
    ///
    /// An unreadable mode must not be treated as `Native`, which would point the
    /// source of truth at a PDS that may hold nothing.
    pub fn parse_or_default(value: &str) -> Self {
        match value {
            "migrating" => Self::Migrating,
            "native" => Self::Native,
            _ => Self::Polyfill,
        }
    }

    /// Whether HappyView still owns this repo's writes.
    pub fn is_authoritative_here(self) -> bool {
        matches!(self, Self::Polyfill | Self::Migrating)
    }
}

impl fmt::Display for HostMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a mode change is one the migration flow can make.
///
/// `native -> polyfill` is allowed: if a PDS stops serving spaces, the repo can
/// fall back, because migration keeps the local copy. `polyfill -> native` is
/// not: skipping `Migrating` would make the PDS authoritative without the
/// verify step running.
pub fn can_transition(from: HostMode, to: HostMode) -> bool {
    matches!(
        (from, to),
        (HostMode::Polyfill, HostMode::Migrating)
            | (HostMode::Migrating, HostMode::Native)
            // A failed handoff returns the repo to where it started.
            | (HostMode::Migrating, HostMode::Polyfill)
            // The PDS dropped support, or an operator rolled back.
            | (HostMode::Native, HostMode::Polyfill)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_cannot_skip_the_migrating_state() {
        assert!(!can_transition(HostMode::Polyfill, HostMode::Native));
    }

    #[test]
    fn a_failed_migration_returns_to_polyfill() {
        assert!(can_transition(HostMode::Migrating, HostMode::Polyfill));
    }

    #[test]
    fn native_can_fall_back_because_the_local_copy_is_retained() {
        assert!(can_transition(HostMode::Native, HostMode::Polyfill));
    }

    #[test]
    fn a_mode_never_transitions_to_itself() {
        for mode in [HostMode::Polyfill, HostMode::Migrating, HostMode::Native] {
            assert!(!can_transition(mode, mode), "{mode} -> {mode}");
        }
    }

    #[test]
    fn an_unreadable_mode_reads_as_polyfill() {
        assert_eq!(HostMode::parse_or_default("native"), HostMode::Native);
        assert_eq!(HostMode::parse_or_default("migrating"), HostMode::Migrating);
        assert_eq!(HostMode::parse_or_default("polyfill"), HostMode::Polyfill);
        assert_eq!(HostMode::parse_or_default(""), HostMode::Polyfill);
        assert_eq!(HostMode::parse_or_default("nonsense"), HostMode::Polyfill);
    }

    #[test]
    fn happyview_owns_writes_until_the_handoff_verifies() {
        assert!(HostMode::Polyfill.is_authoritative_here());
        // Still ours mid-migration: the PDS copy is not trusted until verified.
        assert!(HostMode::Migrating.is_authoritative_here());
        assert!(!HostMode::Native.is_authoritative_here());
    }

    #[test]
    fn the_stored_form_round_trips() {
        for mode in [HostMode::Polyfill, HostMode::Migrating, HostMode::Native] {
            assert_eq!(HostMode::parse_or_default(mode.as_str()), mode);
        }
    }
}
