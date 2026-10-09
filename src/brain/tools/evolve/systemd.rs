//! systemd units for the evolve restart path.
//!
//! Split out of `evolve.rs` (#963 review): these are self-contained command
//! builders with no dependency on the tool or its strategies, and pinning
//! their arg lists in tests is the whole point — silent drift in any flag
//! re-introduces the "Evolved! but the daemon never restarted" symptom.
//!
//! Also holds the pre-flight-and-arm helpers ([`select_restart_targets`],
//! [`sweep_stale_evolve_units`]) since they are the only consumers of the
//! builders and belong with them rather than inside one strategy file. The
//! Homebrew branch (#1779) reaches them too, so the bus rules cannot drift
//! between the two evolve paths.

/// Service-unit glob used to *list* the restart candidates. Matches every
/// profile (default, ops, staging, ...) sharing the same binary. It is a
/// listing pattern only: the scheduled `systemctl restart` is handed explicit
/// unit names (see [`build_systemd_restart_command`]), never this glob,
/// because the glob also matches the transient evolve unit running it.
pub(crate) const SYSTEMD_UNIT_PATTERN: &str = "opencrabs*.service";

/// Prefix of the transient units `build_systemd_restart_command` schedules,
/// one per evolve attempt (`opencrabs-evolve-<pid>`).
///
/// Used two ways: to name the transient unit, and to exclude any unit carrying
/// this prefix from its own restart set. A unit whose ExecStart is
/// `systemctl restart opencrabs*.service` matches *itself* through the glob,
/// so systemctl SIGTERMs the transient unit before it ever reaches the daemon
/// (the daemon then never restarts, and systemd gives up with
/// `start-limit-hit`). Naming targets explicitly, and dropping this prefix
/// from the candidate list, removes that self-match.
pub(crate) const EVOLVE_UNIT_PREFIX: &str = "opencrabs-evolve-";

