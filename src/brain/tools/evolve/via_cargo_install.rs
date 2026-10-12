//! The `cargo install <package> --force` upgrade strategy, for installs
//! that came from crates.io rather than a release binary.

use super::super::error::{Result, ToolError};
use super::super::r#trait::ToolResult;
use super::EvolveTool;
use crate::brain::agent::ProgressEvent;
use crate::utils::update_source::DEFAULT_UPDATE_PACKAGE;

impl EvolveTool {
    /// Update via `cargo install <package> --version <latest> --force`.
    pub(super) async fn evolve_via_cargo_install(
        &self,
        sid: uuid::Uuid,
        current_version: &str,
        latest_version: &str,
    ) -> Result<ToolResult> {
        if let Some(ref cb) = self.progress {
            cb(
                sid,
                ProgressEvent::IntermediateText {
                    text: format!(
                        "Updating via cargo install (v{} -> v{})...",
                        current_version, latest_version
                    ),
                    reasoning: None,
                },
            );
        }

        tracing::info!(
            target: "evolve",
            current_version,
            latest_version,
            package = DEFAULT_UPDATE_PACKAGE,
            session_id = %sid,
            "evolve: running cargo install for the distribution package"
        );
        let output = tokio::process::Command::new("cargo")
            .args([
                "install",
                DEFAULT_UPDATE_PACKAGE,
                "--version",
                latest_version,
                "--force",
            ])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| {
                tracing::warn!(
                    target: "evolve",
                    error = %e,
                    session_id = %sid,
                    "evolve: failed to spawn `cargo`: is the Rust toolchain installed?"
                );
                ToolError::Execution(format!("Failed to spawn cargo: {e}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr_excerpt: String = stderr.chars().take(500).collect();
            tracing::warn!(
                target: "evolve",
                exit_status = %output.status,
                stderr_excerpt = %stderr_excerpt,
                session_id = %sid,
                "evolve: cargo install failed"
            );
            return Ok(ToolResult::error(format!(
                "cargo install {} failed: {stderr_excerpt}",
                DEFAULT_UPDATE_PACKAGE
            )));
        }

        // A zero exit status only proves that *some* package installed. The
        // fork publishes under a different package name than upstream, so a
        // silent fall back to the upstream crate would otherwise be reported
        // as a successful update (#214). Verify the binary cargo just placed
        // reports the version we asked for, and refuse to signal a restart
        // when it does not.
        match installed_binary_version().await {
            Some(installed) if installed == latest_version => {}
            Some(installed) => {
                return Ok(ToolResult::error(format!(
                    "cargo install reported success but the installed binary reports \
                     v{installed}, not v{latest_version}. Refusing to signal a restart."
                )));
            }
            None => {
                return Ok(ToolResult::error(
                    "cargo install reported success but the installed version could not be \
                     read. Refusing to signal a restart."
                        .to_string(),
                ));
            }
        }

        // Signal restart
        if let Some(ref cb) = self.progress {
            cb(
                sid,
                ProgressEvent::RestartReady {
                    status: format!(
                        "Evolved via cargo install: v{} -> v{}.",
                        current_version, latest_version
                    ),
                    // evolve replaced the running exe in place; the handler
                    // resolves it via current_exe().
                    binary_path: None,
                },
            );
        }

        Ok(ToolResult::success(format!(
            "Evolved from v{} to v{} via cargo install.",
            current_version, latest_version
        )))
    }
}

/// Read the version reported by the binary `cargo install` just placed.
///
/// Returns `None` when the binary is absent or its output cannot be parsed.
/// The caller treats that as a failed verification rather than a success, so
/// an unreadable version can never be reported as an update.
async fn installed_binary_version() -> Option<String> {
    let dir = crate::utils::install::cargo_bin_dir()?;
    let exe = dir.join(crate::utils::install::binary_name());
    let out = tokio::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `opencrabs 0.5.10` -> `0.5.10`
    text.split_whitespace().nth(1).map(str::to_string)
}
