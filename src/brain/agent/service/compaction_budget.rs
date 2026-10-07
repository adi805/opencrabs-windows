//! Post-generation budget guard for the compaction continuation document.
//!
//! The continuation document is the woken agent's *entire memory*, so its size
//! is load-bearing in both directions:
//!
//! - **Too large (#1930).** The summariser prompt ordered the document to be
//!   exhaustive, the request allowed the full chat output ceiling, and nothing
//!   trimmed the result. Measured markers ran to a 27.8 KB mean / 69 KB max,
//!   with 99 % of them above a 3 000-token target — a document 2-4x its useful
//!   size re-bills the whole window every round and lands the session near
//!   where it started.
//! - **Cut off (#1933).** A document that hits the output cap loses its tail
//!   silently, because the sections the fresh agent resumes from are written
//!   last. The guard that was supposed to catch it only inspected documents it
//!   had already decided were over budget, so the one failure mode that removes
//!   the tail was the one it never looked at.
//!
//! Two rules, one place:
//!
//! 1. The budget is [`COMPACTION_SUMMARY_MAX_TOKENS`] — the same constant the
//!    summariser request and the input-side reserve are built from, so the
//!    allowance and the reserve cannot drift apart.
//! 2. The required-section check runs on **every** document, over budget or
//!    not. A capped document is exactly budget-sized, so a check that lives
//!    inside the trim branch is blind by construction.

use crate::brain::agent::context::AgentContext;
use crate::brain::agent::service::request_budget::COMPACTION_SUMMARY_MAX_TOKENS;

/// Sections the document must carry: §0 (the obligation directive the fresh
/// agent reads first) and §9 (the continuation message it speaks). They bracket
/// the document, so losing either one means the tail was cut.
pub const REQUIRED_SECTIONS: [u32; 2] = [0, 9];

/// Sections whose content may be spent when the document is over budget,
/// highest first — the narrative tail goes before the narrative head, and the
/// load-bearing sections are never in this list.
const SPENDABLE_SECTIONS: [u32; 6] = [6, 5, 4, 3, 2, 1];

/// Placeholder left where a fenced block was dropped.
const FENCE_PLACEHOLDER: &str = "[code snippet omitted — compaction budget]\n";

/// Section numbers missing from `summary`, in [`REQUIRED_SECTIONS`] order.
///
/// Pure so the caller can warn and a test can assert without a subscriber.
pub fn missing_required_sections(summary: &str) -> Vec<u32> {
    let present: Vec<u32> = split_sections(summary)
        .iter()
        .filter_map(|block| section_number(block))
        .collect();
    REQUIRED_SECTIONS
        .into_iter()
        .filter(|required| !present.contains(required))
        .collect()
}

/// Enforce the continuation-document budget on `summary`.
///
/// Always checks the required sections and warns when one is missing — that
/// check is deliberately outside the over-budget branch (#1933). An over-budget
/// document is trimmed structurally: fenced code blocks in the narrative
/// sections go first (the prompt's own spend order), then whole narrative
/// sections, tail first. Protected sections and code fences that survive are
/// never cut mid-block.
pub fn enforce_summary_budget(summary: &str) -> String {
    let budget = COMPACTION_SUMMARY_MAX_TOKENS as usize;
    let before = AgentContext::estimate_tokens(summary);

    for missing in missing_required_sections(summary) {
        tracing::warn!(
            "Compaction summary is missing required section §{missing} — the document was \
             truncated before its tail ({} chars, {before} tokens)",
            summary.len(),
        );
    }

    if before <= budget {
        return summary.to_string();
    }

    let mut sections = split_sections(summary);
    spend_code_fences(&mut sections);
    let sections = spend_sections(sections, budget);

    let trimmed: String = sections.concat();
    let after = AgentContext::estimate_tokens(&trimmed);
    if after > budget {
        tracing::warn!(
            "Compaction summary trimmed: {before} → {after} tokens, still over the {budget}-token \
             budget — only protected sections remain and they are never dropped"
        );
    } else {
        tracing::warn!("Compaction summary trimmed: {before} → {after} tokens (budget {budget})");
    }
    trimmed
}

/// Drop fenced code blocks from the spendable sections, keeping the fences that
/// stay balanced. A section whose fences never close is left alone: an
/// unterminated fence is already malformed, and dropping the rest of the block
/// would lose text rather than shorten it.
fn spend_code_fences(sections: &mut [String]) {
    for block in sections.iter_mut() {
        if is_spendable(block) {
            *block = strip_code_fences(block);
        }
    }
}

/// Drop whole spendable sections, highest number first, until the document fits.
fn spend_sections(mut sections: Vec<String>, budget: usize) -> Vec<String> {
    for number in SPENDABLE_SECTIONS {
        let joined: String = sections.concat();
        if AgentContext::estimate_tokens(&joined) <= budget {
            return sections;
        }
        sections.retain(|block| section_number(block) != Some(number));
    }
    sections
}

/// Replace each balanced fenced block in `block` with [`FENCE_PLACEHOLDER`].
fn strip_code_fences(block: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    let mut dropped = 0usize;
    for line in block.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            if in_fence {
                in_fence = false;
                dropped += 1;
                out.push_str(FENCE_PLACEHOLDER);
            } else {
                in_fence = true;
            }
            continue;
        }
        if !in_fence {
            out.push_str(line);
        }
    }
    if in_fence || dropped == 0 {
        return block.to_string();
    }
    out
}

/// Split a document into the preamble plus one block per `## ` heading.
///
/// Splitting on headings is what makes the trim fence-safe: a whole section is
/// kept or dropped, so a fence inside a kept section is never cut in half.
fn split_sections(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if is_heading(line) && !current.is_empty() {
            blocks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn is_heading(line: &str) -> bool {
    line.trim_start().starts_with("## ")
}

fn is_spendable(block: &str) -> bool {
    section_number(block).is_some_and(|n| SPENDABLE_SECTIONS.contains(&n))
}

/// Section number of a block, read from its heading.
///
/// Accepts `## 9.`, `## 9:`, `## 9 Title` — anything where the digits end at a
/// non-digit — so a model that reformats the heading is still recognised.
fn section_number(block: &str) -> Option<u32> {
    let first = block.lines().next()?;
    let rest = first.trim_start().strip_prefix("##")?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}
