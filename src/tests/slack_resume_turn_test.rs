//! Failure mapping for the Slack background-resume flow turn (#1986).
//!
//! The turn itself is a network task, so the tests pin the decision it
//! settles on: a deadline reads as timed out, anything else as failed.

use crate::channels::slack::resume_turn::resume_failure_outcome;
use crate::channels::slack::tool_group::TurnOutcome;

#[test]
fn timeout_wording_settles_as_timed_out() {
    assert!(matches!(
        resume_failure_outcome("provider request timed out after 600s"),
        TurnOutcome::TimedOut
    ));
    assert!(matches!(
        resume_failure_outcome("Deadline exceeded while streaming"),
        TurnOutcome::TimedOut
    ));
}

#[test]
fn other_errors_settle_as_failed() {
    assert!(matches!(
        resume_failure_outcome("provider returned HTTP 500"),
        TurnOutcome::Failed
    ));
}
