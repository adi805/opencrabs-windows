//! Behavioral pins for the deterministic Sources footer (#1883).
//! Fixtures are rendered with the EXACT line shapes each engine emits:
//! DuckDuckGo `   🔗 {url}` (web_search.rs), exa/brave/serper `   URL: {url}`
//! (exa_search.rs, brave_search.rs, serper_search.rs).

use crate::brain::tools::sources_footer::{
    MAX_SOURCES, footer_for, harvest_search_sources, is_search_tool,
};

fn ddg_output() -> String {
    "🌐 2 web results:\n\n## DuckDuckGo (rust async)\n\n1. First DDG hit\n   🔗 https://example.com/a\n\n2. Second DDG hit\n   🔗 https://example.com/b\n\n"
        .to_string()
}

fn exa_output() -> String {
    "🧠 2 results from EXA (query: rust async)\n\n1. Exa top hit\n   URL: https://example.com/a\n\n   Why: vector match on the query\n2. Another exa hit\n   URL: https://example.com/c\n"
        .to_string()
}

fn brave_output() -> String {
    "🦁 Brave results (query: rust async, 2 found):\n\n1. Brave result one\n   URL: https://example.com/d\n2. Brave case and slash variant\n   URL: https://EXAMPLE.com/C/\n"
        .to_string()
}

fn serper_output() -> String {
    "🔎 Serper results (query: rust async, 3 organic):\n\n1. Serper organic\n   URL: https://example.com/e\n2. Serper second\n   URL: https://example.com/f\n3. Serper third\n   URL: https://example.com/g\n"
        .to_string()
}

fn all_engines() -> Vec<String> {
    vec![ddg_output(), exa_output(), brave_output(), serper_output()]
}

#[test]
fn collector_parses_both_render_formats_across_all_four_engines() {
    let sources = harvest_search_sources(&all_engines());
    let urls: Vec<&str> = sources.iter().map(|(_, u)| u.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            "https://example.com/a",
            "https://example.com/b",
            "https://example.com/c",
            "https://example.com/d",
            "https://example.com/e",
            "https://example.com/f",
            "https://example.com/g",
        ],
        "both `🔗` and `URL:` lines must harvest, in engine-result order"
    );
    assert_eq!(
        sources[0].0.as_deref(),
        Some("First DDG hit"),
        "the numbered line before a URL becomes the link title"
    );
}

#[test]
fn cross_engine_same_url_dedups_with_first_engine_winning() {
    // https://example.com/a appears as a DDG `🔗` line AND an exa `URL:` line.
    let sources = harvest_search_sources(&all_engines());
    let a_hits = sources
        .iter()
        .filter(|(_, u)| u == "https://example.com/a")
        .count();
    assert_eq!(a_hits, 1, "a URL returned by two engines lists once");
    assert_eq!(
        sources[0].0.as_deref(),
        Some("First DDG hit"),
        "first occurrence wins, so the earlier engine's title is kept"
    );
}

#[test]
fn dedup_matches_case_and_trailing_slash_like_engine_fanout() {
    // exa emits https://example.com/c, brave emits https://EXAMPLE.com/C/.
    let sources = harvest_search_sources(&all_engines());
    let c_hits = sources
        .iter()
        .filter(|(_, u)| u.to_lowercase().trim_end_matches('/') == "https://example.com/c")
        .count();
    assert_eq!(
        c_hits, 1,
        "normalization must be case- and trailing-slash-insensitive, like #1731's dedup"
    );
    let kept = sources
        .iter()
        .find(|(_, u)| u.to_lowercase().contains("/c"))
        .expect("one /c entry survives dedup");
    assert_eq!(
        kept.0.as_deref(),
        Some("Another exa hit"),
        "the exa (first) variant survives, not the brave variant"
    );
}

