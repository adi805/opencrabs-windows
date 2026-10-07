//! Context-aware request budgets.
//!
//! A model context window is shared by input, hidden reasoning, and output.
//! Reserving the global 65,536-token output cap unchanged on a 200K route
//! leaves barely 134K for the conversation and makes every tool round collide
//! with compaction. Keep this arithmetic pure and provider-agnostic: the caller
//! supplies the active context window, and this module leaves at least 80% for
//! input while respecting the configured output ceiling.

/// Maximum share of a context window one request may reserve for output.
const OUTPUT_WINDOW_PERCENT: u32 = 20;

/// Output allowance for one compaction continuation document, in tokens.
///
/// The continuation document is the woken agent's *entire memory*, so it is
/// sized on its own terms rather than inheriting the chat allowance (#1930).
/// A chat turn may legitimately emit 40 000 tokens; a document that large
/// defeats its own purpose — every round re-bills a document several times its
/// useful size, so the session lands near where it started and re-crosses the
/// compaction trigger. Both sides of the budget read this one constant: the
/// summariser's `max_output_tokens` and the input-side `output_reserve` derive
/// from it, so the allowance and the reserve cannot drift apart.
pub const COMPACTION_SUMMARY_MAX_TOKENS: u32 = 3_000;

/// Prompt and instruction headroom reserved on top of the document allowance
/// when the compaction input budget is computed.
pub const COMPACTION_PROMPT_RESERVE_TOKENS: u32 = 1_000;

/// Cap `configured_max` so output cannot consume more than 20% of `context_window`.
///
/// A zero window means "unknown"; preserve the configured value rather than
/// inventing a capacity. Non-zero windows use integer arithmetic deliberately:
/// rounding down leaves the extra fraction to input headroom.
pub(crate) fn bounded_output_tokens(configured_max: u32, context_window: u32) -> u32 {
    if context_window == 0 {
        return configured_max;
    }
    configured_max.min(context_window.saturating_mul(OUTPUT_WINDOW_PERCENT) / 100)
}
