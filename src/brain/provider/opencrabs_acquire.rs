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
