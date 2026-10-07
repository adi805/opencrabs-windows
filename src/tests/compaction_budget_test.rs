//! Tests for `agent::service::compaction_budget`.
//!
//! Regression context: the continuation document is the woken agent's entire
//! memory, and it was unbounded in both directions.
//!
//! - #1930 — the summariser inherited the chat output allowance (40 000 tokens
//!   on a 200K route), the prompt ordered the document to be exhaustive, and
//!   nothing trimmed the result. Measured markers ran to a 27.8 KB mean.
//! - #1933 — the guard that was meant to catch an oversized document only
//!   inspected documents it had already decided were over budget, so a
//!   document truncated at the cap (which is exactly budget-sized) was never
//!   checked for its tail.
//!
//! What must not drift:
//!
//! - The document allowance stays far below the chat allowance.
//! - The required-section check runs on every document, including one that
//!   fits — a missing §9 is reported either way.
//! - A trim never removes §0 or §9, and never cuts a fence in half.

use crate::brain::agent::service::compaction_budget::{
    REQUIRED_SECTIONS, enforce_summary_budget, missing_required_sections,
};
use crate::brain::agent::service::request_budget::{
    COMPACTION_SUMMARY_MAX_TOKENS, bounded_output_tokens,
};

/// Enough filler to push a document well past the budget.
fn filler(units: usize) -> String {
    "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(units / 8)
}

fn well_formed() -> String {
    format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\nCONTINUE THIS TASK: ship it.\n\n\
         ## 1. Chronological Analysis\n{}\n\n\
         ## 8. Next Step\nRun the tests.\n\n\
         ## 9. Continuation Message\nBack on it — picking up the trim.\n",
        filler(20)
    )
}

fn fence_lines(text: &str) -> usize {
    text.lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
}

#[test]
fn compaction_allowance_is_far_below_the_chat_allowance() {
    let chat_allowance = bounded_output_tokens(65_536, 200_000);
    assert_eq!(chat_allowance, 40_000);
    assert!(
        COMPACTION_SUMMARY_MAX_TOKENS * 4 < chat_allowance,
        "the document allowance must stay a small fraction of the chat allowance; \
         got {COMPACTION_SUMMARY_MAX_TOKENS} against {chat_allowance}"
    );
}

#[test]
fn well_formed_document_reports_nothing_missing() {
    assert_eq!(missing_required_sections(&well_formed()), Vec::<u32>::new());
}

#[test]
fn truncated_document_reports_the_missing_tail() {
    let truncated = "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\nDo the thing.\n\n\
                     ## 1. Chronological Analysis\nIt was going fine until\n";
    let missing = missing_required_sections(truncated);
    assert_eq!(
        missing,
        vec![9],
        "a document cut before its tail must name §9 as missing; got {missing:?}"
    );
}

#[test]
fn a_document_with_no_sections_at_all_reports_both_required_missing() {
    assert_eq!(
        missing_required_sections("just prose, no headings"),
        REQUIRED_SECTIONS.to_vec()
    );
}

#[test]
fn under_budget_document_is_returned_unchanged() {
    let doc = well_formed();
    assert_eq!(enforce_summary_budget(&doc), doc);
}

#[test]
fn code_fences_are_spent_before_whole_sections() {
    let code = format!("```rust\n{}\n```", filler(3_200));
    let doc = format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\nShip it.\n\n\
         ## 1. Chronological Analysis\nThe change lives here:\n{code}\n\
         Tail prose after the snippet.\n\n\
         ## 9. Continuation Message\nBack on it.\n"
    );
    let out = enforce_summary_budget(&doc);
    assert!(
        out.contains("[code snippet omitted"),
        "the fence must be spent before any section is dropped; got:\n{out}"
    );
    assert!(
        out.contains("## 1. Chronological Analysis"),
        "§1 survives while spending the fence is enough; got:\n{out}"
    );
    assert!(out.contains("Tail prose after the snippet."));
}

#[test]
fn protected_sections_survive_a_trim_that_drops_everything_spendable() {
    let doc = format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\nShip it.\n\n\
         ## 1. Chronological Analysis\n{}\n\n\
         ## 2. Files Modified\n{}\n\n\
         ## 3. User Preferences\n{}\n\n\
         ## 4. Errors\n{}\n\n\
         ## 5. All User Messages\n{}\n\n\
         ## 6. Pending Tasks\n{}\n\n\
         ## 9. Continuation Message\nBack on it.\n",
        filler(2_000),
        filler(2_000),
        filler(2_000),
        filler(2_000),
        filler(2_000),
        filler(2_000),
    );
    let out = enforce_summary_budget(&doc);
    assert!(
        out.contains("## 0. IMMEDIATE TASK"),
        "§0 is never dropped:\n{out}"
    );
    assert!(
        out.contains("## 9. Continuation Message"),
        "§9 is never dropped:\n{out}"
    );
    assert!(
        !out.contains("## 6. Pending Tasks"),
        "the narrative tail is spent first:\n{out}"
    );
}

#[test]
fn a_fence_inside_a_protected_section_survives_intact() {
    let doc = format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\nShip it.\n```rust\nlet keep = true;\n```\n\n\
         ## 1. Chronological Analysis\n{}\n\n\
         ## 9. Continuation Message\nBack on it.\n",
        filler(2_000),
    );
    let out = enforce_summary_budget(&doc);
    assert!(
        out.contains("let keep = true;"),
        "a protected section is never spent, fence and all:\n{out}"
    );
    assert!(
        fence_lines(&out).is_multiple_of(2),
        "every surviving fence must be balanced — a fence cut in half is worse \
         than a dropped one:\n{out}"
    );
}

#[test]
fn an_unterminated_fence_does_not_swallow_the_rest_of_its_section() {
    let doc = format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\nShip it.\n\n\
         ## 1. Chronological Analysis\nHere is a malformed snippet:\n```rust\nlet x = 1;\n\
         ## 2. Files Modified\n{}\n\n\
         ## 9. Continuation Message\nBack on it.\n",
        filler(3_000),
    );
    let out = enforce_summary_budget(&doc);
    assert!(
        out.contains("Here is a malformed snippet:"),
        "an unterminated fence is malformed already; dropping the rest of the \
         block would lose text rather than shorten it:\n{out}"
    );
    assert!(
        out.contains("let x = 1;"),
        "the snippet itself is kept:\n{out}"
    );
}

#[test]
fn a_document_without_sections_is_left_alone() {
    let doc = filler(4_000);
    assert_eq!(
        enforce_summary_budget(&doc),
        doc,
        "with no heading to split on there is no structural trim, and losing text \
         would be worse than an over-budget document"
    );
}
