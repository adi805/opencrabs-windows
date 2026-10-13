//! Callback authorization on Telegram (#211).
//!
//! The callback dispatch branched on the payload without asking whether the
//! tapper was allowed to act at all, and three branches mutated global or
//! destructive state with no owner check while the approval and plan keyboards
//! already re-checked the tapper. A member who can see a live shared inline
//! keyboard could therefore reach mutations outside the owner command policy.
//!
//! These tests pin three things:
//!
//! 1. the admission decision table, including the unconfigured-channel case
//!    (deny, not "everyone is an owner");
//! 2. the session-selection rule, so a guest cannot pull another chat's
//!    session into theirs;
//! 3. in source form, that the admission gate runs before any branch and that
//!    every global/destructive branch consults privilege - enumerated from a
//!    list of prefixes, so a new privileged branch without a gate fails here.

use crate::channels::telegram::agent::{
    CallbackAdmission, admit_callback, chat_key_belongs_to, session_selectable_by_guest_with,
};
use crate::config::types::TelegramConfig;
use crate::db::Database;
use crate::db::models::Session;
use crate::db::repository::SessionRepository;
use crate::tests::source_shape::read_in;
use uuid::Uuid;

const AGENT: &str = "src/channels/telegram/agent.rs";

fn cfg(allowed: &[&str], owner: &[&str]) -> TelegramConfig {
    TelegramConfig {
        allowed_users: allowed.iter().map(|s| s.to_string()).collect(),
        bot_owner: owner.iter().map(|s| s.to_string()).collect(),
        ..TelegramConfig::default()
    }
}

// ── admission ───────────────────────────────────────────────────────

#[test]
fn an_unconfigured_channel_admits_nobody() {
    let c = TelegramConfig::default();
    assert_eq!(
        admit_callback(&c, "111", "111", true),
        None,
        "#211: with no allow list and no owner the channel is unconfigured, and \
         'nobody is an owner' is the documented behaviour; treating it as open \
         would hand every tapper the privileged branches"
    );
}

#[test]
fn the_owner_and_global_admins_are_privileged() {
    let c = cfg(&["222"], &["111"]);
    assert_eq!(
        admit_callback(&c, "111", "-100", false),
        Some(CallbackAdmission::Privileged),
        "the configured owner is privileged anywhere, groups included"
    );
    assert_eq!(
        admit_callback(&c, "222", "999", true),
        Some(CallbackAdmission::Privileged),
        "a global admin (allowed_users) is privileged in a DM too"
    );
    assert!(CallbackAdmission::Privileged.is_privileged());
}

#[test]
fn a_group_member_is_admitted_but_not_privileged() {
    let mut c = cfg(&["222"], &["111"]);
    c.groups.insert(
        "-100".to_string(),
        crate::config::types::TelegramGroupConfig {
            allowed_users: vec!["333".to_string()],
            ..Default::default()
        },
    );
    assert_eq!(
        admit_callback(&c, "333", "-100", false),
        Some(CallbackAdmission::Guest),
        "a rostered group member may tap, but only as a guest"
    );
    assert!(!CallbackAdmission::Guest.is_privileged());

    // The same member in a DM is not admitted: the group roster is scoped to
    // that group, which is the escape-the-group-oversight bypass #1051 closed.
    assert_eq!(
        admit_callback(&c, "333", "333", true),
        None,
        "a group-only roster must not admit the caller in a DM"
    );

    // An unrelated chat has no roster entry, so the caller is refused.
    assert_eq!(admit_callback(&c, "333", "-200", false), None);
}

// ── session selection ───────────────────────────────────────────────

#[test]
fn a_chat_key_belongs_to_its_own_chat_in_both_shapes() {
    assert!(chat_key_belongs_to("[chat:-1001234]", -1001234));
    assert!(
        chat_key_belongs_to("[chat:-1001234:topic:55]", -1001234),
        "a topic-scoped binding still belongs to the chat"
    );
    assert!(!chat_key_belongs_to("[chat:-1001234]", -999));
    assert!(!chat_key_belongs_to("guild-1", -1001234));
    assert!(!chat_key_belongs_to("", -1001234));
}

