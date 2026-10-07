//! Single source of truth for the repository this build fetches updates from.
//!
//! Upstream ships from `opencrabs/opencrabs`. A fork ships its own builds, so
//! every update path (`/evolve`, crash-recovery rollback, the self-update
//! `git clone`, and RSI template sync) resolves its repo slug and derived
//! URLs from here instead of hardcoding a literal. Pointing a build at a
//! different repo is then one constant (or one env var), not a grep hunt
//! across four modules.
//!
//! Runtime overrides, read once per process:
//!   * `OPENCRABS_UPDATE_REPO=owner/name`: repository slug.
//!   * `OPENCRABS_UPDATE_BRANCH`: branch for raw template fetches.
//!
//! A malformed override falls back to the default rather than building a URL
//! that 404s for reasons the user cannot see: an empty string, whitespace, or
//! a value that is not exactly `owner/name` is ignored.

use std::sync::OnceLock;

/// Repository slug (`owner/name`) this build tracks unless overridden.
///
/// This is the fork, deliberately. A build compiled from the fork must update
/// from the fork; otherwise `/evolve` silently replaces it with an upstream
/// binary and the fork's own commits disappear.
pub const DEFAULT_UPDATE_REPO: &str = "adi805/opencrabs-windows";

/// Branch used for raw template fetches unless overridden.
pub const DEFAULT_UPDATE_BRANCH: &str = "main";

/// Env var that overrides [`DEFAULT_UPDATE_REPO`].
pub const REPO_ENV: &str = "OPENCRABS_UPDATE_REPO";

/// Env var that overrides [`DEFAULT_UPDATE_BRANCH`].
pub const BRANCH_ENV: &str = "OPENCRABS_UPDATE_BRANCH";

/// Every URL derived from one repo slug plus one branch.
///
/// Built once and cached: the values are process-stable (they come from the
/// environment, which systemd sets at spawn), and call sites sit inside
/// `tracing` macros where a `&'static str` keeps the field syntax unchanged.
struct Source {
    repo: String,
    branch: String,
    releases_latest_api: String,
    releases_api: String,
    release_download_base: String,
    releases_page: String,
    repo_page: String,
    clone_url: String,
    raw_base: String,
}

fn build(repo: &str, branch: &str) -> Source {
    Source {
        repo: repo.to_string(),
        branch: branch.to_string(),
        releases_latest_api: format!("https://api.github.com/repos/{repo}/releases/latest"),
        releases_api: format!("https://api.github.com/repos/{repo}/releases"),
        release_download_base: format!("https://github.com/{repo}/releases/download"),
        releases_page: format!("https://github.com/{repo}/releases"),
        repo_page: format!("https://github.com/{repo}"),
        clone_url: format!("https://github.com/{repo}.git"),
        raw_base: format!("https://raw.githubusercontent.com/{repo}/{branch}"),
    }
}

fn source() -> &'static Source {
    static SOURCE: OnceLock<Source> = OnceLock::new();
    SOURCE.get_or_init(|| {
        build(
            &resolve_repo(std::env::var(REPO_ENV).ok().as_deref()),
            &resolve_branch(std::env::var(BRANCH_ENV).ok().as_deref()),
        )
    })
}

/// Accept an override only when it looks like `owner/name`; else the default.
fn resolve_repo(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(v) if is_valid_slug(v) => v.to_string(),
        _ => DEFAULT_UPDATE_REPO.to_string(),
    }
}

/// Accept an override only when it is a non-empty, whitespace-free ref.
fn resolve_branch(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(v) if !v.is_empty() && !v.contains(char::is_whitespace) => v.to_string(),
        _ => DEFAULT_UPDATE_BRANCH.to_string(),
    }
}

/// Exactly two non-empty path segments, no whitespace: `owner/name`.
fn is_valid_slug(v: &str) -> bool {
    if v.is_empty() || v.contains(char::is_whitespace) {
        return false;
    }
    let mut parts = v.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty()
    )
}

/// Repository slug (`owner/name`) this build updates from.
pub fn update_repo() -> &'static str {
    &source().repo
}

/// Branch used for raw template fetches.
pub fn update_branch() -> &'static str {
    &source().branch
}

/// `releases/latest` REST endpoint — what `/evolve` polls.
pub fn releases_latest_api() -> &'static str {
    &source().releases_latest_api
}

