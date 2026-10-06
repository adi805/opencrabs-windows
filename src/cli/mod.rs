//! CLI Module
//!
//! Command-line interface for OpenCrabs using Clap v4.

mod args;
pub(crate) mod commands;
pub(crate) mod crash_recovery;
mod cron;
pub(crate) mod daemon_health;
pub(crate) mod doctor_fix;
pub(crate) mod headless_callbacks;
pub(crate) mod migrate;
// Deliberately NOT cfg(windows)-gated. The script builders and the status
// parser are pure functions with no OS dependency, and their unit tests only
// run in CI if the module compiles on the Linux/macOS runners -- the Windows
// job builds but does not run them, so gating the module here silently took
// the whole test module out of the suite. Only `run_script` reaches the
// process layer, and it early-returns off Windows. `dead_code` below is for
// the Linux lint job: the only callers are the cfg(windows) arms in commands.
pub(crate) mod resume_delivery;
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod service_windows;
pub(crate) mod session_notify;
pub(crate) mod session_resolve;
pub(crate) mod session_set_model;
pub(crate) mod tool_setup;
pub(crate) mod ui;

pub use args::*;
