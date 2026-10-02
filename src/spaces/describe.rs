//! `community.lexicon.service.describe`: capability self-description.
//!
//! An unratified community lexicon, pioneered by bulleted/atpint, that answers
//! "which XRPC methods does this service implement". It exists because atproto
//! publishes no method list: without it, the only way to ask is to call a method
//! and read the failure, where a server missing an optional feature and a broken
//! one look alike.
//!
//! Serving it makes HappyView detectable by other apps, and is the same signal
//! HappyView reads when deciding whether a user's PDS can host spaces natively.
//!
//! Shape follows the lexicon proposed to `lexicon.community`
//! (<https://tangled.org/lexicon.community/lexicons/pulls/2>), which defines
//! `methods` as the sole required property. Existing implementations (ZDS,
//! `blacksky-algorithms/rsky`, `wearenewpublic/atproto-crates`) also emit a
//! `roles` array, but it is absent from the proposed schema and no consumer
//! reads it (`hatk-dev/hatk` and `grainsocial/grain` both parse only
//! `methods[].value`), so this serves `methods` alone.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::AppState;
use crate::error::AppError;

pub const DESCRIBE_NSID: &str = "community.lexicon.service.describe";

/// The `$type` each advertised method carries.
const NSID_ENTRY_TYPE: &str = "community.lexicon.service.describe#nsid";

pub fn describe_routes() -> Router<AppState> {
    Router::new().route(&format!("/xrpc/{DESCRIBE_NSID}"), get(describe))
}

/// Every method this build serves.
///
/// Derived from the same tables that register the routes, so the advertisement
/// cannot list a method that is not routed. Over-claiming is the dangerous
/// direction: a client that detects a method we do not serve will fail on the
/// call it was told would work.
async fn advertised_methods(state: &AppState) -> Vec<String> {
    // Spaces are behind a feature flag. With it off, nothing is advertised, so a
    // client can tell a build without spaces from one with them.
    if !crate::feature_flags::is_enabled(
        &state.db,
        crate::feature_flags::FeatureFlag::SPACES_ENABLED,
        state.db_backend,
    )
    .await
    {
        return Vec::new();
    }

    let mut methods: Vec<String> = crate::spaces::routes::protocol_method_table()
        .into_iter()
        .map(|(nsid, _)| nsid)
        .chain(
            crate::spaces::simplespace::management_method_table()
                .into_iter()
                .map(|(nsid, _)| nsid),
        )
        .collect();

    methods.push(DESCRIBE_NSID.to_string());
    methods.sort();
    methods.dedup();
    methods
}

/// Public: detection happens before authorization, so requiring auth
/// would defeat the purpose.
async fn describe(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let methods: Vec<serde_json::Value> = advertised_methods(&state)
        .await
        .into_iter()
        .map(|nsid| serde_json::json!({ "$type": NSID_ENTRY_TYPE, "value": nsid }))
        .collect();

    // No `roles`; see the module docs.
    Ok(Json(serde_json::json!({ "methods": methods })))
}
