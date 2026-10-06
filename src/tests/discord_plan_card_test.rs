//! Tests for the Discord plan card (FR-008, #1880): the session plan board
//! rendered as ONE edited message carrying Approve/Discard buttons.
//!
//! The owner's original complaint was that Telegram has a plan board and
//! Discord had none (`grep -rln plan src/channels/discord/` was empty). The
//! renderer and the keyboard are pure functions, so they are pinned directly.
//! The wiring — owner gate, callback routing, refresh after a turn — is
//! structural, so it is pinned by scanning the source, the same way
//! `discord_fr004_one_message_test` pins AC-008.

use std::path::Path;

use serenity::builder::CreateActionRow;

use crate::channels::discord::plan_card::{
    PLAN_APPROVE, PLAN_DISCARD, PlanKb, is_plan_callback, render_components, render_plan_card,
};
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use uuid::Uuid;

fn plan_with(status: PlanStatus, tasks: Vec<(&str, TaskStatus, usize)>) -> PlanDocument {
    let mut p = PlanDocument::new(Uuid::new_v4(), "Discord UX parity".to_string());
    p.status = status;
    for (i, (title, st, ac)) in tasks.into_iter().enumerate() {
        let mut t = PlanTask::new(i + 1, title.to_string(), String::new(), TaskType::Edit);
        t.status = st;
        t.acceptance_criteria = (0..ac).map(|j| format!("AC-{j}")).collect();
        p.add_task(t);
    }
    p
}

/// Flattened source: all whitespace removed, so a signature or a call split
/// across lines still matches a single-line needle.
fn flattened(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

fn button_count(row: &CreateActionRow) -> usize {
    match row {
        CreateActionRow::Buttons(buttons) => buttons.len(),
        _ => panic!("the plan card must carry a button row"),
    }
}

/// The card has to read like a checklist: one row per task, the task's own
/// status mark, and its acceptance-criteria count when it has one.
#[test]
fn editing_plan_renders_the_checklist_with_marks_and_criteria_counts() {
    let plan = plan_with(
        PlanStatus::Editing,
        vec![
            ("port the write governor", TaskStatus::Completed, 2),
            ("honest settle", TaskStatus::Pending, 0),
        ],
    );
    let body = render_plan_card(&plan);
    assert!(body.contains("📋 **Discord UX parity**"), "title: {body}");
    assert!(
        body.contains("☑ **1. port the write governor**"),
        "completed row: {body}"
    );
    assert!(
        body.contains("_(2 acceptance criteria)_"),
        "criteria count: {body}"
    );
    assert!(
        body.contains("☐ **2. honest settle**"),
        "pending row: {body}"
    );
    // A task with no criteria must not render a "(0 acceptance criteria)"
    // stub: it says nothing and costs a row's worth of noise.
    assert!(
        !body.contains("(0 acceptance criteria)"),
        "empty criteria count leaked into the card: {body}"
    );
}

/// Editing means the plan is waiting on the user, so both actions apply.
#[test]
fn editing_plan_offers_approve_and_discard() {
    let plan = plan_with(
        PlanStatus::Editing,
        vec![("design it", TaskStatus::Pending, 0)],
    );
    assert_eq!(PlanKb::of(&plan), PlanKb::ApproveDiscard);
    let rows = render_components(PlanKb::of(&plan));
    assert_eq!(rows.len(), 1, "exactly one action row");
    assert_eq!(button_count(&rows[0]), 2, "Approve + Discard");
}

/// Active means the checklist is executing, so only Discard applies. Offering
/// Approve on a live plan would re-seed a turn that is already running.
#[test]
fn active_plan_offers_discard_only() {
    let plan = plan_with(
        PlanStatus::Active,
        vec![("run it", TaskStatus::InProgress, 0)],
    );
    assert_eq!(PlanKb::of(&plan), PlanKb::DiscardOnly);
    let rows = render_components(PlanKb::of(&plan));
    assert_eq!(rows.len(), 1, "exactly one action row");
    assert_eq!(button_count(&rows[0]), 1, "Discard only");
}

/// The `plan:` prefix is load-bearing: the tool-approval family is `approve:`
/// / `always:` / `yolo:` / `deny:`, and a plan button mistaken for a
/// permission prompt would let a checklist tap approve a tool call.
#[test]
fn plan_callbacks_never_collide_with_tool_approval() {
    assert!(is_plan_callback(PLAN_APPROVE));
    assert!(is_plan_callback(PLAN_DISCARD));
    for other in [
        "approve:1",
        "always:1",
        "yolo:1",
        "deny:1",
        "toolgroup:5",
        "plan:ok!",
        "plan:",
    ] {
        assert!(
            !is_plan_callback(other),
            "{other} must not route to the plan card"
        );
    }
}

/// A plan with nothing renderable yields an empty body, which is the signal
/// `refresh_plan_card` reads to DELETE a stale card rather than post a blank
/// one. Pinned because "render nothing" and "render an empty message" are one
/// character apart in behaviour and very different on screen.
#[test]
fn a_plan_with_nothing_renderable_yields_no_card() {
    let plan = PlanDocument::new(Uuid::new_v4(), String::new());
    assert_eq!(render_plan_card(&plan), "");
}

/// The buttons must actually carry the plan custom_ids, not just exist.
#[test]
fn the_buttons_carry_the_plan_custom_ids() {
    let src = flattened("src/channels/discord/plan_card.rs");
    assert!(
        src.contains("CreateButton::new(PLAN_APPROVE)"),
        "the Approve button must be built from PLAN_APPROVE"
    );
    assert!(
        src.contains("CreateButton::new(PLAN_DISCARD)"),
        "the Discard button must be built from PLAN_DISCARD"
    );
}

/// The keyboard sits in a channel any allowlisted member can see, so the
/// tapper is re-checked — same rule the tool-approval keyboard follows.
#[test]
fn the_card_is_owner_gated_and_routed_from_the_component_dispatch() {
    let src = flattened("src/channels/discord/agent.rs");
    assert!(
        src.contains("ifsuper::plan_card::is_plan_callback(custom_id){"),
        "the plan callbacks must be routed from the component dispatch"
    );
    assert!(
        src.contains("crate::config::owner::is_owner("),
        "the tapper must be re-checked before Approve or Discard runs"
    );
    assert!(
        src.contains("crate::utils::plan_mode::discard(session_id,&self.service_context).await"),
        "Discard must go through the shared plan_mode entry point"
    );
    assert!(
        src.contains(
            "crate::utils::plan_mode::try_approve(session_id,crate::tui::plan::ApprovalSource::User,).await"
        ),
        "Approve must use the same validator Telegram uses"
    );
    assert!(
        src.contains(
            "super::plan_card::remove_plan_card(&ctx.http,channel_id,&self.discord_state,session_id,).await"
        ),
        "Discard must take the card down with the plan"
    );
}

/// A plan created, approved, advanced or discarded mid-turn must show its new
/// state. Both places a Discord turn can finish have to reconcile the card, or
/// the board silently freezes on the pre-turn rendering.
#[test]
fn the_card_is_refreshed_after_both_delivery_paths() {
    let message_path = flattened("src/channels/discord/handler.rs");
    assert!(
        message_path.contains(
            "super::plan_card::refresh_plan_card(&ctx.http,target,&discord_state,session_id).await;"
        ),
        "the message path must reconcile the plan card"
    );
    let tap_path = flattened("src/channels/discord/interactions.rs");
    assert!(
        tap_path.contains(
            "super::plan_card::refresh_plan_card(&http,channel,&discord_state,session_id).await;"
        ),
        "the component-tap path must reconcile the plan card"
    );
}
