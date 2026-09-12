//! The smallest `auth_plugin!` consumer: every auth export, and the two host
//! imports an API-key provider needs. It talks to no real provider — each
//! handler echoes its input back in a shape the integration test can check.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use happyview_plugin_sdk::host::{self, HttpRequest};
use happyview_plugin_sdk::{
    auth_plugin, AuthorizeUrlInput, CallbackInput, ExternalProfile, PluginError, PluginInfo,
    RefreshInput, TokenInput, TokenSet,
};

auth_plugin! {
    info: PluginInfo::new("sdk_auth", "SDK Auth fixture", "1.0.0")
        .auth_type("api_key")
        .required_secrets(["PLUGIN_SDK_AUTH_API_KEY"]),
    authorize_url: authorize_url,
    callback: callback,
    refresh: refresh,
    profile: profile,
}

fn authorize_url(input: &AuthorizeUrlInput) -> Result<String, PluginError> {
    Ok(format!(
        "https://example.test/authorize?state={}&redirect={}",
        input.state, input.redirect_uri
    ))
}

fn callback(input: &CallbackInput) -> Result<TokenSet, PluginError> {
    let code = input
        .param("code")
        .ok_or_else(|| PluginError::bad_input("code is required"))?;
    Ok(TokenSet::new(code, "Bearer").refresh_token(format!("refresh-{code}")))
}

/// Returns `expires_in` rather than an absolute `expires_at`, like a provider
/// that hands back a duration; the host derives the absolute expiry.
fn refresh(input: &RefreshInput) -> Result<TokenSet, PluginError> {
    Ok(TokenSet::new(format!("access-{}", input.refresh_token), "Bearer").expires_in(3600))
}

/// Reads a secret and makes an outbound request, so the built module imports
/// both `host_get_secret` and `host_http_request`.
fn profile(input: &TokenInput) -> Result<ExternalProfile, PluginError> {
    let api_key = host::get_secret("API_KEY")?
        .ok_or_else(|| PluginError::bad_input("API_KEY is not configured"))?;
    let url = input.config["profile_url"]
        .as_str()
        .ok_or_else(|| PluginError::bad_input("config.profile_url is required"))?;
    let response = host::http_request(
        &HttpRequest::new("GET", url)
            .header("Authorization", format!("Bearer {}", input.access_token))
            .header("X-Api-Key", api_key),
    )?;
    Ok(ExternalProfile::new(response.text()).display_name("SDK Auth fixture"))
}
