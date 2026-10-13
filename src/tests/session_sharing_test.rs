//! Durable shared/personal session classification (#217).
//!
//! The gate used to live only in a process-local set that starts empty, and
//! `is_session_shared` returned `false` for an absent entry. `false` means
//! "not shared", so an unclassified session read as private and the personal
//! memory surfaces stayed open. Boot recovery dispatches interrupted channel
//! turns straight to the resume path, bypassing ingress, so a resumed guild
//! turn could prepare context before any inbound message re-marked it.
//!
//! These tests pin the three things that close it:
//!
//! 1. `Unknown` is a real state and the gate treats it as shared for personal
//!    content, so absence cannot silently allow the owner's context.
//! 2. The classification is durable, so a later process reads back what
//!    ingress decided instead of starting from nothing.
//! 3. Every channel handler classifies BOTH branches, so no turn-entry path
//!    can leave a session unmarked, and the gates restore before deciding.

use crate::db::Database;
use crate::db::models::Session;
use crate::db::repository::SessionRepository;
use crate::memory::{
    SessionSharing, internal_content_blocked, mark_session_personal, mark_session_shared,
    session_sharing,
};
use crate::tests::source_shape::{flatten, read_in};
use uuid::Uuid;

async fn make_db() -> Database {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db
}

// ── the tri-state contract ──────────────────────────────────────────

#[test]
fn an_unclassified_session_is_unknown_not_private() {
    let fresh = Uuid::new_v4();
    assert_eq!(
        session_sharing(fresh),
        SessionSharing::Unknown,
        "#217: absence of a classification is its own answer"
    );
    assert!(
        internal_content_blocked(fresh),
        "#217: unknown must fail closed for personal content, because the \
         process cannot tell a DM from a group it has not seen yet"
    );
}

#[test]
fn an_explicitly_personal_session_keeps_its_access() {
    let dm = Uuid::new_v4();
    mark_session_personal(dm);
    assert_eq!(session_sharing(dm), SessionSharing::Personal);
    assert!(
        !internal_content_blocked(dm),
        "a known-personal session must keep personal memory: this is the \
         verified-private half of #217"
    );
}

#[test]
fn a_shared_session_is_blocked_and_the_two_marks_do_not_leak() {
    let group = Uuid::new_v4();
    mark_session_shared(group);
    assert!(internal_content_blocked(group), "#1957 default-deny");

    // Re-classifying the same session flips the answer instead of leaving both
    // marks set, so a session that moves between audiences cannot read as both.
    mark_session_personal(group);
    assert_eq!(session_sharing(group), SessionSharing::Personal);
    assert!(!internal_content_blocked(group));

    mark_session_shared(group);
    assert_eq!(session_sharing(group), SessionSharing::Shared);
    assert!(internal_content_blocked(group));
}

#[test]
fn the_durable_spelling_round_trips_and_anything_else_is_unknown() {
    assert_eq!(
        SessionSharing::from_durable(Some("shared")),
        SessionSharing::Shared
    );
    assert_eq!(
        SessionSharing::from_durable(Some("personal")),
        SessionSharing::Personal
    );
    assert_eq!(
        SessionSharing::from_durable(Some(" personal ")),
        SessionSharing::Personal
    );
    // A NULL column and any unrecognised value are Unknown, never a guess.
    for raw in [None, Some(""), Some("group"), Some("dm")] {
        assert_eq!(
            SessionSharing::from_durable(raw),
            SessionSharing::Unknown,
            "raw={raw:?} must not be read as private"
        );
    }
    assert_eq!(SessionSharing::Unknown.as_str(), None);
}

