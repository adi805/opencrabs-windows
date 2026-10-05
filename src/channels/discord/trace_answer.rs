//! Final-answer recovery for trace mode (#1942).
//!
//! CLI providers stream every text block, the answer included, as
//! `IntermediateText` and return an empty `response.content`. Trace mode
//! folds each intermediate into the tool bubble as a clipped note and
//! posts nothing, so the empty-final guard would then skip the only
//! delivery the answer ever gets.

/// The text the final path should deliver: the final content when it has
/// any, otherwise (trace mode only) the last intermediate body that was
/// folded into the bubble instead of posted. With trace off an empty final
/// stays empty, because the intermediates were posted as real messages.
pub(crate) fn final_text_for_delivery(
    text_only: String,
    trace_narration: bool,
    last_trace_body: Option<String>,
) -> String {
    if !trace_narration || !text_only.trim().is_empty() {
        return text_only;
    }
    last_trace_body.unwrap_or(text_only)
}
