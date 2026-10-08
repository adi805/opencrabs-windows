//! Tests for `brain::agent::service::plan_reconcile` (PRD FR-002).
//!
//! Covers AC-003 (drift is patched so the plan matches the artifacts),
//! AC-004 (a clean transition patches nothing), and AC-006 (a resolved task is
//! never reverted and never dropped, and a refusal is recorded with a reason).

use uuid::Uuid;

use crate::brain::agent::service::plan_reconcile::*;
use crate::tui::plan::*;

/// A task with one declared artifact in its acceptance criteria.
fn task(order: usize, title: &str, status: TaskStatus, artifact: &str) -> PlanTask {
    let mut t = PlanTask::new(
        order,
        title.to_string(),
        String::new(),
        TaskType::Other("test".into()),
    );
    t.acceptance_criteria = vec![format!("`{artifact}` exists")];
    t.status = status;
    t
}

fn plan_with(tasks: Vec<PlanTask>) -> PlanDocument {
    let mut p = PlanDocument::new(Uuid::new_v4(), "Reconcile".to_string());
    p.tasks = tasks;
    p
}

fn evidence(paths: &[&str], claimed: &[usize]) -> WorkEvidence {
    WorkEvidence {
        present_paths: paths.iter().map(|s| s.to_string()).collect(),
        claimed_done: claimed.to_vec(),
    }
}

// ── AC-003: work done but not marked ────────────────────────────

#[test]
fn ac003_artifacts_present_and_work_started_marks_the_task_done() {
    let mut plan = plan_with(vec![task(
        1,
        "render the cut",
        TaskStatus::InProgress,
        "out/cut.mp4",
    )]);
    let verdict = reconcile(&plan, &evidence(&["out/cut.mp4"], &[]));
    let ReconcileVerdict::Drift { findings, patch } = verdict else {
        panic!("expected drift");
    };
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, DriftKind::UnmarkedCompletion);
    assert_eq!(patch.len(), 1);

    let applied = validate_patch(&plan, &patch);
    assert!(applied.rejected.is_empty(), "the patch must be admissible");
    let recorded = apply_patch(&mut plan, &applied.accepted);
    assert_eq!(recorded.len(), 1);
    // The plan now matches the artifacts on re-read.
    assert_eq!(
        plan.get_task_by_order(1).unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(
        reconcile(&plan, &evidence(&["out/cut.mp4"], &[])),
        ReconcileVerdict::NoDrift
    );
}

#[test]
fn ac003_a_reported_completion_is_patched_even_from_pending() {
    let mut plan = plan_with(vec![task(
        2,
        "write the page",
        TaskStatus::Pending,
        "site/index.html",
    )]);
    let ReconcileVerdict::Drift { patch, .. } =
        reconcile(&plan, &evidence(&["site/index.html"], &[2]))
    else {
        panic!("expected drift");
    };
    assert_eq!(patch.len(), 1);
    // Two statements, not one nested call: `&mut plan` and `&plan` cannot be
    // live in the same argument list.
    let accepted = validate_patch(&plan, &patch).accepted;
    apply_patch(&mut plan, &accepted);
    assert_eq!(
        plan.get_task_by_order(2).unwrap().status,
        TaskStatus::Completed
    );
}

#[test]
fn blast_radius_a_bare_pending_row_is_reported_but_never_closed() {
    // The plan is forward-only, so a wrong completion hides unfinished work.
    // A Pending row whose artifact happens to exist is reported, not patched.
    let plan = plan_with(vec![task(
        1,
        "edit the module",
        TaskStatus::Pending,
        "src/thing.rs",
    )]);
    let ReconcileVerdict::Drift { findings, patch } =
        reconcile(&plan, &evidence(&["src/thing.rs"], &[]))
    else {
        panic!("expected a finding");
    };
    assert_eq!(findings.len(), 1);
    assert!(
        patch.is_empty(),
        "a bare Pending row must not be auto-closed"
    );
}

// ── AC-004: a clean transition patches nothing ──────────────────

#[test]
fn ac004_no_drift_when_the_declared_artifact_is_absent() {
    let plan = plan_with(vec![task(
        1,
        "render the cut",
        TaskStatus::InProgress,
        "out/cut.mp4",
    )]);
    assert_eq!(
        reconcile(&plan, &evidence(&["out/other.mp4"], &[])),
        ReconcileVerdict::NoDrift
    );
}

