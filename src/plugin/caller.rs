//! The script runner's identity, carried into every library call it makes so
//! a library can act as the user who ran the script rather than as itself.

use std::sync::Arc;

use crate::AppState;
use crate::auth::Claims;
use crate::repo::PdsAuth;

/// Everything a host import needs to speak to the network as the calling
/// script's user. Present only on runners that hold the user's credentials —
/// a query, record-event or label script has none, and its library calls
/// carry no session at all.
pub struct CallerSession {
    pub did: String,
    /// The repo record writes default to, when the script runs on another
    /// account's behalf.
    pub delegate_did: Option<String>,
    pub claims: Arc<Claims>,
    /// Crate-visible because `PdsAuth` is: how this instance holds the user's
    /// credentials is not a plugin's business.
    pub(crate) pds_auth: Arc<PdsAuth>,
    pub app_state: AppState,
}

impl CallerSession {
    /// The repo a record write targets when the caller names none.
    pub fn default_repo(&self) -> &str {
        self.delegate_did.as_deref().unwrap_or(&self.did)
    }
}

/// Reject a record write aimed at a repo the caller has no credentials for.
///
/// A write acts as the caller either way: the DPoP path looks up a session
/// keyed by the target DID, and the cookie path presents the caller's own
/// token. Neither can write to somebody else's repo, so such a write can never
/// succeed — the only question is how confusingly it fails.
///
/// Badly, without this check. The DPoP path dies deep in session lookup with
/// `PDS createRecord failed: not found: DPoP session not found`, which reads as
/// a broken login. That error cost one reporter several days and three
/// rewritten OAuth clients before the real answer surfaced: the instance was
/// never given access to the repo the script had redirected the write to.
///
/// Writing to another account's repo is what [`linked_repos`](crate::lua::linked_repos_api)
/// is for.
pub fn check_writable_repo(
    repo: &str,
    caller_did: &str,
    delegate_did: Option<&str>,
) -> Result<(), String> {
    if repo == caller_did || Some(repo) == delegate_did {
        return Ok(());
    }
    Err(format!(
        "cannot write to repo {repo}: a Record write acts as the caller \
         ({caller_did}), so it can only target the caller's own repo. To write \
         to another account's repo, an admin must link that repo, and the \
         script should use linked_repos.get(\"{repo}\") instead of set_repo()."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALLER: &str = "did:plc:caller";
    const OTHER: &str = "did:plc:someoneelse";

    #[test]
    fn writes_to_the_callers_own_repo_are_allowed() {
        assert!(check_writable_repo(CALLER, CALLER, None).is_ok());
    }

    #[test]
    fn writes_to_a_delegated_repo_are_allowed() {
        let delegate = "did:plc:delegated";
        assert!(check_writable_repo(delegate, CALLER, Some(delegate)).is_ok());
        // The caller's own repo stays writable while delegating.
        assert!(check_writable_repo(CALLER, CALLER, Some(delegate)).is_ok());
    }

    #[test]
    fn writes_to_a_foreign_repo_are_refused() {
        let msg = check_writable_repo(OTHER, CALLER, None)
            .expect_err("writing to another account's repo cannot succeed");

        // The message has to name the repo that was actually targeted and
        // point at the mechanism that does work. The failure it replaces —
        // `DPoP session not found` — named neither, and reads as a broken
        // login rather than a repo the instance has no access to.
        assert!(msg.contains(OTHER), "should name the target repo: {msg}");
        assert!(msg.contains(CALLER), "should name the caller: {msg}");
        assert!(
            msg.contains("linked_repos"),
            "should point at linked repos: {msg}"
        );
        assert!(
            !msg.contains("DPoP"),
            "should not blame the caller's session: {msg}"
        );
    }

    #[test]
    fn delegating_does_not_open_up_unrelated_repos() {
        assert!(check_writable_repo(OTHER, CALLER, Some("did:plc:delegated")).is_err());
    }
}
