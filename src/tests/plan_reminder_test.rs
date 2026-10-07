//! Tests for the pinned plan reminder (discussion #177): the active plan's
//! incomplete tasks are re-injected at the end of the prompt each turn so the
//! model doesn't forget it's mid-plan in a long conversation.
//!
//! Lifecycle-engine rule: only an Active checklist is reminded. Editing
//! plans (pre-init flag or design prose) get no execution nagging: telling
//! an Editing session to `plan start` would corrupt the design track.

use crate::brain::agent::service::format_plan_reminder;
use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use uuid::Uuid;

fn plan_with(status: PlanStatus, tasks: Vec<(&str, TaskStatus)>) -> PlanDocument {
    let mut p = PlanDocument::new(Uuid::new_v4(), "Ship login flow".to_string());
    p.status = status;
    for (i, (title, st)) in tasks.into_iter().enumerate() {
        let mut t = PlanTask::new(i, title.to_string(), String::new(), TaskType::Edit);
        t.status = st;
        p.add_task(t);
    }
    p
}

#[test]
fn active_plan_with_incomplete_tasks_is_reminded() {
    let plan = plan_with(
        PlanStatus::Active,
        vec![
            ("wire auth service", TaskStatus::Completed),
            ("build login form", TaskStatus::InProgress),
            ("add tests", TaskStatus::Pending),
        ],
    );
    let out = format_plan_reminder(&plan).expect("should remind for an in-flight plan");
    assert!(out.contains("ACTIVE PLAN REMINDER"));
    assert!(out.contains("1/3 done"));
    assert!(
        out.contains("→ Task 1: build login form"),
        "in-progress task must be flagged with its order + title, got: {out}"
    );
    assert!(
        out.contains("☐ Task 2: add tests"),
        "pending task must be listed with its order + title, got: {out}"
    );
    // Completed tasks are not listed.
    assert!(!out.contains("wire auth service"));
}

#[test]
fn editing_plan_is_not_reminded() {
    // An Editing plan (design prose, checklist not live) must not nag.
    let plan = plan_with(PlanStatus::Editing, vec![("x", TaskStatus::Pending)]);
    assert!(format_plan_reminder(&plan).is_none());
}

#[test]
fn pre_init_sidecar_is_not_reminded() {
    // The durable pre-init flag must never trigger checklist nagging, even
    // if a stray status/tasks combination sneaks onto the sidecar.
    let mut plan = plan_with(PlanStatus::Active, vec![("x", TaskStatus::Pending)]);
    plan.pre_init_editing = true;
    assert!(format_plan_reminder(&plan).is_none());
}

#[test]
fn legacy_editing_statuses_map_to_editing_and_are_not_reminded() {
    // Legacy "Draft" / "PendingApproval" / "Rejected" strings deserialize
    // to Editing, which never nags.
    for legacy in ["Draft", "PendingApproval", "Rejected"] {
        let status: PlanStatus = serde_json::from_str(&format!("\"{legacy}\"")).unwrap();
        assert_eq!(status, PlanStatus::Editing, "{legacy} must map to Editing");
        let plan = plan_with(status, vec![("x", TaskStatus::Pending)]);
        assert!(format_plan_reminder(&plan).is_none());
    }
}

#[test]
fn fully_resolved_plan_is_not_reminded() {
    // All tasks done or skipped → nothing left, no reminder.
    let plan = plan_with(
        PlanStatus::Active,
        vec![("a", TaskStatus::Completed), ("b", TaskStatus::Skipped)],
    );
    assert!(format_plan_reminder(&plan).is_none());
}

#[test]
fn legacy_active_statuses_map_to_active_and_are_reminded() {
    for legacy in ["Approved", "InProgress"] {
        let status: PlanStatus = serde_json::from_str(&format!("\"{legacy}\"")).unwrap();
        assert_eq!(status, PlanStatus::Active, "{legacy} must map to Active");
        let plan = plan_with(status, vec![("only task", TaskStatus::Pending)]);
        assert!(format_plan_reminder(&plan).is_some());
    }
}

// ---- #1903: staleness cutoff ----------------------------------------
// An Active plan whose file has not moved in `agent.plan_reminder_max_age_hours`
// (default 24h) must stop injecting the full execution reminder every turn:
// under a provider with no native plan tool the model can neither `complete`
// nor `discard`, so the nag was permanent, unremovable prompt overhead. Past
// the cutoff it shrinks to a one-line stale note naming the human exits.

#[test]
fn stale_active_plan_gets_one_line_note_not_full_reminder() {
    let mut plan = plan_with(
        PlanStatus::Active,
        vec![
            ("wire auth service", TaskStatus::Completed),
            ("build login form", TaskStatus::InProgress),
            ("add tests", TaskStatus::Pending),
        ],
    );
    plan.updated_at = chrono::Utc::now() - chrono::Duration::hours(48);
    let out = format_plan_reminder(&plan).expect("stale plans still get a note");
    assert!(
        out.contains("ACTIVE PLAN REMINDER (stale)"),
        "stale plans must be marked stale, got: {out}"
    );
    assert!(
        !out.contains("Keep executing"),
        "the full execution nag must not ride on a stale plan, got: {out}"
    );
    assert!(
        out.contains("plan discard"),
        "the note must name the out-of-band exits, got: {out}"
    );
    assert!(
        !out.contains("→ Task 1"),
        "the task listing must be suppressed when stale, got: {out}"
    );
}

#[test]
fn fresh_active_plan_reminds_in_full() {
    // updated_at = now (set by add_task in the helper): current behaviour
    // must be untouched inside the cutoff window.
    let plan = plan_with(
        PlanStatus::Active,
        vec![
            ("wire auth service", TaskStatus::Completed),
            ("build login form", TaskStatus::InProgress),
        ],
    );
    let out = format_plan_reminder(&plan).expect("fresh in-flight plan must be reminded");
    assert!(out.contains("Keep executing"));
    assert!(!out.contains("stale"));
}
