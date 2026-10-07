//! Shared-session memory gate on the internal surfaces (#1957).
//!
//! The system prompt has always said MEMORY.md is main-session-only
//! ("load/write only in the MAIN session, never in shared/group chats"),
//! but until #1957 only the `external` scope enforced it. These tests pin
//! the enforcement pieces: the default-deny config field, the personal vs
//! generic brain-file classifier, and the single session-scoped decision
//! every surface consults.

use crate::config::types::MemoryConfig;
use crate::memory::{internal_content_blocked, is_personal_brain_file, mark_session_shared};
use uuid::Uuid;

#[test]
fn internal_gate_is_default_deny_and_opt_in_parses() {
    let cfg = MemoryConfig::default();
    assert!(
        !cfg.internal_allowed_in_shared,
        "#1957: internal content must default to blocked in shared sessions"
    );
    let cfg = toml::from_str::<MemoryConfig>("internal_allowed_in_shared = true")
        .expect("valid [memory] TOML");
    assert!(cfg.internal_allowed_in_shared);
}

#[test]
fn personal_files_are_blocked_and_generic_files_are_not() {
    // The generic files ship identical for everyone (see AGENTS.md brain
    // ownership map), so they stay loadable in shared sessions.
    for safe in [
        "SOUL.md",
        "soul.md",
        "  AGENTS.MD ", // whitespace and casing must not smuggle a file past
        "TOOLS.md",
        "CODE.md",
        "SECURITY.md",
        "BOOT.md",
    ] {
        assert!(
            !is_personal_brain_file(safe),
            "generic brain file {safe} must not be classified personal"
        );
    }

    // Everything else in the home dir is owner-accumulated: the curated
    // long-term memory, the user profile, heartbeat, daily notes, and any
    // custom file the owner wrote.
    for personal in [
        "MEMORY.md",
        "USER.md",
        "HEARTBEAT.md",
        "memory/2026-10-07.md",
        "VOICE.md",
        "DECISION-LOG.md",
        "eval_outliers.md",
        "IDENTITY.md",
    ] {
        assert!(
            is_personal_brain_file(personal),
            "personal-context file {personal} must be classified personal"
        );
    }
}

#[test]
fn blocked_only_for_shared_sessions() {
    // Fresh sessions are not shared: internal surfaces stay open, matching
    // the documented main-session default.
    let main_session = Uuid::new_v4();
    assert!(
        !internal_content_blocked(main_session),
        "a non-shared session must never be blocked"
    );

    // A marked-shared session is blocked under the default config (the opt
    // in lives in config.toml; tests must not write the live home).
    let group_session = Uuid::new_v4();
    mark_session_shared(group_session);
    assert!(
        internal_content_blocked(group_session),
        "#1957: shared/group sessions must get the internal gate too"
    );

    // Marking one session never taints another (unique ids per session).
    let other_session = Uuid::new_v4();
    assert!(
        !internal_content_blocked(other_session),
        "the gate is per session, not process-wide"
    );
}
