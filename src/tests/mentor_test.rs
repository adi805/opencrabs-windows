//! The loop review's contract (FR-001).
//!
//! Three things have to hold or the mechanism is worse than nothing: it fires
//! at the counted threshold, it stays silent below it, and it fires at most
//! once per turn. The wording matters too — the review has to carry an exit
//! for a turn that is genuinely finished, or it turns real completion into a
//! reason to call another tool.

use crate::brain::agent::service::mentor::{
    DIAGNOSTIC_QUESTIONS, LoopTally, MentorVerdict, SAME_TOOL_TRIGGER, TOTAL_CALL_TRIGGER,
    TriggerKind, mentor_block, observe_round,
};

/// AC-001: five identical calls produce the review, carrying all six questions.
#[test]
fn ac001_five_identical_calls_produce_the_six_question_review() {
    let mut t = LoopTally::new();
    let mut block = None;
    for _ in 0..SAME_TOOL_TRIGGER {
        block = observe_round(&mut t, Some("bash"));
    }
    let block = block.expect("the review must fire at the same-tool threshold");
    for (i, question) in DIAGNOSTIC_QUESTIONS.iter().enumerate() {
        assert!(
            block.contains(question),
            "the review must carry question {}: {question}",
            i + 1
        );
    }
    assert_eq!(
        DIAGNOSTIC_QUESTIONS.len(),
        6,
        "the checklist is six questions"
    );
}

/// AC-002, first arm: a turn under the threshold injects nothing.
#[test]
fn ac002_below_the_threshold_injects_nothing() {
    let mut t = LoopTally::new();
    for _ in 0..(SAME_TOOL_TRIGGER - 1) {
        assert_eq!(observe_round(&mut t, Some("bash")), None);
    }
    // Alternating tools never trip the same-tool trigger, so the total
    // backstop is the only thing that can fire here: stay one call below it,
    // or this test would be asserting silence at the exact call that breaks it.
    let mut u = LoopTally::new();
    for _ in 0..((TOTAL_CALL_TRIGGER / 2) - 1) {
        assert_eq!(observe_round(&mut u, Some("grep")), None);
        assert_eq!(observe_round(&mut u, Some("read_file")), None);
    }
    assert_eq!(
        observe_round(&mut u, Some("glob")),
        None,
        "a fresh-but-long turn stays quiet below the total"
    );
    assert_eq!(u.total(), TOTAL_CALL_TRIGGER - 1);
}

/// AC-002, second arm: the review must not trap a turn that is genuinely done.
#[test]
fn ac002_the_review_carries_a_finish_that_is_not_a_tool_call() {
    let block = mentor_block(&TriggerKind::SameTool {
        tool: "bash".to_string(),
        count: SAME_TOOL_TRIGGER,
    });
    assert!(
        block.contains("genuinely finished"),
        "the review must offer an exit that is not another tool call"
    );
    assert!(
        block.contains("do not run extra tool calls"),
        "the exit must forbid re-verifying a finished turn"
    );
}

/// AC-005: two triggers inside one turn yield exactly one review.
#[test]
fn ac005_two_triggers_in_one_turn_yield_one_review() {
    let mut t = LoopTally::new();
    let mut fired = 0;
    // Same-tool trigger first.
    for _ in 0..SAME_TOOL_TRIGGER {
        if observe_round(&mut t, Some("bash")).is_some() {
            fired += 1;
        }
    }
    // Then keep going well past the total-call trigger.
    for _ in 0..(TOTAL_CALL_TRIGGER * 2) {
        if observe_round(&mut t, Some("bash")).is_some() {
            fired += 1;
        }
    }
    assert_eq!(fired, 1, "the cap is per turn, not per burst");
}

/// The total-call backstop catches the loop that never repeats one call.
#[test]
fn a_long_cycle_of_distinct_tools_still_triggers_the_total_backstop() {
    let tools = ["grep", "read_file", "glob", "bash", "edit_file", "ls"];
    let mut t = LoopTally::new();
    let mut block = None;
    for i in 0..TOTAL_CALL_TRIGGER {
        block = observe_round(&mut t, Some(tools[i as usize % tools.len()]));
    }
    let block = block.expect("the total backstop must fire");
    assert!(block.contains(&format!("{TOTAL_CALL_TRIGGER} tool calls")));
    assert_eq!(t.total(), TOTAL_CALL_TRIGGER);
}

/// A new turn starts from a fresh tally, so nothing carries over.
#[test]
fn a_new_turn_starts_clean() {
    let mut t = LoopTally::new();
    for _ in 0..(SAME_TOOL_TRIGGER - 1) {
        observe_round(&mut t, Some("bash"));
    }
    // The loop builds one tally per turn; a fresh one is a fresh turn.
    let mut next_turn = LoopTally::new();
    assert_eq!(next_turn.total(), 0);
    assert_eq!(observe_round(&mut next_turn, Some("bash")), None);
}

/// A provider swap clears the replayed calls but keeps the per-turn cap.
#[test]
fn reset_counts_clears_the_tally_but_keeps_the_latch() {
    let mut t = LoopTally::new();
    let mut first = 0;
    for _ in 0..SAME_TOOL_TRIGGER {
        if observe_round(&mut t, Some("bash")).is_some() {
            first += 1;
        }
    }
    assert_eq!(first, 1, "the review fired before the swap");
    t.reset_counts();
    assert_eq!(t.total(), 0, "the replayed copies must not stack");
    // The latch is per turn, not per provider attempt, and the only way to see
    // it from outside is behaviour: the replay re-issues the same calls and
    // none of them may re-fire the review.
    for _ in 0..(TOTAL_CALL_TRIGGER * 2) {
        assert_eq!(observe_round(&mut t, Some("bash")), None);
    }
}

/// A round with no tools cannot repeat and is ignored outright.
#[test]
fn a_round_with_no_tool_never_triggers() {
    let mut t = LoopTally::new();
    for _ in 0..(TOTAL_CALL_TRIGGER * 3) {
        assert_eq!(observe_round(&mut t, None), None);
    }
    assert_eq!(t.total(), 0, "an empty round is not a call");
}

/// The verdict carries the count, and the count is what the model produced.
#[test]
fn the_review_names_the_offending_tool_and_its_count() {
    let mut t = LoopTally::new();
    let mut block = None;
    for _ in 0..SAME_TOOL_TRIGGER {
        block = observe_round(&mut t, Some("bash"));
    }
    let block = block.unwrap();
    assert!(block.contains("`bash`"));
    assert!(block.contains(&SAME_TOOL_TRIGGER.to_string()));
    // The quiet arm of the enum is the default for a fresh turn.
    assert_eq!(LoopTally::new().observe("bash"), MentorVerdict::Quiet);
}
