//! Where the built-in OpenCrabs provider keeps its install ID and token (#2047).
//!
//! Both live in `keys.toml` under `[providers.opencrabs]`. They are only ever
//! written together, after the proxy has issued the token, so a stored ID
//! always has its token. A missing token means no stored install at all, and
//! the caller starts over with a new ID.

use anyhow::Result;
use serde::Deserialize;

use super::opencrabs_install::is_valid_install_id;
use crate::config::{keys_path, write_secret_keys};

/// keys.toml section that holds the install ID and token.
const SECTION: &str = "providers.opencrabs";

/// An install that has a token the proxy issued for its ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredInstall {
    pub install_id: String,
    pub token: String,
}

/// Only the part of keys.toml this module reads. Other sections are ignored.
#[derive(Deserialize, Default)]
struct KeysView {
    #[serde(default)]
    providers: ProvidersView,
}

#[derive(Deserialize, Default)]
struct ProvidersView {
    #[serde(default)]
    opencrabs: Option<OpencrabsView>,
}

#[derive(Deserialize)]
struct OpencrabsView {
    install_id: Option<String>,
    api_key: Option<String>,
}

/// Read the stored install. `None` when there is no file, no section, or the
/// section lacks a valid ID or a token. A file that cannot be parsed is logged
/// and treated as no install, so the user is asked to set up again.
pub fn load() -> Option<StoredInstall> {
    load_from(&keys_path())
}

fn load_from(path: &std::path::Path) -> Option<StoredInstall> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("Could not read keys.toml for the built-in provider: {e}");
            return None;
        }
    };
    let view: KeysView = match toml::from_str(&raw) {
        Ok(view) => view,
        Err(e) => {
            tracing::warn!("keys.toml does not parse, built-in provider needs setup: {e}");
            return None;
        }
    };
    let section = view.providers.opencrabs?;
    let install_id = section.install_id.filter(|id| is_valid_install_id(id))?;
    let token = section.api_key.filter(|t| !t.is_empty())?;
    Some(StoredInstall { install_id, token })
}

/// Write the install ID and its token to keys.toml in one atomic write.
pub fn save(install: &StoredInstall) -> Result<()> {
    write_secret_keys(
        SECTION,
        &[
            ("install_id", install.install_id.as_str()),
            ("api_key", install.token.as_str()),
        ],
    )
}
