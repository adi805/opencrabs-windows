//! A final answer that follows tool groups must be a row of its own, after
//! those groups. Overwriting an earlier text row moves it above the tools,
//! where the fold hides it.
//!
//! The mirror case is a halt on an option-surface tool: the answer streamed
//! before the tool ran, so its row is above the group, and the response the
//! loop synthesizes on the halt repeats that text. It must not become a second
//! row.

use crate::tui::app::answer_placement::{answer_already_on_screen, last_text_precedes_tool_group};
use crate::tui::app::{DisplayMessage, ToolCallEntry, ToolCallGroup};
use uuid::Uuid;

fn msg(role: &str, content: &str) -> DisplayMessage {
    DisplayMessage {
        id: Uuid::new_v4(),
        role: role.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        token_count: None,
        cost: None,
        approval: None,
        approve_menu: None,
        details: None,
        expanded: false,
        expanded_full: false,
        tool_group: None,
        duration_secs: None,
    }
}

fn group_msg() -> DisplayMessage {
    let mut m = msg("tool_group", "1 tool call");
    m.tool_group = Some(ToolCallGroup {
        calls: vec![ToolCallEntry {
            description: "suggest options".to_string(),
            success: true,
            details: None,
            completed: true,
            tool_input: serde_json::Value::Null,
        }],
        expanded: false,
    });
    m
}

#[test]
fn text_then_tool_group_is_stale_so_answer_must_append() {
    // Earlier text, then the options tool ran with no text row after it.
    let msgs = vec![
        msg("user", "hi"),
        msg("assistant", "Let me check"),
        group_msg(),
    ];
    assert!(last_text_precedes_tool_group(&msgs));
}

#[test]
fn text_after_the_last_tool_group_may_be_overwritten() {
    let msgs = vec![
        msg("user", "hi"),
        group_msg(),
        msg("assistant", "Here is the answer"),
    ];
    assert!(!last_text_precedes_tool_group(&msgs));
}

#[test]
fn no_tool_group_is_never_stale() {
    let msgs = vec![msg("user", "hi"), msg("assistant", "Hello")];
    assert!(!last_text_precedes_tool_group(&msgs));
}

#[test]
fn no_assistant_row_is_never_stale() {
    let msgs = vec![msg("user", "hi"), group_msg()];
    assert!(!last_text_precedes_tool_group(&msgs));
}

#[test]
fn text_above_the_last_tool_group_that_the_response_repeats_is_already_on_screen() {
    // suggest_options halted the turn after the answer streamed. The row sits
    // above the group and the response carries the same text, trailing
    // whitespace included.
    let answer = "Nothing was installed, built, or run.";
    let msgs = vec![
        msg("user", "did you run it"),
        msg("assistant", answer),
        group_msg(),
    ];
    assert!(answer_already_on_screen(&msgs, &format!("{answer}\n")));
}

#[test]
fn identical_text_from_an_earlier_turn_is_a_new_answer() {
    // The only identical row sits before the newest user row, so it belongs to
    // an earlier turn and this answer must be appended.
    let answer = "Nothing was installed, built, or run.";
    let msgs = vec![
        msg("user", "did you run it"),
        msg("assistant", answer),
        msg("user", "again"),
        group_msg(),
    ];
    assert!(!answer_already_on_screen(&msgs, answer));
}

#[test]
fn different_text_after_tool_groups_is_not_already_on_screen() {
    let msgs = vec![
        msg("user", "check it"),
        msg("assistant", "Let me check"),
        group_msg(),
    ];
    assert!(!answer_already_on_screen(&msgs, "Here is the answer"));
}

#[test]
fn no_assistant_row_is_never_already_on_screen() {
    let msgs = vec![msg("user", "hi"), group_msg()];
    assert!(!answer_already_on_screen(&msgs, "hello"));
}