#[test]
fn footer_caps_at_five_fresh_links_in_engine_order() {
    let footer =
        footer_for("A plain answer with no links at all.", &all_engines()).expect("footer present");
    let links: Vec<&str> = footer.lines().skip(1).collect();
    assert_eq!(links.len(), MAX_SOURCES, "footer is bounded at five links");
    assert!(links[0].contains("https://example.com/a"));
    assert!(links[MAX_SOURCES - 1].contains("https://example.com/e"));
    assert!(
        !footer.contains("https://example.com/f") && !footer.contains("https://example.com/g"),
        "cap keeps engine order: e is the fifth fresh link, f and g are cut"
    );
}

#[test]
fn footer_drops_urls_the_answer_already_links() {
    let answer =
        "Rust async works via tokio, see [a](https://example.com/a) and https://example.com/c.";
    let footer = footer_for(answer, &all_engines()).expect("footer present");
    assert!(
        !footer.contains("https://example.com/a"),
        "a source the answer already links must not repeat in the footer"
    );
    assert!(
        !footer.contains("https://example.com/c") && !footer.contains("https://EXAMPLE.com/C/"),
        "the cited c variant must not duplicate. got: {footer}"
    );
    assert!(footer.contains("https://example.com/b"));
    assert!(footer.contains("https://example.com/d"));
}

#[test]
fn no_search_turn_and_fully_cited_answer_stay_byte_identical() {
    // No search tools ran: no footer.
    assert_eq!(
        footer_for("Just an answer from memory.", &[]),
        None,
        "a turn with no search-tool output must get no footer"
    );
    // Every harvested source is already in the answer: no footer either.
    let answer = "Links: https://example.com/a https://example.com/b https://example.com/c \
                  https://example.com/d https://example.com/e https://example.com/f \
                  https://example.com/g";
    assert_eq!(
        footer_for(answer, &all_engines()),
        None,
        "an answer that already cites everything gets no footer"
    );
}

#[test]
fn footer_is_channel_safe_markdown_link_list() {
    let footer = footer_for("Answer with no links.", &all_engines()).expect("footer present");
    assert!(
        footer.starts_with("**Sources**\n"),
        "header first: {footer}"
    );
    for line in footer.lines().skip(1) {
        assert!(
            line.starts_with("- "),
            "every entry is a plain list item: {line}"
        );
    }
    assert!(
        !footer.contains('|'),
        "no markdown tables (Discord/WhatsApp unsafe)"
    );
    assert!(
        footer.contains("- [First DDG hit](https://example.com/a)"),
        "titled sources render as markdown links"
    );
}

#[test]
fn applied_once_at_the_delivery_seam() {
    // The settle seam appends the footer to final_text. Re-applying with the
    // same outputs must be a no-op, because every fresh URL is now IN the
    // answer and gets dropped. That is the exactly-once pin: the footer can
    // never double up through retry or replay paths.
    let outputs = all_engines();
    let answer = "Answer with no links.";
    let footer = footer_for(answer, &outputs).expect("first application yields a footer");
    let delivered = format!("{}\n\n{}", answer, footer);
    assert_eq!(
        footer_for(&delivered, &outputs),
        None,
        "second application on the already-footed answer must add nothing"
    );
    assert_eq!(
        delivered.lines().filter(|l| *l == "**Sources**").count(),
        1,
        "exactly one Sources header in the delivered text"
    );
}

#[test]
fn url_without_a_title_line_renders_bare() {
    // The bare-URL fallback: only the line immediately before a URL that is
    // a numbered entry becomes its title; a second URL with no fresh title
    // renders as a plain link.
    let output = "🦁 Brave results (query: rust async, 2 found):\n\n1. Some hit\n   URL: https://example.com/x\n   URL: https://example.com/y\n".to_string();
    let footer = footer_for("answer with no links", &[output]).expect("footer present");
    assert!(
        footer.contains("- [Some hit](https://example.com/x)"),
        "the numbered line titles the URL right after it: {footer}"
    );
    assert!(
        footer.contains("\n- https://example.com/y"),
        "an untitled URL falls back to the bare link: {footer}"
    );
}

