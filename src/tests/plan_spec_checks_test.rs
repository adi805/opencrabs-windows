//! Tests for per-task spec checks (#1904).
//!
//! Acceptance criteria were prose nobody ran: `verify_task_completion`
//! resolved its commands from the task *type*, so a criterion like
//! "cargo test --lib foo reports 4 passed" never executed. A spec check is
//! the runnable half of that criterion, extracted once at `start`, frozen
//! with a sha256, and run at `complete`.
//!
//! Extraction runs first here because it carries the risk: a wrong command
//! locked at start is executed at complete, which would fail a task that was
//! done right. Every refusal below is deliberate.

use crate::brain::tools::plan_tool::{PlanTool, lock_spec_check_if_absent, run_locked_spec_check};
use crate::brain::tools::{Tool, ToolExecutionContext};
use crate::config::profile::{home_for_profile, with_profile_home_async};
use crate::tui::plan::{
    ApprovalSource, PlanDocument, PlanTask, SpecCheck, TaskStatus, TaskType,
    extract_check_commands, is_verifiable, spec_check_hash,
};
use crate::utils::plan_files::{load_plan, save_plan};
use serde_json::json;
use std::fs;
use uuid::Uuid;

fn criteria(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ── extraction: the two shapes that lock a command ───────────────────

#[test]
fn plan_spec_check_extracts_a_backticked_command() {
    let commands = extract_check_commands(&criteria(&["`cargo fmt --all -- --check` exits 0"]));
    assert_eq!(commands, vec!["cargo fmt --all -- --check".to_string()]);
}

#[test]
fn plan_spec_check_extracts_a_runner_up_to_its_expectation_stem() {
    let commands = extract_check_commands(&criteria(&[
        "cargo test --lib pricing reports 4 passed, 0 failed",
    ]));
    assert_eq!(commands, vec!["cargo test --lib pricing".to_string()]);
}

#[test]
fn plan_spec_check_skips_a_backticked_noun_and_takes_the_real_command() {
    let commands = extract_check_commands(&criteria(&[
        "the `slugify` helper is wired in and `cargo test --lib slugify` reports 0 failed",
    ]));
    assert_eq!(commands, vec!["cargo test --lib slugify".to_string()]);
}

#[test]
fn plan_spec_check_keeps_a_runner_whose_name_ends_in_a_digit() {
    let commands = extract_check_commands(&criteria(&[
        "python3 -m unittest discover -s tests -v reports 12 passed",
    ]));
    assert_eq!(
        commands,
        vec!["python3 -m unittest discover -s tests -v".to_string()]
    );
}

#[test]
fn plan_spec_check_deduplicates_a_command_named_twice() {
    let commands = extract_check_commands(&criteria(&[
        "cargo test --lib pricing reports 0 failed",
        "cargo test --lib pricing exits 0",
    ]));
    assert_eq!(commands, vec!["cargo test --lib pricing".to_string()]);
}

#[test]
fn plan_spec_check_keeps_criteria_order() {
    let commands = extract_check_commands(&criteria(&[
        "cargo fmt --all -- --check exits 0",
        "cargo test --lib pricing reports 0 failed",
    ]));
    assert_eq!(
        commands,
        vec![
            "cargo fmt --all -- --check".to_string(),
            "cargo test --lib pricing".to_string()
        ]
    );
}

// ── extraction: every refusal below is on purpose ────────────────────

#[test]
fn plan_spec_check_locks_nothing_from_a_prose_criterion() {
    let commands = extract_check_commands(&criteria(&["the documentation reads well"]));
    assert!(
        commands.is_empty(),
        "prose is not a check, got {commands:?}"
    );
}

#[test]
fn plan_spec_check_locks_nothing_when_the_tail_is_prose() {
    // A runner appears, but what follows it is a sentence, not a command
    // line. Slicing at the trailing stem would lock "cargo test ... then
    // check the output is", which is not runnable at all.
    let commands = extract_check_commands(&criteria(&[
        "run cargo test --all-features then check the output is clean",
    ]));
    assert!(commands.is_empty(), "prose tail, got {commands:?}");
}

#[test]
fn plan_spec_check_locks_nothing_from_a_bare_runner() {
    // `cargo` alone prints help and exits 0, which the gate would read as a
    // pass on any task.
    let commands = extract_check_commands(&criteria(&["`cargo` exits 0"]));
    assert!(commands.is_empty(), "bare runner, got {commands:?}");
}

#[test]
fn plan_spec_check_refuses_a_compound_command() {
    let commands = extract_check_commands(&criteria(&["`cargo test && git push` exits 0"]));
    assert!(commands.is_empty(), "compound command, got {commands:?}");
}

#[test]
fn plan_spec_check_refuses_a_piped_command() {
    let commands = extract_check_commands(&criteria(&[
        "cargo test --lib pricing | tail -5 shows 0 failed",
    ]));
    assert!(commands.is_empty(), "piped command, got {commands:?}");
}

#[test]
fn plan_spec_check_does_not_read_a_runner_out_of_a_longer_word() {
    // "digit" ends in "git": the pattern is matched at a word boundary so a
    // noun cannot drag a command out of the sentence.
    assert!(command_start("digit total shows 4 entries").is_none());
    assert!(command_start("the github page shows 0 open issues").is_none());
}

#[test]
fn plan_spec_check_ignores_a_criterion_with_no_expectation() {
    assert!(!is_verifiable("cargo build --release"));
    assert!(extract_check_commands(&criteria(&["cargo build --release"])).is_empty());
}

// ── the lock itself ─────────────────────────────────────────────────

#[test]
fn plan_spec_check_hash_separates_command_boundaries() {
    let joined_left = vec!["cargo test --lib a b".to_string(), "c".to_string()];
    let joined_right = vec!["cargo test --lib a".to_string(), "b c".to_string()];
    assert_ne!(
        spec_check_hash(&joined_left),
        spec_check_hash(&joined_right),
        "where the boundary falls must change the hash"
    );
}

#[test]
fn plan_spec_check_lock_matches_only_the_commands_it_froze() {
    let locked = SpecCheck::lock(vec!["cargo test --lib pricing".to_string()]);
    assert!(locked.matches(&["cargo test --lib pricing".to_string()]));
    assert!(!locked.matches(&["cargo test --lib pricing --".to_string()]));
    assert!(!locked.matches(&[]));
}

#[test]
fn plan_spec_check_with_an_empty_hash_never_matches() {
    // A hand-edited plan file could carry the struct with no hash. An empty
    // hash must not read as "locked and unchanged".
    let forged = SpecCheck {
        commands: vec!["cargo test".to_string()],
        sha256: String::new(),
    };
    assert!(!forged.matches(&["cargo test".to_string()]));
}

#[test]
fn plan_spec_check_round_trips_on_the_task() {
    let mut task = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Test);
    task.spec_check = Some(SpecCheck::lock(vec![
        "cargo test --lib pricing".to_string(),
    ]));
    let json = serde_json::to_string(&task).unwrap();
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    let restored = back.spec_check.expect("spec check survived serde");
    assert_eq!(
        restored.commands,
        vec!["cargo test --lib pricing".to_string()]
    );
    assert_eq!(restored.sha256, spec_check_hash(&restored.commands));
}

