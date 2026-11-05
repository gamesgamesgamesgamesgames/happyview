//! atrium-oauth 0.1.7 answers two failures with `todo!()` instead of an error:
//! `OAuthClient::callback` when the token exchange is refused, and
//! `OAuthClient::authorize` when the authorization server has no PAR endpoint
//! and does not require one. A `match` on either result cannot see the panic,
//! so it unwinds the handler and the browser gets a dropped connection.
//! [`catch`] turns it back into an error the caller can answer.

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;

/// What a caller says when `callback` panics. atrium drops the server's
/// reason before the `todo!()`, so there is nothing more specific to say.
pub const EXCHANGE_REFUSED: &str =
    "the authorization server did not accept the sign-in; start it again";

/// What a caller says when `authorize` panics.
pub const NO_PAR_ENDPOINT: &str = "the account's authorization server does not offer pushed \
     authorization requests, which atproto OAuth requires";

/// The message of a panic caught by [`catch`].
#[derive(Debug)]
pub struct Panicked(pub String);

impl std::fmt::Display for Panicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runs an atrium-oauth call, answering a panic inside it as [`Panicked`].
///
/// Unwind safety holds because the client keeps no in-memory state a later
/// call reads: its stores are database tables, and each `todo!()` runs after
/// its last write.
pub async fn catch<T>(fut: impl Future<Output = T>) -> Result<T, Panicked> {
    AssertUnwindSafe(fut)
        .catch_unwind()
        .await
        .map_err(|payload| {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            Panicked(message)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_todo_becomes_an_error() {
        let result: Result<(), Panicked> = catch(async { todo!() }).await;
        assert_eq!(result.unwrap_err().0, "not yet implemented");
    }

    #[tokio::test]
    async fn a_value_passes_through() {
        assert_eq!(catch(async { 7 }).await.unwrap(), 7);
    }
}
