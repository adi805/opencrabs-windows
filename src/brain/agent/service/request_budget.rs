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

/// The compaction continuation DOCUMENT budget, in tokens.
///
/// This is the size the continuation document *should* be, and the value
/// `enforce_summary_budget` trims to. It is deliberately NOT the summariser
/// request's output allowance: on providers that bill hidden reasoning inside
/// `max_output_tokens`, a request allowed only this much comes back CUT at this
/// size, with the reasoning counted inside it (#1933). The request is allowed
/// `compaction_summary_request_allowance()` instead — the document budget plus
/// a reasoning allowance — and the guard trims the result back to this budget.
///
/// The request allowance and the input-side `output_reserve` MUST derive from
/// the SAME arithmetic. When they disagree the request reserves less room than
/// the call is allowed to consume, and nothing trims the result: measured
/// 2026-10-04 the allowance was `bounded_output_tokens(65_536, 200_000)` =
/// 40 000 tokens while the reserve was a hard-coded 9 000 (4.44x), and 208 of
/// 210 live markers exceeded this budget (mean 27.8 KB, max 69.2 KB).
/// See opencrabs/opencrabs#1930.
pub(crate) const COMPACTION_SUMMARY_MAX_TOKENS: u32 = 3_000;

/// Reasoning tokens the summariser may burn INSIDE its output allowance.
///
/// On providers that bill hidden reasoning inside `max_output_tokens`, the
/// allowance bounds reasoning + visible text TOGETHER. Measured 2026-10-04
/// (#1933): a 3 000-token allowance returned `usage_output=3000,
/// usage_reasoning=1842`, so the visible document was cut to ~1 158 tokens and
/// lost its §10 fence before it was ever persisted. Sized above that
/// measurement with margin, so a full document still fits once the reasoning is
/// paid for.
pub(crate) const COMPACTION_SUMMARY_REASONING_HEADROOM_TOKENS: u32 = 2_500;

/// Headroom the summariser *prompt* needs on top of its output allowance (~1k tokens).
const COMPACTION_PROMPT_HEADROOM_TOKENS: usize = 1_000;

/// The continuation DOCUMENT budget, in tokens.
///
/// Single source for the size the document should be and the value the trim
/// guard enforces. NOT the request's output allowance — see
/// `compaction_summary_request_allowance()`.
pub(crate) fn compaction_summary_output_tokens() -> u32 {
    COMPACTION_SUMMARY_MAX_TOKENS
}

/// The summariser REQUEST's output allowance, in tokens.
///
/// The document budget PLUS a reasoning allowance: this is what is passed to
/// `LLMRequest::with_max_tokens`, and it is deliberately LARGER than the
/// document budget because reasoning is billed inside the allowance (#1933).
/// A generous request allowance is safe precisely BECAUSE
/// `enforce_summary_budget` trims the result to the DOCUMENT budget — #1930's
/// protection is intact.
///
/// Single source for BOTH call sites (background `compaction.rs`, manual
/// `/compact` in `context.rs`) so they cannot drift apart again.
pub(crate) fn compaction_summary_request_allowance() -> u32 {
    COMPACTION_SUMMARY_MAX_TOKENS + COMPACTION_SUMMARY_REASONING_HEADROOM_TOKENS
}

/// Room the input budget must reserve for the summariser call: its output
/// allowance plus the prompt headroom.
///
/// `context.rs` subtracts this from the snapshot's context window to size the
/// messages sent to the summariser. Deriving it from the same arithmetic is the
/// whole point: a reserve smaller than the allowance means the summariser is
/// handed more input than the window can hold.
pub(crate) fn compaction_summary_input_reserve() -> usize {
    compaction_summary_request_allowance() as usize + COMPACTION_PROMPT_HEADROOM_TOKENS
}