#[test]
fn plan_spec_check_is_absent_from_json_when_not_locked() {
    let task = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Test);
    let json = serde_json::to_string(&task).unwrap();
    assert!(
        !json.contains("spec_check"),
        "an unlocked task must not carry the key: {json}"
    );
    let back: PlanTask = serde_json::from_str(&json).unwrap();
    assert!(back.spec_check.is_none());
}

/// Test-local wrapper: the module keeps `command_start_index` private, so
/// this exercises it through the public extractor instead.
fn command_start(criterion: &str) -> Option<String> {
    extract_check_commands(&criteria(&[criterion]))
        .into_iter()
        .next()
}

// ── locking at start, running at complete ────────────────────────────
//
// The lock is the whole point of the feature: a check re-derived at
// completion time would be a check the task can rewrite for itself.

/// A project `ralph_loop.toml`, so the policy comes from this tempdir and never
/// from the machine-wide safety copy (a test that reads the user's real config
/// passes or fails with their setup).
fn policy_dir(name: &str, spec_checks: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("spec1904_{name}_"))
        .tempdir()
        .expect("tempdir");
    fs::write(
        dir.path().join("ralph_loop.toml"),
        format!("[verification]\nenabled = true\nspec_checks = \"{spec_checks}\"\n"),
    )
    .expect("write config");
    dir
}

fn plan_with_one_task(criteria_items: Vec<String>) -> PlanDocument {
    let mut plan = PlanDocument::new(Uuid::new_v4(), "spec check plan".to_string());
    let mut task = PlanTask::new(1, "t".to_string(), "d".to_string(), TaskType::Edit);
    task.acceptance_criteria = criteria_items;
    plan.add_task(task);
    plan
}

/// `ls .` is the pass case on purpose: it exists on every machine this runs
/// on, exits 0, and prints no `test result:` line, so it cannot trip the
/// vacuous-pass guard by accident.
fn passing_criteria() -> Vec<String> {
    criteria(&["`ls .` exits 0"])
}

/// `git log --oneline` exits non-zero in a directory that is not a repository,
/// which is what a fresh tempdir is. No compilation, no fixtures.
fn failing_criteria() -> Vec<String> {
    criteria(&["`git log --oneline` exits 0"])
}

