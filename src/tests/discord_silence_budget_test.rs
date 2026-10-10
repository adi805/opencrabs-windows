//! FR-006 (silence warning) and FR-011 (budget clock denominator) for the
//! Discord progress card (#1880).
//!
//! Both features read config at render time, so these tests swap the
//! process-wide in-memory mirror via `Config::set_current` — no file is ever
//! written. `CFG_LOCK` serializes the swap inside this file.
//!
//! Note on shape: `render_content` only routes through `summary_line` when the
//! group has 2+ entries (a lone collapsed tool renders as its own line), so
//! every fixture here carries two tools.

use std::time::{Duration, Instant};

use crate::channels::discord::tool_group::{
    GroupEntry, GroupState, SettledStatus, TurnOutcome, render_content,
};
use crate::config::Config;

static CFG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cfg_with(silence_secs: u64, budget_secs: u64) -> Config {
    let mut cfg: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("embedded config.toml.example must parse");
    cfg.channels.discord.progress.silence_warning_secs = silence_secs;
    cfg.agent.thinking_loop_timeout_secs = budget_secs;
    cfg
}

/// A two-tool group whose last update landed `idle_secs` ago.
fn group(idle_secs: u64, settled: bool) -> GroupState {
    let now = Instant::now();
    let entry = |name: &str| GroupEntry {
        name: name.to_string(),
        context: " (cargo test)".to_string(),
        status: None,
    };
    GroupState {
        entries: vec![entry("bash"), entry("read_file")],
        notes: Vec::new(),
        expanded: false,
        started_at: now
            .checked_sub(Duration::from_secs(idle_secs + 60))
            .expect("started_at within range"),
        live_ctx: None,
        settled: settled
            .then(|| SettledStatus::new(TurnOutcome::Finished, Duration::from_secs(5), None)),
        last_activity_at: now
            .checked_sub(Duration::from_secs(idle_secs))
            .expect("last_activity_at within range"),
    }
}

/// AC-012: a card quiet past the threshold says so, and names the last
/// activity so the line is more than a spinner.
#[test]
fn silence_line_appears_after_the_configured_idle() {
    let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Config::set_current(cfg_with(60, 600));
    let body = render_content(&group(120, false));
    assert!(body.contains("still working"), "body was: {body}");
    assert!(body.contains("read_file"), "names last activity: {body}");
}

/// AC-012 boundary: a fresh card carries no silence line.
#[test]
fn silence_line_absent_while_the_card_is_fresh() {
    let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Config::set_current(cfg_with(60, 600));
    let body = render_content(&group(5, false));
    assert!(!body.contains("still working"), "body was: {body}");
}

/// A settled turn never claims to still be working, however long it sat.
#[test]
fn silence_line_absent_once_the_turn_settled() {
    let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Config::set_current(cfg_with(60, 600));
    let body = render_content(&group(120, true));
    assert!(!body.contains("still working"), "body was: {body}");
}

/// AC-013: `0` disables the line rather than firing instantly.
#[test]
fn silence_threshold_zero_disables_the_line() {
    let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Config::set_current(cfg_with(0, 600));
    let body = render_content(&group(120, false));
    assert!(!body.contains("still working"), "body was: {body}");
}

/// AC-023: the clock carries the budget denominator read from config.
#[test]
fn clock_shows_the_budget_denominator() {
    let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Config::set_current(cfg_with(60, 600));
    let body = render_content(&group(5, false));
    assert!(body.contains("/ 10:00"), "body was: {body}");
}

/// A disabled budget (0) keeps the bare elapsed clock.
#[test]
fn clock_omits_the_denominator_when_the_budget_is_disabled() {
    let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Config::set_current(cfg_with(60, 0));
    let body = render_content(&group(5, false));
    assert!(!body.contains(" / "), "body was: {body}");
}

/// AC-013: the render path reads the threshold from config, never a literal.
#[test]
fn render_path_carries_no_literal_threshold() {
    let src = include_str!("../channels/discord/tool_group.rs");
    assert!(
        src.contains("silence_warning_secs"),
        "the threshold must come from config"
    );
    assert!(
        !src.contains("from_secs(90)"),
        "90 must not be a literal in the render path"
    );
    assert!(
        !src.contains("from_secs(600)"),
        "600 must not be a literal in the render path"
    );
}
