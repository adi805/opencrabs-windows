//! Discord Integration
//!
//! Runs a Discord bot alongside the TUI, forwarding messages from
//! allowlisted users to the AgentService and replying with responses.
//!
//! Layout: [`state`] holds the shared `DiscordState` struct and its
//! constructor; each concern has its own impl module beside it
//! (`approval`, `cancel`, `connection`, `pending_interactions`,
//! `sessions`, and the tool-group methods in `tool_group`); `agent` runs
//! the gateway, `handler` routes inbound messages, `interactions` /
//! `reactions` / `suggest_options` / `typing` handle their UI surfaces,
//! `presence` publishes the bot's own activity (FR-003),
//! `member_events` greets joining members and reports a missing privileged
//! intent (FR-004 / NFR-001),
//! `poll` validates the native-poll spec (#1848), `embed` validates the
//! multi-embed report layout (C2), `guard` holds the outbound size/count
//! ceilings (C3), `flags` maps the silent delivery flag, and
//! `resume` re-delivers
//! background results. This file is declarations
//! only — no function definitions live here (CONTRIBUTING.md).

mod agent;
mod approval;
mod cancel;
pub(crate) mod commands;
mod connection;
pub(crate) mod embed;
pub(crate) mod flags;
pub(crate) mod governor;
pub(crate) mod guard;
pub(crate) mod handler;
pub(crate) mod interactions;
pub(crate) mod long_answer;
pub(crate) mod member_events;
mod pending_interactions;
pub(crate) mod plan_card;
pub(crate) mod poll;
pub(crate) mod presence;
pub(crate) mod reactions;
pub(crate) mod resume;
mod sessions;
mod state;
pub(crate) mod suggest_options;
pub(crate) mod table_convert;
pub(crate) mod tool_group;
pub(crate) mod trace_answer;
pub(crate) mod typing;
pub(crate) mod writes;

pub use agent::DiscordAgent;
pub use state::DiscordState;
