//! Regression guards for the deterministic Sources footer (#1883).
//!
//! A search-backed answer must carry its source links even when the model
//! cites nothing, and the wiring that makes that true is structural: the
//! tool loop harvests search-tool outputs into `turn_search_outputs` and
//! appends the footer to the delivered text. These guards pin that wiring
//! the same way `discord_followup_tap_tool_loop_test.rs` pins #1852.
//!
//! The behavioural contract — harvest, cross-engine dedup, cap, drop URLs
//! already linked in the answer, no-op when no search tool ran — is
//! unit-tested in `src/brain/tools/sources_footer.rs`.

/// The tool loop must run the delivered text through the footer builder.
#[test]
fn tool_loop_appends_the_sources_footer() {
    let loop_src = include_str!("../brain/agent/service/tool_loop.rs");
    assert!(
        loop_src.contains("sources_footer::append_sources_footer("),
        "#1883: the delivered answer must run through append_sources_footer"
    );
}

/// Harvesting must be gated on the search-tool name, never on output shape.
#[test]
fn tool_loop_harvests_only_search_tool_outputs() {
    let loop_src = include_str!("../brain/agent/service/tool_loop.rs");
    assert!(
        loop_src.contains("sources_footer::is_search_tool("),
        "#1883: harvesting must be gated on the search-tool name"
    );
    assert!(
        loop_src.contains("turn_search_outputs"),
        "#1883: harvested search outputs must land in turn_search_outputs"
    );
}

/// The parallel batch must attribute each output to its tool, or the
/// name-gated harvest above cannot see which results are search hits.
#[test]
fn parallel_batch_carries_tool_names() {
    let parallel_src = include_str!("../brain/agent/service/parallel_tools.rs");
    assert!(
        parallel_src.contains("pub names: Vec<String>"),
        "#1883: ParallelBatchOutcome must carry per-output tool names"
    );
}
