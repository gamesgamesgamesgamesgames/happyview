use crate::error::AppError;
use crate::spaces::types::SpaceAccess;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceReadAccess {
    Read,     // whole space — can getDelegationToken, read any repo
    ReadSelf, // own repo only — no delegation token, only own records
}

impl SpaceReadAccess {
    pub fn from_space_access(access: SpaceAccess) -> Self {
        match access {
            SpaceAccess::ReadSelf => SpaceReadAccess::ReadSelf,
            SpaceAccess::Read | SpaceAccess::Write => SpaceReadAccess::Read,
        }
    }
}

/// Check whether the caller may read a specific target repo.
///
/// Space credentials always grant full read (they were already authorized by the
/// credential issuance flow). OAuth/session callers are limited by their membership
/// access level.
pub fn check_read_access(
    caller_did: &str,
    target_repo_did: &str,
    access: SpaceReadAccess,
    has_space_credential: bool,
) -> Result<(), AppError> {
    if has_space_credential {
        return Ok(());
    }
    match access {
        SpaceReadAccess::Read => Ok(()),
        SpaceReadAccess::ReadSelf => {
            if caller_did == target_repo_did {
                Ok(())
            } else {
                Err(AppError::Forbidden(
                    "read_self access only permits reading your own repo".into(),
                ))
            }
        }
    }
}

/// Check whether the caller may call getDelegationToken.
///
/// Requires full `read` access — `read_self` members cannot obtain delegation tokens.
pub fn check_delegation_token_access(
    access: SpaceReadAccess,
    has_space_credential: bool,
) -> Result<(), AppError> {
    if has_space_credential {
        return Ok(());
    }
    match access {
        SpaceReadAccess::Read => Ok(()),
        SpaceReadAccess::ReadSelf => Err(AppError::Forbidden(
            "read_self access does not permit obtaining delegation tokens".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_access_allows_any_repo() {
        assert!(
            check_read_access("did:plc:alice", "did:plc:bob", SpaceReadAccess::Read, false).is_ok()
        );
    }

    #[test]
    fn read_self_allows_own_repo() {
        assert!(
            check_read_access(
                "did:plc:alice",
                "did:plc:alice",
                SpaceReadAccess::ReadSelf,
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn read_self_denies_other_repo() {
        assert!(
            check_read_access(
                "did:plc:alice",
                "did:plc:bob",
                SpaceReadAccess::ReadSelf,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn space_credential_bypasses_read_self() {
        assert!(
            check_read_access(
                "did:plc:alice",
                "did:plc:bob",
                SpaceReadAccess::ReadSelf,
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn delegation_token_requires_read() {
        assert!(check_delegation_token_access(SpaceReadAccess::Read, false).is_ok());
        assert!(check_delegation_token_access(SpaceReadAccess::ReadSelf, false).is_err());
    }

    #[test]
    fn delegation_token_space_credential_bypasses() {
        assert!(check_delegation_token_access(SpaceReadAccess::ReadSelf, true).is_ok());
    }

    #[test]
    fn from_space_access_mapping() {
        assert_eq!(
            SpaceReadAccess::from_space_access(SpaceAccess::Read),
            SpaceReadAccess::Read
        );
        assert_eq!(
            SpaceReadAccess::from_space_access(SpaceAccess::Write),
            SpaceReadAccess::Read
        );
        assert_eq!(
            SpaceReadAccess::from_space_access(SpaceAccess::ReadSelf),
            SpaceReadAccess::ReadSelf
        );
    }
}