#[test]
fn ac004_no_drift_when_the_task_declares_nothing_checkable() {
    let mut t = PlanTask::new(
        1,
        "think".into(),
        "reason about it".into(),
        TaskType::Other("x".into()),
    );
    t.status = TaskStatus::InProgress;
    let plan = plan_with(vec![t]);
    assert_eq!(
        reconcile(&plan, &evidence(&[], &[1])),
        ReconcileVerdict::NoDrift
    );
}

#[test]
fn ac004_a_resolved_task_is_never_re_examined() {
    let plan = plan_with(vec![task(
        1,
        "done already",
        TaskStatus::Completed,
        "out/cut.mp4",
    )]);
    assert_eq!(
        reconcile(&plan, &evidence(&["out/cut.mp4"], &[])),
        ReconcileVerdict::NoDrift
    );
}

// ── AC-006: the validator is the mechanical gate ────────────────

#[test]
fn ac006_a_resolved_task_is_never_reverted() {
    let plan = plan_with(vec![task(
        1,
        "shipped",
        TaskStatus::Completed,
        "out/cut.mp4",
    )]);
    let patch = vec![PatchOp::Modify {
        order: 1,
        status: Some(TaskStatus::Pending),
        reason: "changed my mind".into(),
    }];
    let v = validate_patch(&plan, &patch);
    assert!(v.accepted.is_empty());
    assert_eq!(v.rejected.len(), 1);
    assert!(matches!(
        v.rejected[0].1,
        RejectReason::RevertsResolvedTask { order: 1, .. }
    ));
}

#[test]
fn ac006_a_resolved_task_is_never_dropped() {
    let plan = plan_with(vec![task(
        1,
        "shipped",
        TaskStatus::Completed,
        "out/cut.mp4",
    )]);
    let v = validate_patch(
        &plan,
        &[PatchOp::Remove {
            order: 1,
            reason: "looks redundant".into(),
        }],
    );
    assert!(v.accepted.is_empty());
    assert_eq!(
        v.rejected[0].1,
        RejectReason::DropsResolvedTask { order: 1 }
    );
}

#[test]
fn ac006_a_refusal_always_carries_a_reason() {
    let plan = plan_with(vec![task(1, "open", TaskStatus::Pending, "a.rs")]);
    let v = validate_patch(
        &plan,
        &[
            PatchOp::Modify {
                order: 1,
                status: Some(TaskStatus::Completed),
                reason: String::new(),
            },
            PatchOp::Remove {
                order: 99,
                reason: "ghost".into(),
            },
            PatchOp::Modify {
                order: 42,
                status: Some(TaskStatus::Completed),
                reason: "ghost too".into(),
            },
        ],
    );
    assert!(v.accepted.is_empty());
    assert_eq!(v.rejected.len(), 3);
    for (_, reason) in &v.rejected {
        assert!(
            !reason.to_string().is_empty(),
            "every refusal must be explainable"
        );
    }
}

#[test]
fn ac006_one_bad_op_does_not_take_the_batch_down() {
    let plan = plan_with(vec![
        task(1, "shipped", TaskStatus::Completed, "a.rs"),
        task(2, "open", TaskStatus::Pending, "b.rs"),
    ]);
    let v = validate_patch(
        &plan,
        &[
            PatchOp::Modify {
                order: 2,
                status: Some(TaskStatus::Completed),
                reason: "artifacts present".into(),
            },
            PatchOp::Remove {
                order: 1,
                reason: "drop it".into(),
            },
        ],
    );
    assert_eq!(v.accepted.len(), 1);
    assert_eq!(v.rejected.len(), 1);
    assert_eq!(v.accepted[0].order(), 2);
}

#[test]
fn apply_patch_records_every_mutation() {
    let mut plan = plan_with(vec![task(1, "open", TaskStatus::Pending, "b.rs")]);
    let patch = vec![PatchOp::Modify {
        order: 1,
        status: Some(TaskStatus::Completed),
        reason: "artifacts present".into(),
    }];
    let recorded = apply_patch(&mut plan, &patch);
    assert_eq!(recorded.len(), 1);
    let notes = plan.get_task_by_order(1).unwrap().notes.clone().unwrap();
    assert!(
        notes.contains("[reconcile]"),
        "the mutation must leave a trace"
    );
    assert!(notes.contains("artifacts present"));
}

// ── artifact extraction ─────────────────────────────────────────

