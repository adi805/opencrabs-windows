//! Tests for the Bot API 10.3 rich-plane blocks (`rich/ast.rs` +
//! `rich/render_json.rs` + `rich/detect.rs`): the `is_compact` flag on
//! RichBlockTable, RichBlockExpandableBlockQuotation, and RichBlockDocument
//! with its `tg://document?id=` reference.
//!
//! The AST and the JSON serializer are pinned here, plus the gate that sends a
//! document reference to the rich plane: a `tg://document?id=` string is only
//! meaningful there, and the HTML ladder would show it as literal text.

use crate::channels::telegram::rich::ast::{Align, Block, Inline, Table};
use crate::channels::telegram::rich::detect::{
    contains_document_ref, document_ref_id, has_rich_structure, prefers_rich_render,
};
use crate::channels::telegram::rich::parse::parse_markdown;
use crate::channels::telegram::rich::render_html::render_block as render_html_block;
use crate::channels::telegram::rich::render_json::{input_rich_message, render_block};

fn text(s: &str) -> Vec<Inline> {
    vec![Inline::Text(s.to_string())]
}

fn table(is_compact: bool) -> Table {
    Table {
        align: vec![Align::Left, Align::Right],
        header: vec![text("h1"), text("h2")],
        rows: vec![vec![text("a"), text("1")], vec![text("b"), text("2")]],
        is_compact,
    }
}

// ── RichBlockTable.is_compact ────────────────────────────────────────

#[test]
fn table_carries_is_compact_when_set() {
    let v = render_block(&Block::Table(table(true)));
    assert_eq!(v["type"], "table");
    assert_eq!(v["is_compact"], true);
    assert_eq!(v["header"][0][0]["text"], "h1");
    assert_eq!(v["rows"][1][0][0]["text"], "b");
}

#[test]
fn table_omits_is_compact_when_unset() {
    let v = render_block(&Block::Table(table(false)));
    // Absent, not false: a table that never asked for the compact layout must
    // keep sending the body it sent before 10.3.
    assert!(v.get("is_compact").is_none());
    assert_eq!(v["align"][0], "left");
    assert_eq!(v["align"][1], "right");
}

#[test]
fn a_parsed_table_defaults_to_not_compact() {
    let blocks = parse_markdown("| a | b |\n| - | - |\n| 1 | 2 |");
    let Block::Table(parsed) = &blocks[0] else {
        panic!("expected a table block, got {:?}", blocks[0]);
    };
    assert!(!parsed.is_compact);
    assert!(render_block(&blocks[0]).get("is_compact").is_none());
}

// ── RichBlockExpandableBlockQuotation ────────────────────────────────

#[test]
fn expandable_quote_serializes_to_expandable_blockquote() {
    let block = Block::ExpandableQuote {
        text: text("quoted line"),
        credit: Some(text("source")),
    };
    let v = render_block(&block);
    assert_eq!(v["type"], "expandable_blockquote");
    assert_eq!(v["text"][0]["type"], "text");
    assert_eq!(v["text"][0]["text"], "quoted line");
    assert_eq!(v["credit"][0]["text"], "source");
}

#[test]
fn expandable_quote_omits_credit_when_absent() {
    let block = Block::ExpandableQuote {
        text: text("just the quote"),
        credit: None,
    };
    let v = render_block(&block);
    assert_eq!(v["type"], "expandable_blockquote");
    assert!(v.get("credit").is_none());
}

// ── RichBlockDocument ────────────────────────────────────────────────

#[test]
fn document_block_serializes_to_rich_block_document() {
    let block = Block::Document {
        media: "BQACAgQAAxkBAAIC".to_string(),
        caption: Some(text("the file")),
    };
    let v = render_block(&block);
    assert_eq!(v["type"], "document");
    assert_eq!(v["document"]["type"], "document");
    assert_eq!(v["document"]["media"], "BQACAgQAAxkBAAIC");
    assert_eq!(v["caption"]["text"][0]["text"], "the file");
}

