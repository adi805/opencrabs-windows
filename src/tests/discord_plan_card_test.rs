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
use crate::channels::telegram::flow_chrome::ProseSection;
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
    let body = render_plan_card(&plan, None);
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
    assert_eq!(render_plan_card(&plan, None), "");
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
/// state, and the card must MOVE with the conversation. Telegram re-sticks its
/// card on every settled turn (delete + repost at the bottom); Discord used to
/// only edit in place, which is exactly why the card stayed buried at the top
/// while the chat moved on. Both places a Discord turn can finish must run the
/// re-stick tail, or the board freezes at a stale position.
#[test]
fn the_card_is_resticked_after_both_delivery_paths() {
    let message_path = flattened("src/channels/discord/handler.rs");
    assert!(
        message_path.contains(
            "super::plan_card::restick_plan_card_after_turn(&ctx.http,target,&discord_state,session_id).await;"
        ),
        "the message path must re-stick the plan card after the turn"
    );
    let tap_path = flattened("src/channels/discord/interactions.rs");
    assert!(
        tap_path.contains(
            "super::plan_card::restick_plan_card_after_turn(&http,channel,&discord_state,session_id).await;"
        ),
        "the component-tap path must re-stick the plan card after the turn"
    );
}

/// The re-stick is the settle tail ONLY. Turn start and the 4 s flow ticker
/// must keep editing in place: re-sticking on every tick would churn the card
/// toward Discord's write limits for no benefit, since a tick is not a settled
/// turn. Pinned from source so a later "let's also restick there" fails loudly.
#[test]
fn the_ticker_and_turn_start_keep_the_in_place_refresh() {
    let handler = flattened("src/channels/discord/handler.rs");
    assert!(
        handler.contains(
            "super::plan_card::refresh_plan_card(&ctx.http,target,&discord_state,session_id).await;"
        ),
        "the turn-start path must keep refreshing in place, not re-sticking"
    );
    assert!(
        handler.contains("super::plan_card::refresh_plan_card(&http,channel,&dstate,sid).await;"),
        "the flow ticker must keep refreshing in place, not re-sticking"
    );
}

// ── #103: an Editing card must show the prose the user is approving ──────

fn prose(heading: Option<&str>, body: &str) -> ProseSection {
    ProseSection {
        heading: heading.map(str::to_string),
        body: body.to_string(),
    }
}

/// While Editing, `tasks[]` is empty (the checklist is seeded from the `.md`
/// only after approval), so the prose is the ONLY place the plan's contents
/// exist. A title-only card asked the user to approve a plan they could not
/// read (#103).
#[test]
fn editing_card_shows_the_plan_prose() {
    let plan = plan_with(PlanStatus::Editing, vec![]);
    let sections = vec![prose(
        Some("Implementation steps"),
        "1. Fix the clippy lint\n2. Push and watch CI",
    )];
    let body = render_plan_card(&plan, Some(&sections));
    assert!(body.contains("📋 **Discord UX parity**"), "title: {body}");
    assert!(
        body.contains("Fix the clippy lint"),
        "the steps must be readable on an Editing card: {body}"
    );
}

/// Active means the checklist is the content; prose is dropped so the
/// executing card stays lean.
#[test]
fn active_card_omits_prose() {
    let plan = plan_with(
        PlanStatus::Active,
        vec![("run it", TaskStatus::InProgress, 0)],
    );
    let sections = vec![prose(Some("Context"), "a very long design rationale")];
    let body = render_plan_card(&plan, Some(&sections));
    assert!(
        !body.contains("design rationale"),
        "prose leaked into an Active card: {body}"
    );
    assert!(body.contains("▶ **1. run it**"), "checklist: {body}");
}

/// The steps section is what the user is approving, so it must survive
/// truncation even when a long Context section precedes it in the file.
#[test]
fn the_steps_section_leads_the_excerpt() {
    let plan = plan_with(PlanStatus::Editing, vec![]);
    let sections = vec![
        prose(Some("Context"), "background that can be long"),
        prose(Some("Implementation steps"), "STEP-ONE do the thing"),
    ];
    let body = render_plan_card(&plan, Some(&sections));
    let steps_at = body.find("STEP-ONE").expect("steps present");
    let ctx_at = body
        .find("background that can be long")
        .expect("context present");
    assert!(steps_at < ctx_at, "steps must lead the excerpt: {body}");
}

/// Discord rejects a body over its cap outright, so the card must always fit,
/// however long the prose and the checklist get.
#[test]
fn the_card_never_exceeds_the_discord_cap() {
    let tasks: Vec<(&str, TaskStatus, usize)> = (0..40)
        .map(|_| {
            (
                "a fairly long task title that eats characters",
                TaskStatus::Pending,
                3,
            )
        })
        .collect();
    let plan = plan_with(PlanStatus::Editing, tasks);
    let sections = vec![prose(Some("Implementation steps"), &"x".repeat(5000))];
    let body = render_plan_card(&plan, Some(&sections));
    assert!(
        body.chars().count() <= 2000,
        "card is {} chars, over Discord's cap",
        body.chars().count()
    );
}

/// A cut excerpt must read as incomplete, not as the whole plan.
#[test]
fn a_truncated_excerpt_is_marked() {
    let plan = plan_with(PlanStatus::Editing, vec![]);
    let sections = vec![prose(Some("Implementation steps"), &"y".repeat(5000))];
    let body = render_plan_card(&plan, Some(&sections));
    assert!(body.ends_with('…'), "truncation must be marked: {body}");
    assert!(body.chars().count() <= 2000);
}

/// No prose on disk (a checklist-style plan) must not regress: the title and
/// checklist still render exactly as before.
#[test]
fn an_editing_card_without_prose_still_renders() {
    let plan = plan_with(
        PlanStatus::Editing,
        vec![("design it", TaskStatus::Pending, 0)],
    );
    let body = render_plan_card(&plan, None);
    assert!(body.contains("📋 **Discord UX parity**"), "{body}");
    assert!(body.contains("☐ **1. design it**"), "{body}");
}

/// The refresh path must actually load the `.md` prose and pass it in, or the
/// renderer fix never reaches a real card.
#[test]
fn the_refresh_path_loads_the_plan_prose() {
    let src = flattened("src/channels/discord/plan_card.rs");
    assert!(
        src.contains("flow_chrome::load_plan_prose(session_id).await"),
        "the card refresh must load the design prose"
    );
    assert!(
        src.contains("render_plan_card(&plan,prose.as_deref())"),
        "the loaded prose must be passed to the renderer"
    );
}
