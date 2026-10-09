//! Release discovery: the GitHub `releases/latest` probe, its status-aware
//! diagnostics, platform-asset presence and semver comparison.

use crate::utils::install::{InstallMethod, platform_suffix};

/// `releases/latest` endpoint for the repository this build updates from.
///
/// Resolved through [`crate::utils::update_source`] so a fork build polls the
/// fork instead of upstream.
pub(super) fn github_api() -> &'static str {
    crate::utils::update_source::releases_latest_api()
}

/// Build an honest, status-aware error string for a non-success
/// response from `releases/latest`. Replaces the prior hardcoded
/// "rate limited or unavailable" suffix that lied about every
/// non-2xx — a real 404 (no published release) and a 403 (rate
/// limit) looked identical to the user, sending us down wrong
/// debug paths.
///
/// `body_excerpt` should be the first ~300 chars of the response
/// body so the message can quote the API's own explanation when
/// it returns one (GitHub error envelopes carry a useful `message`
/// field, e.g. "API rate limit exceeded for ...").
pub(crate) fn diagnose_releases_latest_status(
    status: reqwest::StatusCode,
    body_excerpt: &str,
    ratelimit_remaining: Option<&str>,
    ratelimit_reset: Option<&str>,
) -> String {
    let code = status.as_u16();
    let body_tail = if body_excerpt.trim().is_empty() {
        String::new()
    } else {
        format!(" — API said: {}", body_excerpt.trim())
    };
    let ratelimit_tail = match (ratelimit_remaining, ratelimit_reset) {
        (Some(r), Some(reset)) => {
            format!(" [x-ratelimit-remaining={r}, x-ratelimit-reset={reset}]")
        }
        (Some(r), None) => format!(" [x-ratelimit-remaining={r}]"),
        _ => String::new(),
    };
    match code {
        404 => format!(
            "GitHub returned 404 for releases/latest — no published \
             (non-draft, non-prerelease) release exists for this repo \
             at this moment, or there's a brief publish-propagation lag. \
             Try again in a minute.{body_tail}{ratelimit_tail}"
        ),
        403 | 429 => format!(
            "GitHub rate limit hit ({code}) — unauthenticated requests \
             are capped at 60/hr per IP. Wait an hour, or set GITHUB_TOKEN \
             in your env to raise the cap to 5000/hr if you share this \
             IP.{body_tail}{ratelimit_tail}"
        ),
        500..=599 => format!(
            "GitHub API returned {code} — server-side issue, retry in a \
             few minutes.{body_tail}"
        ),
        _ => format!("GitHub API returned {status}.{body_tail}{ratelimit_tail}"),
    }
}

/// Check GitHub for a newer release. Returns `Some(latest_version)` if an
/// update is available **and** a binary asset exists for this platform,
/// `None` if already on latest, no asset ready, or on error.
pub async fn check_for_update() -> Option<String> {
    let current_version = crate::VERSION;
    let client = reqwest::Client::new();
    let resp = match client
        .get(github_api())
        .header("User-Agent", format!("opencrabs/{}", current_version))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "evolve",
                url = github_api(),
                error = %e,
                "background update check failed to reach GitHub"
            );
            return None;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body_excerpt: String = body.chars().take(300).collect();
        tracing::warn!(
            target: "evolve",
            url = github_api(),
            %status,
            body_excerpt,
            "background update check: releases/latest returned non-2xx"
        );
        return None;
    }
    let release: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "evolve",
                url = github_api(),
                error = %e,
                "background update check: failed to parse releases/latest JSON"
            );
            return None;
        }
    };

    let latest_tag = match release["tag_name"].as_str() {
        Some(t) => t,
        None => {
            tracing::warn!(
                target: "evolve",
                "background update check: releases/latest payload missing tag_name"
            );
            return None;
        }
    };
    let latest_version = latest_tag.strip_prefix('v').unwrap_or(latest_tag);

    if !is_newer(latest_version, current_version) {
        return None;
    }

    // If running from source, check if Cargo.toml already has the latest version
    if let Some(source_version) = source_cargo_version()
        && source_version == latest_version
    {
        return None;
    }

    // For pre-built binary installs, only report "available" if the platform
    // asset actually exists in the release (release may still be building).
    if matches!(InstallMethod::detect(), InstallMethod::PrebuiltBinary) {
        match stable_asset_state(&release, latest_tag) {
            StableAsset::Present => {}
            StableAsset::Building => {
                tracing::debug!(
                    "Release {} exists but no asset for this platform yet",
                    latest_tag
                );
                return None;
            }
            StableAsset::NotDistributed => {
                tracing::debug!(
                    target: "evolve",
                    tag = latest_tag,
                    os = std::env::consts::OS,
                    arch = std::env::consts::ARCH,
                    "stable channel publishes no asset for this platform"
                );
                return None;
            }
            StableAsset::UnsupportedPlatform => {
                tracing::debug!(
                    target: "evolve",
                    os = std::env::consts::OS,
                    arch = std::env::consts::ARCH,
                    "no release asset name is defined for this platform"
                );
                return None;
            }
        }
    }

    Some(latest_version.to_string())
}

