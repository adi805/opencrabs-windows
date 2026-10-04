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

use crate::brain::agent::service::context::parse_context_manifest;
use crate::brain::agent::service::request_budget::{
    compaction_summary_input_reserve, compaction_summary_output_tokens,
    COMPACTION_SUMMARY_MAX_TOKENS,
};
use crate::brain::agent::service::AgentService;

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

/// The rendered directive states the budget AND the order the budget is spent in.
///
/// Naming the size is not enough on its own: a budget with no retention order
/// lets the model spend it on whichever section it writes first, which is how
/// the load-bearing blocks (obligation status, manifest fence) get crowded out.
#[test]
fn the_rendered_directive_states_the_budget_and_the_retention_order() {
    let directive = AgentService::compaction_budget_directive(COMPACTION_SUMMARY_MAX_TOKENS);

    assert!(
        directive.contains(&COMPACTION_SUMMARY_MAX_TOKENS.to_string()),
        "the directive must state the token budget it enforces; got: {directive}"
    );
    assert!(
        directive.contains("context-manifest"),
        "the directive must name the mandatory context-manifest fence as a MUST-keep block"
    );
    assert!(
        directive.contains("FIRST TO GO"),
        "the directive must name what is dropped first when the budget is tight"
    );
    // The blocks a woken agent cannot work without must be named as MUST-keep,
    // not left to the model's judgement of what matters.
    for block in ["section 0", "section 8", "section 7"] {
        assert!(
            directive.contains(block),
            "the directive must name {block} among the MUST-keep blocks; got: {directive}"
        );
    }
}

/// The summariser prompt no longer orders maximum verbosity.
///
/// This is the incident's own wording, and it is what a 27.8 KB mean against a
/// 3 000-token budget was obeying.
#[test]
fn the_prompt_no_longer_orders_maximum_verbosity() {
    let system_prompt = AgentService::compaction_system_prompt();
    assert!(
        !system_prompt.contains("verbose"),
        "the summariser system prompt must not invite verbosity (#1930): {system_prompt}"
    );

    let context_src = include_str!("../brain/agent/service/context.rs");
    assert!(
        !context_src.contains("BE EXHAUSTIVE"),
        "the continuation-document prompt must not order maximum size (#1930)"
    );
}

// ---------------------------------------------------------------------------
// The post-generation trim guard (#1930, part 2).
//
// The prompt now ASKS for a 3 000-token document. Asking is not enforcing: on
// 2026-10-04 the live markers read a mean of 27 831 B (max 69 154 B, 208 of 210
// over budget), because a 40 000-token output allowance sat against a 9 000
// -token input reserve and nothing trimmed the result. `enforce_summary_budget`
// is that trimmer, and these tests pin its contract.
// ---------------------------------------------------------------------------

/// A tight budget for the fixture below: large enough that the must-keep blocks
/// fit, small enough that the bulk sections cannot.
///
/// Sized with margin rather than near the trimmed result: the fixture is
/// ~33.5 KB (~8 300 tokens), the guard trims it to ~1.4 KB (~360 tokens), and
/// the assertion is that it lands UNDER the budget. Pinning the budget at the
/// trimmed size would make the test a coin-flip on the tokenizer's exact ratio
/// (a red gate costs a whole CI cycle); 1 000 still forces every bulk section
/// out, so the guard is exercised exactly as intended.
const TIGHT_BUDGET: usize = 1_000;

/// A document shaped like the real one: small load-bearing blocks, large bulk.
fn oversized_summary() -> String {
    let bulk = |label: &str| {
        format!(
            "## {label}\n{}",
            "the summariser recorded this in great detail, at length, twice over, \
             with file paths, line numbers and quoted code, because it was told to be \
             exhaustive and nothing ever trimmed it. "
                .repeat(30)
        )
    };

    format!(
        "CRITICAL: The context window is at 91% capacity.\n\
         You are creating a COMPREHENSIVE CONTINUATION DOCUMENT.\n\n\
         ## 0. IMMEDIATE TASK (CRITICAL — MOST IMPORTANT SECTION)\n\
         **Obligation status: OPEN**\n\
         CONTINUE THIS TASK: finish the trim guard. Do NOT deviate.\n\n\
         {}\n{}\n{}\n{}\n{}\n{}\n\n\
         ## 7. Recovery Playbook\n\
         Run `git log --oneline -3` and read the plan file.\n\n\
         ## 8. Next Step\n\
         Add the guard, then commit it.\n\n\
         ## 9. Continuation Message\n\
         Picking the guard back up right now.\n\n\
         ## 10. Context Manifest (MANDATORY MACHINE-READABLE BLOCK)\n\
         ```context-manifest\n\
         active_skills:\n  - opencrabs-dev/editor.md\n\
         discard_skills:\n  - meta-factory\n\
         required_tools:\n  - plan\n\
         ```\n",
        bulk("1. Chronological Analysis"),
        bulk("2. Files Modified"),
        bulk("3. User Preferences & Constraints"),
        bulk("4. Errors & Corrections"),
        bulk("5. All User Messages"),
        bulk("6. Pending Tasks"),
    )
}