#[test]
fn plan_spec_check_locks_once_and_ignores_a_later_rewrite() {
    let dir = policy_dir("lock_once", "advisory");
    let mut plan = plan_with_one_task(passing_criteria());

    let note = lock_spec_check_if_absent(&mut plan, 1, dir.path()).expect("criteria are runnable");
    assert!(
        note.contains("ls ."),
        "the note must name the check: {note}"
    );

    let locked = plan
        .get_task_by_order(1)
        .unwrap()
        .spec_check
        .clone()
        .expect("locked");
    assert_eq!(locked.commands, vec!["ls .".to_string()]);

    // A second call, which is what the post-compaction recovery `plan start`
    // is, and what a retry after a failure is, must not re-derive the check.
    plan.get_task_by_order_mut(1).unwrap().acceptance_criteria = failing_criteria();
    assert!(lock_spec_check_if_absent(&mut plan, 1, dir.path()).is_none());
    assert_eq!(
        plan.get_task_by_order(1).unwrap().spec_check.as_ref(),
        Some(&locked),
        "the lock must survive both the rewrite and the second start"
    );
}

#[test]
fn plan_spec_check_locks_nothing_under_off_or_without_a_command() {
    let off = policy_dir("lock_off", "off");
    let mut plan = plan_with_one_task(passing_criteria());
    assert!(lock_spec_check_if_absent(&mut plan, 1, off.path()).is_none());
    assert!(plan.get_task_by_order(1).unwrap().spec_check.is_none());

    let on = policy_dir("lock_prose", "advisory");
    let mut prose = plan_with_one_task(criteria(&["the parser rejects a bad slug"]));
    assert!(lock_spec_check_if_absent(&mut prose, 1, on.path()).is_none());
    assert!(
        prose.get_task_by_order(1).unwrap().spec_check.is_none(),
        "prose that names no runner locks nothing and behaves as before"
    );
}

#[test]
fn plan_spec_check_a_pass_is_proof() {
    let dir = policy_dir("pass", "advisory");
    let mut plan = plan_with_one_task(passing_criteria());
    lock_spec_check_if_absent(&mut plan, 1, dir.path());

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(result.passed, "`ls .` exits 0");
    assert!(result.note.is_none());
    assert!(!result.rejected);
    assert_eq!(result.commands, vec!["ls .".to_string()]);
}

#[test]
fn plan_spec_check_a_failure_is_refused_only_under_block() {
    let advisory = policy_dir("fail_advisory", "advisory");
    let mut plan = plan_with_one_task(failing_criteria());
    lock_spec_check_if_absent(&mut plan, 1, advisory.path());

    let result = run_locked_spec_check(&plan, 1, advisory.path());
    assert!(!result.passed, "a non-zero exit is not a pass");
    assert!(!result.rejected, "advisory accepts the completion");
    assert!(
        result.note.expect("must explain").contains("failed"),
        "the note must say the check failed"
    );

    let blocking = policy_dir("fail_block", "block");
    let mut blocked = plan_with_one_task(failing_criteria());
    lock_spec_check_if_absent(&mut blocked, 1, blocking.path());

    let result = run_locked_spec_check(&blocked, 1, blocking.path());
    assert!(result.rejected, "block refuses the completion");
    assert!(!result.passed);
}

#[test]
fn plan_spec_check_a_rewritten_criterion_is_refused_and_not_executed() {
    let dir = policy_dir("stale", "block");
    let marker = dir.path().join("ran.txt");
    let mut plan = plan_with_one_task(criteria(&["`git log --oneline` exits 0"]));

    // Lock a command that would leave a trace, then rewrite the criterion.
    let task = plan.get_task_by_order_mut(1).unwrap();
    task.spec_check = Some(SpecCheck::lock(vec![format!("touch {}", marker.display())]));
    task.acceptance_criteria = passing_criteria();

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(result.rejected, "a lock that no longer matches is refused");
    assert!(!result.passed);
    assert!(
        !marker.exists(),
        "a stale lock must not execute: its result would belong to a criterion \
         that no longer exists"
    );
    assert!(
        result
            .note
            .expect("must explain")
            .contains("no longer matches"),
        "the note must name the real cause"
    );
}

#[test]
fn plan_spec_check_an_unlocked_task_runs_nothing() {
    let dir = policy_dir("none", "block");
    let plan = plan_with_one_task(passing_criteria());

    let result = run_locked_spec_check(&plan, 1, dir.path());
    assert!(!result.passed && !result.rejected);
    assert!(result.note.is_none());
    assert!(result.commands.is_empty());
}

// ── the tool path: `start` locks, `complete` judges ──────────────────
//
// The helpers above pin the pieces. These pin the wiring: a fail-open bug
// here would log a passing belief for a check that never ran, which is the
// failure the whole feature exists to prevent.