#[test]
fn document_block_omits_caption_when_absent() {
    let block = Block::Document {
        media: "attach://doc1".to_string(),
        caption: None,
    };
    let v = render_block(&block);
    assert_eq!(v["document"]["media"], "attach://doc1");
    assert!(v.get("caption").is_none());
}

#[test]
fn document_block_carries_a_tg_document_reference_verbatim() {
    let block = Block::Document {
        media: "tg://document?id=doc1".to_string(),
        caption: None,
    };
    let v = render_block(&block);
    assert_eq!(v["document"]["media"], "tg://document?id=doc1");
}

#[test]
fn envelope_wraps_the_new_blocks() {
    let blocks = vec![
        Block::ExpandableQuote {
            text: text("q"),
            credit: None,
        },
        Block::Document {
            media: "tg://document?id=d".to_string(),
            caption: None,
        },
    ];
    let v = input_rich_message(&blocks);
    let arr = v["blocks"].as_array().expect("blocks array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "expandable_blockquote");
    assert_eq!(arr[1]["type"], "document");
}

// ── tg://document?id= detection ──────────────────────────────────────

#[test]
fn document_ref_id_reads_the_id() {
    assert_eq!(document_ref_id("tg://document?id=abc123"), Some("abc123"));
    let wrapped = document_ref_id("see [file](tg://document?id=abc123) here");
    assert_eq!(wrapped, Some("abc123"));
    let bracket = document_ref_id("see [file][tg://document?id=ref9]");
    assert_eq!(bracket, Some("ref9"));
}

#[test]
fn document_ref_id_rejects_absent_or_empty() {
    assert_eq!(document_ref_id("no reference here"), None);
    assert_eq!(document_ref_id("tg://document?id="), None);
    assert_eq!(document_ref_id("tg://document?id=)"), None);
    // The photo reference is a different scheme and must not match.
    assert_eq!(document_ref_id("tg://photo?id=abc"), None);
}

#[test]
fn document_ref_stops_at_a_query_separator() {
    let amp = document_ref_id("tg://document?id=abc&x=1");
    assert_eq!(amp, Some("abc"));
    let frag = document_ref_id("tg://document?id=abc#frag");
    assert_eq!(frag, Some("abc"));
    let plain = document_ref_id("tg://document?id=a-b_c.d~e");
    assert_eq!(plain, Some("a-b_c.d~e"));
}

#[test]
fn document_ref_asks_for_the_rich_plane() {
    assert!(contains_document_ref("tg://document?id=abc"));
    assert!(!contains_document_ref("plain prose only"));
    assert!(has_rich_structure("here: tg://document?id=abc"));
    assert!(prefers_rich_render("here: tg://document?id=abc"));
    // The control: neither gate fires on ordinary prose.
    assert!(!has_rich_structure("just a sentence"));
    assert!(!prefers_rich_render("just a sentence with **bold** in it"));
}

// ── HTML fallback for the two new blocks ─────────────────────────────

#[test]
fn html_fallback_renders_expandable_quote_as_a_blockquote() {
    let block = Block::ExpandableQuote {
        text: text("quoted"),
        credit: Some(text("credit line")),
    };
    let html = render_html_block(&block, false);
    assert!(html.contains("<blockquote>quoted</blockquote>"), "{html}");
    assert!(html.contains("<i>credit line</i>"), "{html}");
}

#[test]
fn html_fallback_renders_a_document_as_caption_plus_reference() {
    let block = Block::Document {
        media: "tg://document?id=d1".to_string(),
        caption: Some(text("the file")),
    };
    let html = render_html_block(&block, false);
    assert!(html.contains("the file"), "{html}");
    assert!(html.contains("<code>tg://document?id=d1</code>"), "{html}");
}

#[test]
fn html_fallback_shows_a_captionless_document_as_the_reference() {
    let block = Block::Document {
        media: "attach://doc1".to_string(),
        caption: None,
    };
    let html = render_html_block(&block, false);
    assert_eq!(html, "<code>attach://doc1</code>");
}