#[tokio::test]
async fn a_guest_may_only_select_a_session_bound_to_their_chat() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = SessionRepository::new(db.pool().clone());

    let mine = Session::new(Some("t".into()), None, None);
    let mine = repo
        .insert_or_resolve_channel(&mine, "[chat:-100]")
        .await
        .unwrap();
    let theirs = Session::new(Some("t".into()), None, None);
    let theirs = repo
        .insert_or_resolve_channel(&theirs, "[chat:-200]")
        .await
        .unwrap();

    assert!(
        session_selectable_by_guest_with(&repo, mine.id, -100).await,
        "#211: a guest may select the session bound to their own chat"
    );
    assert!(
        !session_selectable_by_guest_with(&repo, theirs.id, -100).await,
        "#211: a guest must NOT be able to pull another chat's session in"
    );
    assert!(
        !session_selectable_by_guest_with(&repo, Uuid::new_v4(), -100).await,
        "an unknown session id must be refused, not assumed fine"
    );
}

// ── the wiring ──────────────────────────────────────────────────────

/// Branches that change global or destructive state. Each must consult
/// privilege; the list is the contract, so adding a branch means adding it here
/// *and* gating it.
const PRIVILEGED_PREFIXES: &[&str] = &["allm:", "prof:confirm_migrate:", "prof:confirm_del:"];

#[test]
fn every_privileged_branch_consults_privilege() {
    let src = read_in("src/channels/telegram", "agent.rs");
    let lines: Vec<&str> = src.lines().collect();

    for prefix in PRIVILEGED_PREFIXES {
        let needle = format!("strip_prefix(\"{prefix}\")");
        let idx = lines
            .iter()
            .position(|l| l.contains(&needle))
            .unwrap_or_else(|| panic!("{prefix} branch not found; did it get renamed?"));
        // The gate must be the first thing the branch does, so a short window
        // is enough and an unrelated check elsewhere cannot satisfy it.
        let window = lines[idx..(idx + 8).min(lines.len())].join("\n");
        assert!(
            window.contains("is_privileged()"),
            "{AGENT}:{prefix} has no privilege gate within 8 lines of its branch \
             head, so an admitted guest could reach a global/destructive action \
             through the keyboard (#211)"
        );
    }
}

/// The admission gate must run before any branch. The handler's first statement
/// inside `if let Some(data)` is where it belongs: putting it later would let an
/// earlier branch run unauthenticated.
#[test]
fn admission_runs_before_the_first_branch() {
    let src = read_in("src/channels/telegram", "agent.rs");
    let lines: Vec<&str> = src.lines().collect();

    let info = lines
        .iter()
        .position(|l| l.contains("Telegram callback query received"))
        .expect("the callback handler logs the payload");
    let admit = lines
        .iter()
        .position(|l| l.contains("let cb_admission = {"))
        .expect("the handler admits the tapper");
    let first_branch = lines
        .iter()
        .position(|l| l.contains("strip_prefix(\"allm:\")"))
        .expect("the first payload branch");

    assert!(
        admit > info,
        "admission must come after the payload is known"
    );
    assert!(
        admit < first_branch,
        "{AGENT}: the admission gate sits after the first payload branch, so \
         that branch runs for a caller who was never admitted (#211)"
    );
    assert!(
        lines[admit..first_branch]
            .iter()
            .any(|l| l.contains("refuse_callback")),
        "the admission gate must refuse the caller, not merely compute a verdict"
    );
}

/// Session selection must be authorized against the caller, not just parsed.
#[test]
fn session_selection_is_authorized_before_rebinding() {
    let src = read_in("src/channels/telegram", "agent.rs");
    let lines: Vec<&str> = src.lines().collect();
    let idx = lines
        .iter()
        .position(|l| l.contains("session_selectable_by_guest("))
        .expect("the session branch authorizes the selection");
    let rebind = lines
        .iter()
        .position(|l| l.contains("register_session_chat("))
        .expect("the session branch rebinds the chat");
    assert!(
        idx < rebind,
        "{AGENT}: the session binding is written before the selection is \
         authorized (#211)"
    );
}
