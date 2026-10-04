//! The compaction continuation document has ONE budget, shared by both sides (#1930).
//!
//! Observed 2026-10-04 (ops profile, live): the summariser request was allowed
//! `bounded_output_tokens(65_536, 200_000)` = 40 000 output tokens while the
//! input budget reserved a hard-coded `8_000 + 1_000` = 9 000 for it — a 4.44x
//! disagreement — and no code path trimmed the result. 208 of the 210 live
//! markers written that day exceeded a 3 000-token budget (mean 27.8 KB,
//! max 69.2 KB), and the worst session compacted 59 times in one day.
//!
//! Both sides now derive from `COMPACTION_SUMMARY_MAX_TOKENS`. This test is the
//! drift guard: it fails if either side re-acquires a literal, which is the one
//! edit that silently restores the 4.44x disagreement.

use crate::brain::agent::service::request_budget::{
    compaction_summary_input_reserve, compaction_summary_output_tokens,
    COMPACTION_SUMMARY_MAX_TOKENS,
};

/// The input reserve must COVER the output allowance.
///
/// This is the incident inverted: 9 000 reserved against 40 000 allowed.
#[test]
fn the_input_reserve_covers_the_output_allowance() {
    let reserve = compaction_summary_input_reserve();
    let allowance = compaction_summary_output_tokens() as usize;
    assert!(
        reserve > allowance,
        "input reserve ({reserve}) must exceed the summariser's output allowance \
         ({allowance}): a reserve smaller than the allowance hands the summariser \
         more input than the context window can hold (#1930)"
    );
}

/// The reserve is the budget plus prompt headroom, and nothing else.
#[test]
fn the_reserve_is_derived_from_the_one_constant() {
    assert_eq!(
        compaction_summary_output_tokens(),
        COMPACTION_SUMMARY_MAX_TOKENS,
        "the summariser's allowance must BE the shared constant"
    );
    let headroom = compaction_summary_input_reserve() - compaction_summary_output_tokens() as usize;
    assert_eq!(
        headroom, 1_000,
        "the reserve must be the summary budget plus the prompt headroom"
    );
}

/// Both call sites reference the shared helpers, and neither has a literal left.
#[test]
fn both_call_sites_derive_from_the_constant() {
    let context_src = include_str!("../brain/agent/service/context.rs");
    let compaction_src = include_str!("../brain/agent/service/compaction.rs");

    assert!(
        context_src.contains("compaction_summary_input_reserve()"),
        "context.rs must size the input budget from the shared reserve helper"
    );
    assert!(
        !context_src.contains("8_000usize"),
        "context.rs must not carry a hard-coded output reserve literal (#1930)"
    );
    assert!(
        context_src.contains("compaction_summary_output_tokens()"),
        "the manual /compact call site must pass the shared output budget"
    );
    assert!(
        compaction_src.contains("compaction_summary_output_tokens()"),
        "the background compaction call site must pass the shared output budget"
    );
}
