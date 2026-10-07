//! Deterministic Sources footer for search-backed answers (#1883).
//!
//! Search tools already carry source URLs into the tool output the model
//! sees — DuckDuckGo renders `   🔗 {url}`, Exa/Brave/Serper render
//! `   URL: {url}` — but nothing on the *delivered* side surfaces them:
//! whether a chat reply cites its sources is purely the model's inclination
//! per turn. This module harvests those URLs from the turn's search-tool
//! outputs and builds a bounded, deterministic footer that the composition
//! path appends to the final answer, so the links appear even when the model
//! stays silent.
//!
//! Contract (issue #1883):
//! - Harvest in engine-result order; dedup cross-engine (the same URL in
//!   either render format counts once).
//! - Cap the list at [`MAX_SOURCES`].
//! - Drop URLs the answer already links, so the footer never repeats the
//!   model's own citation.
//! - Turns that used no search tool are unchanged, byte-for-byte.
//! - Plain markdown link list (no tables) so Discord/WhatsApp render it.

use super::web_search::{entry_url, normalize_url};
use regex::Regex;

/// Upper bound on footer entries.
pub const MAX_SOURCES: usize = 5;

/// The search tools whose rendered output carries source URLs. Harvesting is
/// gated on the tool NAME, not on the output shape, so a file or page whose
/// text happens to contain a `URL:` line is never mistaken for a search hit.
pub fn is_search_tool(name: &str) -> bool {
    matches!(
        name,
        "web_search" | "exa_search" | "brave_search" | "serper_search"
    )
}

/// Harvest source URLs from rendered search-tool outputs.
///
/// Later duplicates — the same URL returned by another engine, in either
/// render format — are dropped; first occurrence wins, so engine-result
/// order is preserved. Stops as soon as `cap` URLs are collected.
pub fn collect_source_urls(outputs: &[String], cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut urls: Vec<String> = Vec::new();
    for output in outputs {
        for line in output.lines() {
            if urls.len() >= cap {
                return urls;
            }
            if let Some(url) = entry_url(line)
                && seen.insert(normalize_url(url))
            {
                urls.push(url.to_string());
            }
        }
    }
    urls
}

/// http(s) URLs already present in `text`, normalized for comparison.
///
/// Wrappers are ignored — a bare URL, `<https://…>`, or `[t](https://…)`
/// all count as "already linked", because the point is to avoid repeating a
/// source the reader can already see.
fn linked_urls(text: &str) -> std::collections::HashSet<String> {
    // Trailing sentence punctuation and closing brackets are not part of the
    // URL; the character class already excludes `)` `]` `>` and quotes.
    let re = Regex::new(r#"https?://[^\s<>"')\]]+"#).expect("valid url regex");
    re.find_iter(text)
        .map(|m| {
            let raw = m.as_str().trim_end_matches(['.', ',', ';', ':']);
            normalize_url(raw)
        })
        .filter(|u| u.len() > "https://".len())
        .collect()
}

/// Build the deterministic footer for `answer` from the turn's search outputs.
///
/// Returns `None` when no search output contributed a URL, or when every
/// harvested URL is already linked in the answer — callers append only
/// `Some`, so a turn with nothing to add is unchanged.
pub fn sources_footer(answer: &str, outputs: &[String]) -> Option<String> {
    let harvested = collect_source_urls(outputs, MAX_SOURCES);
    if harvested.is_empty() {
        return None;
    }
    let already = linked_urls(answer);
    let fresh: Vec<String> = harvested
        .into_iter()
        .filter(|u| !already.contains(&normalize_url(u)))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    let mut footer = String::from("\n\nSources:\n");
    for url in fresh {
        footer.push_str("- ");
        footer.push_str(&url);
        footer.push('\n');
    }
    Some(footer)
}

/// Append the Sources footer to `answer`, or return it unchanged when there
/// is nothing to add.
pub fn append_sources_footer(answer: &str, outputs: &[String]) -> String {
    match sources_footer(answer, outputs) {
        Some(footer) => format!("{answer}{footer}"),
        None => answer.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ddg(url: &str, title: &str) -> String {
        format!("1. {title}\n   🔗 {url}\n\n")
    }

    fn url_format(url: &str, title: &str) -> String {
        format!("1. {title}\n   URL: {url}\n\n")
    }

    #[test]
    fn parses_both_render_formats() {
        let outputs = vec![
            ddg("https://ddg.example/a", "A"),
            url_format("https://exa.example/b", "B"),
        ];
        assert_eq!(
            collect_source_urls(&outputs, MAX_SOURCES),
            vec!["https://ddg.example/a", "https://exa.example/b"]
        );
    }

    #[test]
    fn dedups_cross_engine_same_url_in_either_format() {
        let outputs = vec![
            ddg("https://shared.example/x", "X"),
            url_format("https://shared.example/x", "X"),
            url_format("https://Shared.Example/X/", "X"),
            url_format("https://other.example/y", "Y"),
        ];
        assert_eq!(
            collect_source_urls(&outputs, MAX_SOURCES),
            vec!["https://shared.example/x", "https://other.example/y"]
        );
    }

    #[test]
    fn preserves_order_and_caps() {
        let outputs: Vec<String> = (1..=8)
            .map(|i| url_format(&format!("https://s.example/{i}"), &format!("T{i}")))
            .collect();
        let urls = collect_source_urls(&outputs, MAX_SOURCES);
        assert_eq!(urls.len(), MAX_SOURCES);
        assert_eq!(urls[0], "https://s.example/1");
        assert_eq!(urls[4], "https://s.example/5");
    }

    #[test]
    fn drops_urls_already_linked_in_answer() {
        let outputs = vec![
            url_format("https://a.example/1", "A"),
            url_format("https://b.example/2", "B"),
        ];
        let answer = "See [A](https://a.example/1) for the detail.";
        let footer = sources_footer(answer, &outputs).expect("one fresh source remains");
        assert!(footer.contains("https://b.example/2"));
        assert!(!footer.contains("https://a.example/1"));
    }

    #[test]
    fn no_footer_when_answer_already_links_every_source() {
        let outputs = vec![url_format("https://a.example/1", "A")];
        let answer = "Cited at https://a.example/1.";
        assert_eq!(sources_footer(answer, &outputs), None);
    }

    #[test]
    fn no_search_tool_leaves_answer_byte_identical() {
        let answer = "Plain answer with no search behind it.";
        assert_eq!(append_sources_footer(answer, &[]), answer);
    }

    #[test]
    fn footer_is_a_plain_markdown_list() {
        let outputs = vec![url_format("https://a.example/1", "A")];
        let out = append_sources_footer("Answer.", &outputs);
        assert!(out.ends_with("\n\nSources:\n- https://a.example/1\n"));
    }
}
