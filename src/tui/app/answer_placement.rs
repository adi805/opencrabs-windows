//! Where a turn's final answer goes when it is written back to the chat.
//!
//! `complete_response` overwrites the last assistant row with the final
//! answer whenever an intermediate text arrived earlier in the turn. That is
//! only correct when no tool group ran after that row. If tools ran after it,
//! the overwrite moves the answer above those tools, and the fold then hides
//! it inside the working-out group. The answer must instead be a new row
//! after the tool groups.

use crate::tui::app::DisplayMessage;

/// True when the newest assistant row sits before a tool group, meaning tools
/// ran after the last visible text and a final answer must not overwrite it.
pub(crate) fn last_text_precedes_tool_group(messages: &[DisplayMessage]) -> bool {
    let last_assistant = messages.iter().rposition(|m| m.role == "assistant");
    let last_group = messages.iter().rposition(|m| m.role == "tool_group");
    match (last_assistant, last_group) {
        (Some(a), Some(g)) => a < g,
        _ => false,
    }
}
