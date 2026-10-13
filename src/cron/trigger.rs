use std::time::Duration;
use tokio::process::Command;

/// Condition required for a trigger to fire.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TriggerCondition {
    /// Fires if stdout or stderr contains any non-whitespace output (default).
    #[default]
    NonEmpty,
    /// Fires if the command exits with a non-zero exit code.
    ExitNonZero,
    /// Fires if the command exits with a zero exit code (success).
    ExitZero,
    /// Fires if the combined stdout/stderr output matches the provided regular expression.
    Regex(String),
    /// Fires whenever the trigger command finishes executing, regardless of output or exit status.
    Always,
}

impl TriggerCondition {
    pub fn parse(s: Option<&str>) -> Self {
        let raw = match s.map(str::trim) {
            Some(v) if !v.is_empty() => v,
            _ => return Self::NonEmpty,
        };

        if let Some(pat) = raw
            .strip_prefix("regex:")
            .or_else(|| raw.strip_prefix("re:"))
        {
            return Self::Regex(pat.trim().to_string());
        }

        match raw.to_lowercase().as_str() {
            "exit_non_zero" | "exitnonzero" | "non_zero" => Self::ExitNonZero,
            "exit_zero" | "exitzero" | "zero" | "exit_0" => Self::ExitZero,
            "always" => Self::Always,
            _ => Self::NonEmpty,
        }
    }

    pub fn should_fire(&self, result: &TriggerResult) -> bool {
        match self {
            Self::NonEmpty => !result.combined_output().trim().is_empty(),
            Self::ExitNonZero => result.exit_code != 0,
            Self::ExitZero => result.exit_code == 0,
            Self::Regex(pat) => match regex::Regex::new(pat) {
                Ok(re) => re.is_match(&result.combined_output()),
                Err(e) => {
                    tracing::warn!("Invalid regex trigger condition '{pat}': {e}");
                    false
                }
            },
            Self::Always => true,
        }
    }
}

/// Result of executing a pre-flight trigger command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl TriggerResult {
    pub fn combined_output(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

/// Runner for pre-flight trigger shell commands.
#[derive(Debug, Clone)]
pub struct TriggerRunner {
    timeout: Duration,
}

impl Default for TriggerRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl TriggerRunner {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// Run the given shell command under the platform shell with the
    /// configured timeout. This is the repo's own shell contract
    /// ([`crate::utils::shell::shell_pair`] + `PushShellCommand`), not a
    /// hand-rolled `.arg()`: cmd.exe does not parse MSVCRT-escaped argv, so
    /// a quoted payload like `dir "C:\Program Files"` must ride through as
    /// raw text (raw_arg), while unix keeps plain .arg semantics.
    /// Hardcoding `/bin/sh -c` here compiles green on the windows build but
    /// fails at runtime on every trigger -- exactly the "starts vs ships"
    /// class the footgun gate exists to catch. The POSIX-vs-cmd dialect
    /// gap for trigger snippets is tracked as #7.
    ///
    /// The command is run through the shared command blocklist first, so this
    /// path cannot do what `bash` refuses (OC-A2).
    pub async fn run(&self, cmd: &str) -> Result<TriggerResult, String> {
        // The bash hard blocklist is the immovable floor for every execution
        // path (OC-06 / OC-A2). A `trigger_cmd` is authored through
        // `cron_manage`, and this path ran it through the platform shell with
        // no floor at all, so `trigger_cmd = "rm -rf ~"` executed on schedule
        // while `bash` refused the identical string. Checked here, at run time,
        // rather than only at create time, so a job stored before this gate
        // existed cannot fire unguarded either.
        //
        // Refused as an Err, not a non-firing result: the scheduler treats a
        // trigger error as "do not run this job" and records the refusal, which
        // is the fail-closed outcome. A silent skip would report the job as
        // having decided not to fire.
        if let Some(reason) = crate::brain::tools::bash::assert_command_allowed(cmd) {
            return Err(format!(
                "Trigger command refused by the command blocklist: {reason} (OC-A2)."
            ));
        }

        use crate::utils::shell::PushShellCommand;
        let (shell, shell_arg) = crate::utils::shell::shell_pair();
        let child = Command::new(shell)
            .push_shell_command(shell_arg, cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // On timeout the future below is dropped, and a tokio Child does
            // not kill its process on drop. Without this a trigger that hangs
            // leaves the shell running unsupervised, once per schedule tick,
            // for as long as the daemon lives.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to spawn trigger process: {e}"))?;

        let output_res = tokio::time::timeout(self.timeout, child.wait_with_output()).await;

        match output_res {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                Ok(TriggerResult {
                    exit_code,
                    stdout,
                    stderr,
                })
            }
            Ok(Err(e)) => Err(format!("Trigger process execution error: {e}")),
            Err(_) => Err(format!(
                "Trigger command timed out after {}s",
                self.timeout.as_secs()
            )),
        }
    }
}