/// Asset suffixes the stable release channel publishes.
///
/// Mirrors the `asset_suffix` values in `.github/workflows/release-fork.yml`,
/// which is Linux only on purpose: the header there says the fork's own hosts
/// are an aarch64 set-top box and an x86_64 VPS, and the macOS and Windows legs
/// stay in `prerelease.yml` for people who build from source.
///
/// The updater polls `releases/latest`, which excludes prereleases, so a
/// platform absent from this list cannot receive an automatic update at all.
/// Telling it "try again in a few minutes" would be a promise that never comes
/// due. `stable_channel_list_matches_the_release_workflow` in
/// `src/tests/evolve_test.rs` fails if the workflow grows a leg this list does
/// not carry.
pub(crate) const STABLE_ASSET_SUFFIXES: &[&str] = &["linux-amd64", "linux-arm64"];

/// Why the stable channel does, or does not, have an asset for this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StableAsset {
    /// The release carries an asset for this platform.
    Present,
    /// This platform is published by the stable workflow, so a missing asset
    /// most likely means the release is still uploading.
    Building,
    /// The stable workflow does not build this platform at all.
    NotDistributed,
    /// No asset name is defined for this OS/arch pair.
    UnsupportedPlatform,
}

/// Classify the stable channel's asset situation for one platform suffix.
///
/// Split out from [`stable_asset_state`] so every arm is reachable from a test
/// on any host, rather than only on the platform it describes.
pub(crate) fn classify_stable_asset(suffix: Option<&str>, asset_present: bool) -> StableAsset {
    if asset_present {
        return StableAsset::Present;
    }
    match suffix {
        None => StableAsset::UnsupportedPlatform,
        Some(s) if STABLE_ASSET_SUFFIXES.contains(&s) => StableAsset::Building,
        Some(_) => StableAsset::NotDistributed,
    }
}

/// Classify the asset situation for the platform this build runs on.
pub(crate) fn stable_asset_state(release: &serde_json::Value, tag: &str) -> StableAsset {
    classify_stable_asset(platform_suffix(), has_platform_asset(release, tag))
}

/// Check whether the release JSON contains a downloadable asset for the
/// current platform.
pub(crate) fn has_platform_asset(release: &serde_json::Value, tag: &str) -> bool {
    let suffix = match platform_suffix() {
        Some(s) => s,
        None => return false,
    };
    let ext = if std::env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let expected = format!("opencrabs-{}-{}.{}", tag, suffix, ext);
    let legacy = format!("opencrabs-{}.{}", suffix, ext);

    release["assets"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|a| {
                let name = a["name"].as_str().unwrap_or("");
                name == expected || name == legacy
            })
        })
        .unwrap_or(false)
}

/// Compare semver strings: returns true if `latest` is strictly newer than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').filter_map(|s| s.parse().ok()).collect() };
    let l = parse(latest);
    let c = parse(current);
    l > c
}

/// Try to read the version from the source Cargo.toml relative to the running
/// binary. Returns `None` if not running from a source build or file not found.
pub(super) fn source_cargo_version() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let target_dir = exe.parent()?;
    let repo_root = target_dir.parent()?.parent()?;
    let cargo_toml = repo_root.join("Cargo.toml");
    let content = std::fs::read_to_string(&cargo_toml).ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("package")?
        .get("version")?
        .as_str()
        .map(String::from)
}