async fn in_temp_home<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let profile = format!("spec-check-test-{}", Uuid::new_v4());
    let out = with_profile_home_async(Some(&profile), f).await;
    let home = home_for_profile(Some(&profile));
    let _ = std::fs::remove_dir_all(&home);
    out
}

async fn run_tool(
    tool: &PlanTool,
    ctx: &ToolExecutionContext,
    input: serde_json::Value,
) -> (bool, String) {
    let r = tool.execute(input, ctx).await.unwrap();
    let text = if r.success {
        r.output
    } else {
        r.error.unwrap_or_default()
    };
    (r.success, text)
}

/// A plan with one task carrying `criterion`, bound to a context whose working
/// directory is the policy tempdir, so the policy comes from there and never
/// from the machine-wide ralph_loop.toml.
async fn tool_with_one_task(
    dir: &tempfile::TempDir,
    tool: &PlanTool,
    criterion: &str,
) -> ToolExecutionContext {
    let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
    ctx.working_directory = dir.path().to_path_buf();
    let (ok, out) = run_tool(
        tool,
        &ctx,
        json!({
            "operation": "init",
            "title": "spec check plan",
            "mode": "checklist",
            "tasks": [{
                "title": "t",
                "description": "d",
                "task_type": "edit",
                "acceptance_criteria": [criterion]
            }]
        }),
    )
    .await;
    assert!(ok, "init must succeed: {out}");
    if let Some(mut plan) = load_plan(ctx.session_id).await {
        plan.approve(ApprovalSource::User);
        save_plan(&plan).await.unwrap();
    }
    ctx
}

#[tokio::test]
async fn plan_spec_check_tool_locks_at_start_and_reports_the_pass() {
    in_temp_home(async {
        let dir = policy_dir("tool_pass", "advisory");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`ls .` exits 0").await;

        let (ok, out) = run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        assert!(ok, "start must succeed: {out}");
        assert!(
            out.contains("Locked spec check"),
            "start must echo the check it locked: {out}"
        );

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "a passing check must not block the completion: {out}");
        assert!(
            out.contains("Locked spec check passed"),
            "the pass must be reported: {out}"
        );
        assert!(
            !out.contains("UNCERTAIN"),
            "a check that ran and exited 0 is proof, not silence: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_block_refuses_a_failing_check() {
    in_temp_home(async {
        let dir = policy_dir("tool_block", "block");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`git log --oneline` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(!ok, "block must refuse the completion: {out}");
        assert!(out.contains("spec check REJECTED"), "got: {out}");

        let plan = load_plan(ctx.session_id).await.unwrap();
        assert_ne!(
            plan.get_task_by_order(1).unwrap().status,
            TaskStatus::Completed,
            "a refused completion must not mark the task done"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_advisory_downgrades_a_failing_check() {
    in_temp_home(async {
        let dir = policy_dir("tool_advisory", "advisory");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`git log --oneline` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;
        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "advisory accepts the completion: {out}");
        assert!(
            out.contains("UNCERTAIN") && out.contains("locked spec check failed"),
            "the downgrade must name the check that failed: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_never_runs_a_criterion_rewritten_after_start() {
    in_temp_home(async {
        let dir = policy_dir("tool_stale", "block");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`ls .` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;

        // The hole the lock exists to close: the criterion is relaxed after
        // the work started, so the task can complete against wording nobody
        // approved.
        let mut plan = load_plan(ctx.session_id).await.unwrap();
        plan.get_task_by_order_mut(1).unwrap().acceptance_criteria =
            criteria(&["`git log --oneline` exits 0"]);
        save_plan(&plan).await.unwrap();

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(!ok, "a rewritten criterion must be refused: {out}");
        assert!(
            out.contains("no longer matches"),
            "the refusal must name the cause: {out}"
        );
    })
    .await;
}

#[tokio::test]
async fn plan_spec_check_tool_advisory_downgrades_a_criterion_rewritten_after_start() {
    in_temp_home(async {
        let dir = policy_dir("tool_stale_advisory", "advisory");
        let tool = PlanTool;
        let ctx = tool_with_one_task(&dir, &tool, "`ls .` exits 0").await;

        run_tool(&tool, &ctx, json!({ "operation": "start" })).await;

        let mut plan = load_plan(ctx.session_id).await.unwrap();
        plan.get_task_by_order_mut(1).unwrap().acceptance_criteria =
            criteria(&["`git log --oneline` exits 0"]);
        save_plan(&plan).await.unwrap();

        let (ok, out) = run_tool(
            &tool,
            &ctx,
            json!({ "operation": "complete", "task_order": 1, "action": "success" }),
        )
        .await;
        assert!(ok, "advisory accepts the completion: {out}");
        assert!(
            out.contains("no longer matches") && out.contains("UNCERTAIN"),
            "the downgrade must name the criterion that moved: {out}"
        );
    })
    .await;
}
