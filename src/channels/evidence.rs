//! Mechanical evidence footer for a turn's final answer (FR-007, #1880).
//!
//! Shared by Discord and Telegram so the wording, the cap, and the
//! phantom-safety property cannot drift between the two surfaces
//! (NFR-002, NFR-003).
//!
//! The input is a list of tool NAMES taken from what each channel already
//! collected out of `ProgressEvent::ToolStarted` — real executions in the
//! tool loop. Nothing here reads model output, so the line can never name a
//! tool the turn did not run (AC-014, AC-015).
//!
//! Callers append the line at the CHANNEL layer, after the agent returned.
//! The phantom gate inspects the model's own output, so it never sees this
//! text (AC-016). Belt and braces: [`EVIDENCE_HEADER`] is pinned by test
//! against every language's `executed_framings`.

/// Header of the evidence footer. Kept as a constant so the phantom-safety
/// test pins the SAME string the renderers emit, instead of a copy that can
/// drift (AC-016).
pub(crate) const EVIDENCE_HEADER: &str = "🔎 evidence:";

/// Distinct tool names in first-seen order, rendered as the footer.
///
/// `None` when the turn ran no tools: an empty or invented line is worse
/// than no line at all (AC-015).
pub(crate) fn evidence_line<'a, I>(names: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    // A turn that reads the same file four times is still ONE tool in the
    // evidence line: repetition is not evidence of breadth.
    let mut uniq: Vec<&str> = Vec::new();
    for n in names {
        if !uniq.contains(&n) {
            uniq.push(n);
        }
    }
    if uniq.is_empty() {
        return None;
    }
    // Cap the list: a 20-tool turn must not turn its footer into a wall.
    const SHOWN: usize = 6;
    let extra = uniq.len().saturating_sub(SHOWN);
    let shown: Vec<&str> = uniq.iter().copied().take(SHOWN).collect();
    let mut line = format!("{EVIDENCE_HEADER} {}", shown.join(", "));
    if extra > 0 {
        line.push_str(&format!(" +{extra} more"));
    }
    Some(line)
}