/// Build the `systemd-run` command that schedules a delayed restart of
/// `units`, an explicit list of unit names resolved by
/// [`select_restart_targets`]. Extracted so the arg list can be pinned by
/// tests — silent drift in any of these flags would re-introduce the
/// "Evolved! but daemon didn't restart" symptom that issue #136 reported.
///
/// Set `user` to `true` to target user-level units (`systemctl --user`),
/// e.g. when OpenCrabs was installed via `install_systemd_service()` which
/// writes to `~/.config/systemd/user/`.
///
/// The `pid` argument is used to derive a unique transient unit
/// name (`opencrabs-evolve-<pid>`) so concurrent evolve calls don't
/// collide on the transient unit registry.
///
/// Precondition: `units` is non-empty. Callers pass the output of
/// [`select_restart_targets`], which returns `None` rather than an empty list
/// so a bare `systemctl restart` (no operand, an error) is never scheduled.
pub(crate) fn build_systemd_restart_command(
    pid: u32,
    user: bool,
    units: &[String],
) -> std::process::Command {
    let unit_name = format!("{EVOLVE_UNIT_PREFIX}{pid}");
    let mut cmd = std::process::Command::new("systemd-run");
    let mut args = vec![];
    // --user on systemd-run itself is required when the daemon runs as a
    // user service: without it, systemd-run tries to talk to the system
    // bus and either fails (no permission from within a --user service)
    // or creates the transient timer in the system instance, where the
    // spawned systemctl won't have DBUS_SESSION_BUS_ADDRESS available.
    if user {
        args.push("--user".to_string());
    }
    args.push("--on-active=3".to_string());
    args.push(format!("--unit={unit_name}"));
    args.push("systemctl".to_string());
    // --user on systemctl is needed to target the user service manager.
    if user {
        args.push("--user".to_string());
    }
    args.push("restart".to_string());
    // Explicit names, never a glob: see EVOLVE_UNIT_PREFIX for the self-match
    // this avoids.
    for unit in units {
        args.push(unit.clone());
    }
    cmd.args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Glob matching every transient evolve restart unit we have ever
/// scheduled. `build_systemd_restart_command` embeds the scheduling
/// process PID in each unit name (`opencrabs-evolve-<pid>`), so over a
/// host's lifetime many such units are created — one per evolve / auto
/// update attempt.
pub(crate) const EVOLVE_UNIT_GLOB: &str = "opencrabs-evolve-*.service";

/// Build the command that garbage-collects spent evolve restart units.
///
/// We cannot pass `--collect` to `systemd-run` (it's unsupported on
/// systemd < v240, RHEL 7 / CentOS 7), so a finished transient
/// `opencrabs-evolve-<pid>` unit lingers in systemd's registry — and a
/// restart that *fails* (e.g. it lost the channel-token race against a
/// running TUI) lingers in the **failed** state, accumulating forever.
/// `systemctl reset-failed <glob>` clears those spent units and is
/// available on every systemd version we target, so we call it right
/// before scheduling a fresh restart. Best-effort: its failure must
/// never block the evolve.
pub(crate) fn build_systemd_cleanup_command(user: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.arg("reset-failed")
        .arg(EVOLVE_UNIT_GLOB)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// List the unit names matching `pattern`, at either system or user level.
///
/// Set `user` to `true` to query user-level units (`systemctl --user`).
///
/// Returns `Some(names)` on a successful query (the list may be empty), or
/// `None` if `systemctl` failed to spawn / returned a non-zero exit status (a
/// permissions issue or non-systemd host). `None` is a "don't know" signal, not
/// "zero units": on a healthy host `systemctl list-units` exits 0 with empty
/// output when nothing matches, which is `Some(vec![])`.
///
/// `--no-legend --no-pager --plain` keep stdout machine-parseable: one line
/// per matched unit, whose first whitespace-delimited field is the unit name
/// (e.g. `opencrabs.service loaded active running OpenCrabs Daemon [default]`).
pub(crate) fn list_matching_systemd_units(pattern: &str, user: bool) -> Option<Vec<String>> {
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(["list-units", "--no-legend", "--no-pager", "--plain"]);
    if user {
        cmd.arg("--user");
    }
    cmd.arg(pattern);
    cmd.stderr(std::process::Stdio::null());
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(
        stdout
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Drop any unit whose name carries [`EVOLVE_UNIT_PREFIX`] from a candidate
/// list.
///
/// This is the exclusion that fixes the self-match: the transient unit running
/// the restart must never be in its own restart set. Pure and total, so it is
/// unit-tested with synthetic input (the end-to-end systemd behaviour is not
/// portable to the macOS / Windows CI runners).
pub(crate) fn filter_evolve_units(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| !name.starts_with(EVOLVE_UNIT_PREFIX))
        .collect()
}

/// Resolve which unit bus to restart and the explicit units to name, or `None`
/// when nothing is restartable.
///
/// `Some((user, units))` = restart `units`; `user` selects the bus (`false` =
/// system, `true` = user). `units` is always non-empty. The user-bus fallback
/// is the substance of #162: OpenCrabs installed as a user service
/// (`install_systemd_service()` writes to `~/.config/systemd/user/`) has zero
/// system-level units, and checking only the system bus is what produced the
/// "Evolved! but the daemon never restarted" line of reports (#136).
///
/// `None` means a confirmed absence of restartable units on both buses: the
/// caller reports [`crate::brain::tools::evolve::restart_status::RestartStatus::NoUnitsMatched`]
/// with the honest manual-restart message instead of scheduling a no-op. A
/// `None` *query* from [`list_matching_systemd_units`] (systemctl could not
/// run) is logged and the other bus is still tried; it is not treated as zero.
///
/// Both evolve branches (binary download, Homebrew) pre-flight through here so
/// the bus rules cannot drift apart.
pub(crate) fn select_restart_targets(sid: uuid::Uuid) -> Option<(bool, Vec<String>)> {
    match list_matching_systemd_units(SYSTEMD_UNIT_PATTERN, false) {
        Some(names) => {
            let targets = filter_evolve_units(names);
            if !targets.is_empty() {
                tracing::info!(
                    target: "evolve",
                    pattern = SYSTEMD_UNIT_PATTERN,
                    matched_units = targets.len(),
                    units = ?targets,
                    use_user_units = false,
                    session_id = %sid,
                    "evolve: pre-flight resolved {} system-level restart \
                     target(s), scheduling restart (+3s)",
                    targets.len()
                );
                return Some((false, targets));
            }
        }
        None => {
            tracing::warn!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                session_id = %sid,
                "evolve: could not list system-level units (systemctl spawn failed), \
                 trying the user bus"
            );
        }
    }

    match list_matching_systemd_units(SYSTEMD_UNIT_PATTERN, true) {
        Some(names) => {
            let targets = filter_evolve_units(names);
            if !targets.is_empty() {
                tracing::info!(
                    target: "evolve",
                    pattern = SYSTEMD_UNIT_PATTERN,
                    user_units = targets.len(),
                    units = ?targets,
                    session_id = %sid,
                    "evolve: no system-level targets, using {} user-level \
                     unit(s), scheduling restart with --user",
                    targets.len()
                );
                return Some((true, targets));
            }
        }
        None => {
            tracing::warn!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                session_id = %sid,
                "evolve: could not list user-level units (systemctl spawn failed)"
            );
        }
    }

    tracing::warn!(
        target: "evolve",
        pattern = SYSTEMD_UNIT_PATTERN,
        session_id = %sid,
        "evolve: no restartable systemd units matched the pattern (checked system \
         and user level), skipping scheduled restart"
    );
    None
}

/// Sweep spent transient evolve units before scheduling a fresh one.
///
/// Best-effort and deliberately silent about failure: a `reset-failed` that
/// cannot run must never block the restart that matters. Without it, failed
/// transient units accumulate without bound across evolves.
pub(crate) fn sweep_stale_evolve_units(use_user_units: bool, sid: uuid::Uuid) {
    match build_systemd_cleanup_command(use_user_units).status() {
        Ok(status) => tracing::debug!(
            target: "evolve",
            glob = EVOLVE_UNIT_GLOB,
            success = status.success(),
            session_id = %sid,
            "evolve: reset-failed swept stale evolve units"
        ),
        Err(e) => tracing::warn!(
            target: "evolve",
            glob = EVOLVE_UNIT_GLOB,
            error = %e,
            session_id = %sid,
            "evolve: could not sweep stale evolve units, the restart still proceeds"
        ),
    }
}
