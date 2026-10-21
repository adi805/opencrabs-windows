//! Get this install's caller token for the built-in OpenCrabs provider (#2047).
//!
//! A stored install is reused as-is, so a second launch never calls the proxy.
//! Without one, a fresh ID is generated and sent to the install route. A `409`
//! means the proxy already holds a token for that ID, which only happens if the
//! ID repeats, so the request is retried once with a new ID before giving up.

use std::time::Duration;

use super::opencrabs_install::{
    INSTALL_PATH, InstallError, IssuedToken, OPENCRABS_BASE_URL, install_request_body,
    new_install_id, parse_install_response,
};
use super::opencrabs_store::{self, StoredInstall};
use crate::config::Config;

/// Most requests made for one acquire, counting the first.
const ATTEMPTS: usize = 2;

/// Longest wait for the proxy to answer one install request.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Return this install's stored token, or request and store a new one.
pub async fn ensure_install() -> Result<StoredInstall, InstallError> {
    ensure_install_at(OPENCRABS_BASE_URL).await
}

/// [`ensure_install`] against an explicit proxy base URL, so tests can point
/// it at a local server.
pub async fn ensure_install_at(base_url: &str) -> Result<StoredInstall, InstallError> {
    if let Some(install) = opencrabs_store::load() {
        return Ok(install);
    }

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| {
            tracing::error!("Could not build the HTTP client for the built-in provider: {e}");
            InstallError::Unreachable
        })?;
    let url = format!("{}{}", base_url.trim_end_matches('/'), INSTALL_PATH);

    let mut last = InstallError::ProxyFailed;
    for _ in 0..ATTEMPTS {
        let install_id = new_install_id();
        match post_install(&client, &url, &install_id).await {
            Ok(issued) => {
                let install = StoredInstall {
                    install_id,
                    token: issued.token,
                };
                opencrabs_store::save(&install).map_err(|e| {
                    tracing::error!("Could not save the built-in provider token: {e}");
                    InstallError::StorageFailed
                })?;
                return Ok(install);
            }
            Err(InstallError::AlreadyIssued) => last = InstallError::AlreadyIssued,
            Err(other) => return Err(other),
        }
    }
    Err(last)
}

/// One request to the install route, read through the pure parser.
async fn post_install(
    client: &reqwest::Client,
    url: &str,
    install_id: &str,
) -> Result<IssuedToken, InstallError> {
    let response = client
        .post(url)
        .json(&install_request_body(install_id))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("Built-in provider install request failed: {e}");
            InstallError::Unreachable
        })?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    parse_install_response(status, &body)
}

/// Whether this install still needs the built-in provider provisioned (#2151).
///
/// True only when the section exists, it holds no token, and no other provider
/// can serve a request. That last condition is what keeps this from hijacking
/// a configured install: a user with a working provider is never touched,
/// whatever their `[providers.opencrabs]` section happens to say.
///
/// The section ships disabled in the documented example, so `enabled` is
/// deliberately not part of the test. A fresh home has it disabled and a
/// token-less section is exactly the install that cannot answer a request.
pub fn needs_builtin_provision(config: &Config) -> bool {
    let Some(section) = config.providers.opencrabs.as_ref() else {
        return false;
    };
    if section.api_key.as_ref().is_some_and(|key| !key.is_empty()) {
        return false;
    }
    // Ask the registry the factory walks, with the built-in section switched
    // off: whatever it answers is a provider this install can actually use,
    // and such an install must not be touched.
    //
    // Switching the section off instead of reading
    // `active_provider_and_model()` directly is what makes the empty-token
    // case work. That walk only asks whether `api_key` is `Some`, and an empty
    // string is `Some`, so an `api_key = ""` section reports "opencrabs" for an
    // install that has no usable provider at all (`try_create_opencrabs`
    // filters empty tokens out). This is the exact shape of the reported
    // deployment, so trusting the unmodified answer would skip the one case
    // the fix exists for.
    let mut without_builtin = config.clone();
    if let Some(section) = without_builtin.providers.opencrabs.as_mut() {
        section.enabled = false;
    }
    let (provider, _) = without_builtin.providers.active_provider_and_model();
    provider == "none"
}

/// Put an issued token into the in-memory config (#2151).
///
/// `ensure_install` writes `keys.toml`, but every caller here already holds a
/// `Config` it loaded before that write, and `try_create_opencrabs` reads the
/// token from the config. Without this the file would carry a token the
/// factory still cannot see, and the command would fail exactly as before.
pub fn apply_install(config: &mut Config, token: &str) {
    if let Some(section) = config.providers.opencrabs.as_mut() {
        section.enabled = true;
        section.api_key = Some(token.to_string());
    }
}

/// Zero-setup for the entry points that never open the TUI (#2151).
///
/// The TUI provisions on first run (`App::provision_first_run`): it enables the
/// section, asks the proxy for this install's token and rebuilds onto it. A
/// machine that only ever runs `opencrabs run`, `daemon` or the cron scheduler
/// had no such path, so it started with an empty key and reported "No provider
/// configured" on a fresh home, with no way out short of a TTY.
///
/// This is the provider half of that provisioning. It returns whether a token
/// was acquired, and failure is never fatal: the caller's own "no provider"
/// error is more useful than a new one here, and a proxy that is down must not
/// stop a user whose other provider is configured.
///
/// One `POST /v1/install` on first launch only, because `ensure_install`
/// returns the stored install without a request.
pub async fn provision_builtin_provider(config: &mut Config) -> bool {
    if !needs_builtin_provision(config) {
        return false;
    }

    tracing::info!("First run: requesting the built-in provider token");
    if let Err(e) = Config::write_key("providers.opencrabs", "enabled", "true") {
        // Not fatal. The in-memory section is set below either way, and the
        // token write is the part a later launch depends on.
        tracing::warn!("Could not enable the built-in provider in config.toml: {e}");
    }

    let install = match ensure_install().await {
        Ok(install) => install,
        Err(e) => {
            tracing::warn!("Built-in provider token request failed: {e:?}");
            return false;
        }
    };

    apply_install(config, &install.token);

    // The install is set up now, so a later TUI launch goes straight to chat
    // instead of running first-run provisioning a second time.
    crate::tui::onboarding::state::OnboardingState::mark_completed();
    true
}