/// Full releases REST endpoint — what crash-recovery rollback lists.
pub fn releases_api() -> &'static str {
    &source().releases_api
}

/// Base for release asset downloads: `<base>/<tag>/<filename>`.
pub fn release_download_base() -> &'static str {
    &source().release_download_base
}

/// Direct URL for one release asset.
pub fn release_asset_url(tag: &str, filename: &str) -> String {
    release_asset_url_for(release_download_base(), tag, filename)
}

fn release_asset_url_for(download_base: &str, tag: &str, filename: &str) -> String {
    format!("{download_base}/{tag}/{filename}")
}

/// Human-facing releases page, for error hints.
pub fn releases_page() -> &'static str {
    &source().releases_page
}

/// Human-facing repository page.
pub fn repo_page() -> &'static str {
    &source().repo_page
}

/// HTTPS clone URL for the self-update `git clone`.
pub fn clone_url() -> &'static str {
    &source().clone_url
}

/// Raw-content base for template fetches (no trailing slash).
pub fn raw_base() -> &'static str {
    &source().raw_base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_url_is_derived_from_the_one_slug() {
        let s = build("owner/name", "main");
        assert_eq!(s.repo, "owner/name");
        assert_eq!(
            s.releases_latest_api,
            "https://api.github.com/repos/owner/name/releases/latest"
        );
        assert_eq!(
            s.releases_api,
            "https://api.github.com/repos/owner/name/releases"
        );
        assert_eq!(
            s.release_download_base,
            "https://github.com/owner/name/releases/download"
        );
        assert_eq!(s.releases_page, "https://github.com/owner/name/releases");
        assert_eq!(s.repo_page, "https://github.com/owner/name");
        assert_eq!(s.clone_url, "https://github.com/owner/name.git");
        assert_eq!(
            s.raw_base,
            "https://raw.githubusercontent.com/owner/name/main"
        );
    }

    #[test]
    fn the_default_build_never_points_at_upstream() {
        // The regression this module exists to prevent: a fork build that
        // still polls upstream for updates, then overwrites itself with a
        // binary that does not contain the fork's commits.
        let s = build(DEFAULT_UPDATE_REPO, DEFAULT_UPDATE_BRANCH);
        let urls = [
            &s.releases_latest_api,
            &s.releases_api,
            &s.release_download_base,
            &s.releases_page,
            &s.repo_page,
            &s.clone_url,
            &s.raw_base,
        ];
        for url in urls {
            assert!(
                !url.contains("opencrabs/opencrabs"),
                "default build still resolves upstream: {url}"
            );
        }
        assert_eq!(s.repo, "adi805/opencrabs-windows");
    }

    #[test]
    fn asset_url_joins_tag_and_filename_under_the_download_base() {
        assert_eq!(
            release_asset_url_for(
                "https://github.com/owner/name/releases/download",
                "v1.2.3",
                "opencrabs-v1.2.3-linux-amd64.tar.gz"
            ),
            "https://github.com/owner/name/releases/download/v1.2.3/opencrabs-v1.2.3-linux-amd64.tar.gz"
        );
    }

    #[test]
    fn repo_override_must_look_like_owner_name() {
        assert_eq!(resolve_repo(Some("acme/fork")), "acme/fork");
        assert_eq!(resolve_repo(Some("  acme/fork  ")), "acme/fork");
        for rejected in [
            None,
            Some(""),
            Some("   "),
            Some("justname"),
            Some("a/b/c"),
            Some("/name"),
            Some("owner/"),
            Some("own er/name"),
        ] {
            assert_eq!(
                resolve_repo(rejected),
                DEFAULT_UPDATE_REPO,
                "should have fallen back to the default: {rejected:?}"
            );
        }
    }

    #[test]
    fn branch_override_rejects_empty_and_whitespace() {
        assert_eq!(resolve_branch(Some("develop")), "develop");
        assert_eq!(resolve_branch(Some("  main  ")), "main");
        assert_eq!(resolve_branch(Some("release/1.x")), "release/1.x");
        for rejected in [None, Some(""), Some("   "), Some("a b")] {
            assert_eq!(
                resolve_branch(rejected),
                DEFAULT_UPDATE_BRANCH,
                "should have fallen back to the default: {rejected:?}"
            );
        }
    }
}