#[test]
fn declared_artifacts_takes_paths_and_leaves_prose_alone() {
    let mut t = PlanTask::new(
        1,
        "x".into(),
        "write out/cut.mp4".into(),
        TaskType::Other("x".into()),
    );
    t.acceptance_criteria = vec![
        "`src/a.rs` compiles".into(),
        "9/9 pass, AC-003 and Lint/Test agree".into(),
        "see https://example.com/page.html".into(),
    ];
    let got = declared_artifacts(&t);
    assert!(got.iter().any(|a| a == "out/cut.mp4"));
    assert!(got.iter().any(|a| a == "src/a.rs"));
    assert!(
        !got.iter()
            .any(|a| a.contains("AC-003") || a.contains("Lint/Test")),
        "prose tokens are not artifacts: {got:?}"
    );
}

#[test]
fn declared_artifacts_is_deduplicated_and_case_folded() {
    let mut t = PlanTask::new(
        1,
        "x".into(),
        "touch Out/Cut.MP4".into(),
        TaskType::Other("x".into()),
    );
    t.acceptance_criteria = vec!["out/cut.mp4 exists".into()];
    assert_eq!(declared_artifacts(&t).len(), 1);
}

#[test]
fn a_windows_separator_matches_a_posix_path() {
    let plan = plan_with(vec![task(1, "win", TaskStatus::InProgress, "src\\mod.rs")]);
    assert!(matches!(
        reconcile(&plan, &evidence(&["src/mod.rs"], &[])),
        ReconcileVerdict::Drift { .. }
    ));
}

// ── other drift classes ─────────────────────────────────────────

#[test]
fn duplicate_titles_are_reported_without_a_patch() {
    let plan = plan_with(vec![
        task(1, "audit", TaskStatus::Pending, "a.rs"),
        task(2, "Audit", TaskStatus::Pending, "b.rs"),
    ]);
    let ReconcileVerdict::Drift { findings, patch } = reconcile(&plan, &evidence(&[], &[])) else {
        panic!("expected a finding");
    };
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, DriftKind::DuplicateTitle);
    assert_eq!(findings[0].order, 2);
    assert!(
        patch.is_empty(),
        "a duplicate is reported, never auto-dropped"
    );
}

#[test]
fn a_duplicate_of_a_resolved_row_is_removed_with_its_reason() {
    // The one duplicate case with a safe automatic answer: the sibling is
    // already done, so the open row is a leftover and removing it cannot
    // delete work nobody did.
    let mut plan = plan_with(vec![
        task(1, "audit", TaskStatus::Completed, "a.rs"),
        task(2, "Audit", TaskStatus::Pending, "b.rs"),
    ]);
    let ReconcileVerdict::Drift { findings, patch } = reconcile(&plan, &evidence(&[], &[])) else {
        panic!("expected a finding");
    };
    assert_eq!(findings.len(), 1);
    assert_eq!(patch.len(), 1);
    assert!(matches!(patch[0], PatchOp::Remove { order: 2, .. }));
    assert!(patch[0].reason().contains("already covers it"));

    let v = validate_patch(&plan, &patch);
    assert!(
        v.rejected.is_empty(),
        "a resolved sibling may cover the row"
    );
    let recorded = apply_patch(&mut plan, &v.accepted);
    assert_eq!(recorded.len(), 1);
    assert_eq!(plan.tasks.len(), 1, "the leftover row is gone");
    assert_eq!(
        plan.get_task_by_order(1).unwrap().status,
        TaskStatus::Completed
    );
}

#[test]
fn a_dependency_on_a_missing_order_is_reported() {
    let mut t = PlanTask::new(1, "x".into(), String::new(), TaskType::Other("x".into()));
    t.dependencies = vec![TaskDep::Index(7)];
    let plan = plan_with(vec![t]);
    let ReconcileVerdict::Drift { findings, .. } = reconcile(&plan, &evidence(&[], &[])) else {
        panic!("expected a finding");
    };
    assert_eq!(findings[0].kind, DriftKind::DanglingDependency);
}

// ── evidence collection ─────────────────────────────────────────

#[test]
fn collect_evidence_stats_the_declared_paths() {
    let dir = std::env::temp_dir().join(format!("oc-reconcile-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("made.txt"), b"x").unwrap();
    let plan = plan_with(vec![
        task(1, "made", TaskStatus::InProgress, "made.txt"),
        task(2, "not made", TaskStatus::InProgress, "missing.txt"),
    ]);
    let ev = collect_evidence(&plan, &dir);
    assert!(ev.present_paths.iter().any(|p| p == "made.txt"));
    assert!(!ev.present_paths.iter().any(|p| p == "missing.txt"));
    std::fs::remove_dir_all(&dir).ok();
}