/// AC2 — a forced compaction under a tight budget keeps the two blocks a woken
/// agent cannot recover from anywhere else: the §0 obligation status and a
/// parseable `context-manifest` fence.
#[test]
fn a_trimmed_document_keeps_its_obligation_status_and_manifest_fence() {
    let oversized = oversized_summary();
    let before = crate::brain::tokenizer::count_tokens(&oversized);
    assert!(
        before > TIGHT_BUDGET,
        "the fixture must actually exceed the tight budget ({before} vs {TIGHT_BUDGET}), \
         or the guard is never exercised and this test proves nothing"
    );

    let trimmed = AgentService::enforce_summary_budget(oversized.clone(), TIGHT_BUDGET);
    let after = crate::brain::tokenizer::count_tokens(&trimmed);

    assert!(
        after < before,
        "the guard must drop content when the document is over budget ({before} -> {after})"
    );
    assert!(
        after <= TIGHT_BUDGET,
        "the trimmed document must fit the budget it was given ({after} > {TIGHT_BUDGET}):\n{trimmed}"
    );
    assert!(
        parse_context_manifest(&trimmed).is_some(),
        "the trimmed document must still carry a parseable context-manifest fence:\n{trimmed}"
    );
    assert!(
        trimmed.to_lowercase().contains("obligation status"),
        "the trimmed document must still carry the §0 obligation-status line:\n{trimmed}"
    );
    // And the bulk really was what went — a guard that keeps everything and
    // merely rewrites whitespace would satisfy none of the above.
    assert!(
        !trimmed.contains("## 1. Chronological Analysis\nthe summariser recorded"),
        "a bulk section's body must be dropped whole, not left in place:\n{trimmed}"
    );
}

/// A document that already fits is returned byte-for-byte: the guard is a
/// no-op on the normal path, so it cannot be blamed for reshaping summaries.
#[test]
fn a_document_within_budget_is_returned_unchanged() {
    let doc = "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n\n\
               ## 10. Context Manifest\n```context-manifest\n\
               active_skills:\n  - opencrabs-dev\n```\n";
    let out = AgentService::enforce_summary_budget(doc.to_string(), COMPACTION_SUMMARY_MAX_TOKENS as usize);
    assert_eq!(out, doc, "an under-budget document must pass through untouched");
}

/// The guard ships an over-budget document rather than cut the obligation out.
///
/// When the must-keep blocks alone exceed the budget there is nothing safe to
/// trim: silently deleting §0 would leave the woken agent with no task at all,
/// which is strictly worse than a large document. The guard must WARN and keep
/// them (log line `must-keep sections alone are N tokens`).
#[test]
fn the_guard_keeps_the_obligation_rather_than_trim_it() {
    let doc = format!(
        "## 0. IMMEDIATE TASK\n**Obligation status: OPEN**\n{}\n\n\
         ## 10. Context Manifest\n```context-manifest\n\
         active_skills:\n  - opencrabs-dev\n```\n",
        "CONTINUE THIS TASK: keep going. ".repeat(400)
    );
    let out = AgentService::enforce_summary_budget(doc, 100);

    assert!(
        out.to_lowercase().contains("obligation status"),
        "the obligation status must survive even when it alone busts the budget"
    );
    assert!(
        parse_context_manifest(&out).is_some(),
        "the manifest fence must survive even when the budget cannot be met"
    );
}

/// The guard runs BEFORE the #482 artifact correction, inside the summariser.
///
/// Order matters: the correction is appended after trimming, so a correction can
/// never be trimmed away — and a correction that was trimmed would be the exact
/// silent-failure mode #482 exists to prevent.
#[test]
fn the_guard_runs_before_the_artifact_correction() {
    let src = include_str!("../brain/agent/service/context.rs");
    let fn_start = src
        .find("pub(super) async fn compute_compaction_summary(")
        .expect("compute_compaction_summary must exist");
    let guard = src
        .find("Self::enforce_summary_budget(")
        .expect("compute_compaction_summary must call the budget guard (#1930)");
    let correction = src
        .find("Self::flag_unbacked_artifacts(summary, &snapshot_messages)")
        .expect("the #482 artifact correction must still run");

    assert!(
        fn_start < guard,
        "the guard call must live inside compute_compaction_summary"
    );
    assert!(
        guard < correction,
        "the budget guard must run BEFORE the #482 artifact correction, so a \
         correction is never trimmed away (#1930)"
    );
}