// ── durability ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_persisted_classification_is_readable_by_a_later_process() {
    let db = make_db().await;
    let repo = SessionRepository::new(db.pool().clone());
    let session = Session::new(Some("t".into()), None, None);
    repo.create(&session).await.unwrap();

    // A local session is personal by construction, not by absence.
    assert_eq!(
        repo.sharing_of(session.id).await.unwrap().as_deref(),
        Some("personal"),
        "generic create writes the durable answer, so a TUI session is not unknown"
    );

    repo.set_sharing(session.id, "shared").await.unwrap();
    assert_eq!(
        repo.sharing_of(session.id).await.unwrap().as_deref(),
        Some("shared"),
        "what ingress decides must survive the process that decided it"
    );

    // A row that does not exist has no classification: None, not "private".
    assert_eq!(repo.sharing_of(Uuid::new_v4()).await.unwrap(), None);
}

#[tokio::test]
async fn a_channel_session_starts_personal_and_the_handler_upgrades_it() {
    let db = make_db().await;
    let repo = SessionRepository::new(db.pool().clone());
    let session = Session::new(Some("t".into()), None, None);
    let resolved = repo
        .insert_or_resolve_channel(&session, "discord:123")
        .await
        .unwrap();

    assert_eq!(
        repo.sharing_of(resolved.id).await.unwrap().as_deref(),
        Some("personal"),
        "a channel session is created personal; the handler upgrades a group"
    );
    repo.set_sharing(resolved.id, "shared").await.unwrap();
    assert_eq!(
        repo.sharing_of(resolved.id).await.unwrap().as_deref(),
        Some("shared")
    );
}

// ── the wiring, so a new turn-entry path cannot skip the classification ──

/// Every channel handler must classify BOTH branches. Marking only the group
/// branch is what left a DM indistinguishable from an unclassified session,
/// and the group branch alone cannot heal an existing row after a restart.
#[test]
fn every_channel_handler_classifies_both_branches() {
    const HANDLERS: [(&str, &str); 4] = [
        ("src/channels", "discord/handler.rs"),
        ("src/channels", "telegram/handler.rs"),
        ("src/channels", "slack/handler.rs"),
        ("src/channels", "whatsapp/handler.rs"),
    ];
    for (dir, name) in HANDLERS {
        let src = flatten(read_in(dir, name));
        for needle in [
            "mark_session_shared_durable(",
            "mark_session_personal_durable(",
        ] {
            assert!(
                src.contains(needle),
                "{name}: missing {needle}; a turn-entry path that only marks one \
                 branch leaves the other unclassified, which is the #217 gap"
            );
        }
    }
}

/// Every gate that consults `internal_content_blocked` must read the durable
/// classification first, or a resumed turn falls through to the in-memory set
/// that ingress never filled.
///
/// The gate list is ENUMERATED from the tree, not written by hand. A hand
/// list is how the first version of this test passed while `load_brain_file`
/// stayed ungated: it named two gates and the third one was invisible.
#[test]
fn every_personal_content_gate_restores_before_deciding() {
    use crate::tests::source_shape::sources_in;

    // The definition itself, and the tests that call it directly, are not
    // gates: the first IS the decision, the second has no turn to restore.
    const NOT_GATES: [&str; 2] = ["src/memory/settings.rs", "src/tests/"];

    let mut checked = 0usize;
    // `sources_in` is non-recursive, so each directory that holds a gate is
    // listed explicitly; a gate added under a new directory means adding it
    // here, which is visible in the diff rather than silently skipped.
    const GATE_DIRS: [&str; 4] = [
        "src/brain/tools",
        "src/brain/agent/service",
        "src/channels",
        "src/cli",
    ];
    for dir in GATE_DIRS {
        for (name, body) in sources_in(dir) {
            let path = format!("{dir}/{name}");
            if NOT_GATES.iter().any(|n| path.starts_with(n)) {
                continue;
            }
            if !body.contains("internal_content_blocked(") {
                continue;
            }
            assert!(
                body.contains("restore_sharing("),
                "{path}: consults `internal_content_blocked` without restoring the \
                 durable classification first (#217), so a resumed or cold-start \
                 turn decides on an empty in-memory set"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 3,
        "expected at least three gates (context recall, memory_search, \
         load_brain_file), found {checked}; the sweep may be measuring nothing"
    );
}
