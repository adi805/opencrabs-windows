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

/// Output budget for the compaction continuation document.
///
/// The summariser's own `max_output_tokens` and the input-side `output_reserve`
/// MUST both derive from this one value. When they disagree the request reserves
/// less room than the call is allowed to consume, and nothing trims the result:
/// measured 2026-10-04 the allowance was `bounded_output_tokens(65_536, 200_000)`
/// = 40 000 tokens while the reserve was a hard-coded 9 000 (4.44x), and 208 of
/// 210 live markers exceeded this budget (mean 27.8 KB, max 69.2 KB).
/// See opencrabs/opencrabs#1930.
pub(crate) const COMPACTION_SUMMARY_MAX_TOKENS: u32 = 3_000;

/// Headroom the summariser *prompt* needs on top of its output budget (~1k tokens).
const COMPACTION_PROMPT_HEADROOM_TOKENS: usize = 1_000;

/// The summariser call's output allowance, in tokens.
///
/// Single source for BOTH call sites (background `compaction.rs`, manual
/// `/compact` in `context.rs`) so they cannot drift apart again.
pub(crate) fn compaction_summary_output_tokens() -> u32 {
    COMPACTION_SUMMARY_MAX_TOKENS
}

/// Room the input budget must reserve for the summariser call: its output
/// allowance plus the prompt headroom.
///
/// `context.rs` subtracts this from the snapshot's context window to size the
/// messages sent to the summariser. Deriving it from the same constant is the
/// whole point: a reserve smaller than the allowance means the summariser is
/// handed more input than the window can hold.
pub(crate) fn compaction_summary_input_reserve() -> usize {
    COMPACTION_SUMMARY_MAX_TOKENS as usize + COMPACTION_PROMPT_HEADROOM_TOKENS
}