#[test]
fn only_the_four_search_tools_harvest() {
    for name in ["web_search", "exa_search", "brave_search", "serper_search"] {
        assert!(is_search_tool(name), "{name} must feed the footer");
    }
    for name in [
        "http_request",
        "web_scrape",
        "browser_navigate",
        "bash",
        "read_file",
    ] {
        assert!(
            !is_search_tool(name),
            "{name} is not a search engine and must never feed links"
        );
    }
}

// ── Source-scan guards: the delivery seam must call the builder ──────────
// Precedent: slack_followup_tap_tool_loop_test.rs / discord_followup_tap_
// tool_loop_test.rs pin wiring structurally when the live path needs a
// provider. #1883 requires the footer applied on EVERY path that runs the
// tool loop, so all three tool-execution sites must feed the harvest vector
// and the settle point must call the builder before returning the answer.

fn tool_loop_source() -> &'static str {
    include_str!("../brain/agent/service/tool_loop.rs")
}

#[test]
fn every_tool_execution_site_feeds_the_footer_harvest() {
    let src = tool_loop_source();
    // Sequential non-approval path and approval path push by tool name.
    assert_eq!(
        src.matches("turn_search_outputs.push(").count(),
        3,
        "#1883: sequential, approval-gated and parallel-batch sites must all \
         feed turn_search_outputs (a missed site silently drops that call's links)"
    );
    // The parallel batch harvests through the positional search-call mask.
    assert!(
        src.contains("search_call_mask.iter().zip(batch.outputs.iter())"),
        "the parallel fast path must map batch outputs back to their tool names"
    );
}

#[test]
fn the_settle_seam_appends_the_built_footer_to_the_delivered_answer() {
    let src = tool_loop_source();
    let call = src
        .find("sources_footer::footer_for(&final_text, &turn_search_outputs)")
        .expect("settle point must call the builder on the delivered text");
    // rfind: the file's EARLIER `Ok(AgentResponse {` returns are the /compact
    // confirmations at the loop's head; the settle return is the turn's tail.
    let return_site = src
        .rfind("Ok(AgentResponse {")
        .expect("the turn's Ok(AgentResponse) return is present");
    assert!(
        call < return_site,
        "the footer must be applied BEFORE the answer is handed to the surface"
    );
    assert!(
        src.contains("final_text = format!(\"{}\\n\\n{}\", final_text.trim_end(), footer)"),
        "the footer must ride inside final_text, so TUI and every channel deliver it"
    );
    // The persisted DB row gets the same footer, so a resumed session sees
    // exactly what was delivered.
    assert!(
        src.contains("append_content(assistant_db_msg.id, &format!(\"\\n\\n{footer}\")"),
        "the DB row must carry the delivered footer too"
    );
}

#[test]
fn acceptance_search_turn_ends_with_a_harvested_sources_list() {
    // The contract from the issue: a turn that fired web_search delivers an
    // answer ending with a Sources list harvested from the tool output, even
    // when the model's own text contains no links. Mirrors the seam's exact
    // formatting so the pin covers the delivered bytes, not just the builder.
    let model_answer = "Rust async runs tasks concurrently on a single thread \
                        using an executor.";
    assert!(
        !model_answer.contains("http"),
        "the fixture must model a silent-citation answer"
    );
    let delivered = match footer_for(model_answer, &[ddg_output()]) {
        Some(footer) => format!("{}\n\n{}", model_answer.trim_end(), footer),
        None => panic!("a search-backed turn must get a footer"),
    };
    // Titled hits render as markdown links, so the delivered tail is the
    // titled entry, not a bare URL (bare fallback is pinned separately).
    assert!(
        delivered.ends_with("- [Second DDG hit](https://example.com/b)"),
        "delivered answer must END with the harvested list: {delivered}"
    );
    assert!(delivered.contains("**Sources**"));
    assert!(delivered.contains("- [First DDG hit](https://example.com/a)"));
    assert!(delivered.starts_with(model_answer));
    // Applying the seam logic again is a no-op (exactly-once on replay).
    assert_eq!(footer_for(&delivered, &[ddg_output()]), None);
}
