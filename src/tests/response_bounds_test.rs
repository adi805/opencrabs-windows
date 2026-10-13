//! Bounded response reads (#216).
//!
//! `web_scrape`'s static fetch and dynamic HTTP tools both called `resp.text()`,
//! which buffers the ENTIRE body before any limit applies. The tool's own
//! output budget ran afterwards, so it was a display rule, not a bound: a large
//! or endless chunked response had to arrive in full, in memory, first.
//!
//! The bound now runs during the read. These tests pin the accumulation rule
//! against the chunk shapes that matter (a chunk larger than the whole cap, and
//! a body that ends exactly at it), the configurable per-tool policy, and - in
//! source form - that neither path buffers unconditionally any more.

use crate::brain::tools::dynamic::tool::DynamicToolDef;
use crate::brain::tools::response_bounds::{
    DEFAULT_MAX_RESPONSE_BYTES, accumulate_capped, note_truncation,
};
use crate::tests::source_shape::{flatten, read_in};

// ── the accumulation rule ───────────────────────────────────────────

#[test]
fn a_body_under_the_cap_is_returned_whole_and_untruncated() {
    let (buf, truncated) = accumulate_capped(vec![b"hello ".to_vec(), b"world".to_vec()], 64);
    assert_eq!(buf, b"hello world");
    assert!(!truncated, "a body under the cap is not truncated");
}

#[test]
fn a_body_exactly_at_the_cap_is_not_truncated() {
    let (buf, truncated) = accumulate_capped(vec![b"12345".to_vec(), b"67890".to_vec()], 10);
    assert_eq!(buf.len(), 10);
    assert!(
        !truncated,
        "landing exactly on the cap is a complete read; reporting it as \
         truncated would tell the model the document was cut when it was not"
    );
}

#[test]
fn a_single_chunk_larger_than_the_cap_is_cut_at_the_cap() {
    // The shape that matters most: one chunk can be arbitrarily large, so
    // "read the chunk, then check the size" would already have buffered it all.
    let (buf, truncated) = accumulate_capped(vec![vec![b'x'; 10_000]], 100);
    assert_eq!(buf.len(), 100, "the cap is the ceiling, not the chunk size");
    assert!(truncated);
}

#[test]
fn accumulation_stops_at_the_cap_and_ignores_later_chunks() {
    let (buf, truncated) = accumulate_capped(vec![vec![b'a'; 8], vec![b'b'; 8], vec![b'c'; 8]], 10);
    assert_eq!(buf.len(), 10);
    assert!(truncated);
    assert!(
        !buf.contains(&b'c'),
        "the third chunk must never be read once the cap is reached"
    );
}

#[test]
fn an_empty_body_is_not_truncated() {
    let (buf, truncated) = accumulate_capped(Vec::<Vec<u8>>::new(), 16);
    assert!(buf.is_empty());
    assert!(!truncated);
}

#[test]
fn the_default_cap_is_finite_and_generous() {
    assert!(DEFAULT_MAX_RESPONSE_BYTES >= 1024 * 1024);
    assert!(
        DEFAULT_MAX_RESPONSE_BYTES <= 64 * 1024 * 1024,
        "a cap this large would not bound anything useful on a small host"
    );
}

// ── the truncation notice ───────────────────────────────────────────

#[test]
fn a_truncated_read_says_so_and_a_complete_one_does_not() {
    let (buf, truncated) = accumulate_capped(vec![vec![b'x'; 50]], 10);
    let read = crate::brain::tools::response_bounds::BodyRead {
        text: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
        bytes_read: buf.len(),
    };
    let noted = note_truncation(read.text.clone(), &read, 10);
    assert!(
        noted.contains("truncated"),
        "a partial body must be labelled, or the model reads a prefix as the whole"
    );

    let complete = crate::brain::tools::response_bounds::BodyRead {
        text: "all of it".to_string(),
        truncated: false,
        bytes_read: 9,
    };
    assert_eq!(
        note_truncation(complete.text.clone(), &complete, 10),
        complete.text,
        "a complete read must not be annotated"
    );
}

// ── the per-tool policy ─────────────────────────────────────────────

#[test]
fn a_dynamic_http_tool_defaults_to_guarded_and_capped() {
    let def: DynamicToolDef = toml::from_str(
        r#"
name = "probe"
description = "d"
executor = "http"
url = "https://example.com/x"
"#,
    )
    .expect("a minimal HTTP tool parses");
    assert!(
        !def.allow_private_network,
        "#216: private-network targets must be opt-IN; an accidental blanket \
         prohibition is replaced by an explicit policy, not by a default allow"
    );
    assert_eq!(
        def.max_response_bytes, None,
        "unset means the shared default cap, not unlimited"
    );
}

#[test]
fn a_dynamic_http_tool_can_opt_into_both() {
    let def: DynamicToolDef = toml::from_str(
        r#"
name = "probe"
description = "d"
executor = "http"
url = "http://10.0.0.5/status"
allow_private_network = true
max_response_bytes = 4096
"#,
    )
    .expect("the opt-in fields parse");
    assert!(def.allow_private_network);
    assert_eq!(def.max_response_bytes, Some(4096));
}

// ── the wiring ──────────────────────────────────────────────────────

#[test]
fn neither_fetch_path_buffers_the_whole_body_any_more() {
    // Flattened, because rustfmt wraps these chains across lines. The needle
    // carries `.await` so it matches the CALL and not the prose in the module
    // doc, which names the old call on purpose.
    let scrape = flatten(read_in("src/brain/tools/web_scrape", "fetch.rs"));
    assert!(
        !scrape.contains("resp.text().await"),
        "web_scrape still awaits `resp.text()`, which buffers the whole body \
         before the cap can apply (#216)"
    );
    assert!(
        scrape.contains("read_body_capped("),
        "web_scrape must read its body under the cap"
    );

    let dynamic = flatten(read_in("src/brain/tools/dynamic", "tool.rs"));
    assert!(
        !dynamic.contains("resp.text().await.unwrap_or_default()"),
        "dynamic HTTP still buffers the whole body unconditionally (#216)"
    );
    assert!(
        dynamic.contains("read_body_capped("),
        "dynamic HTTP must read its body under the cap"
    );
    assert!(
        dynamic.contains("ssrf::guard_client("),
        "dynamic HTTP must go through the SSRF-guarded client instead of \
         `Client::new()` (#216)"
    );
    assert!(
        dynamic.contains("validate_url_resolved("),
        "dynamic HTTP must validate its destination before sending (#216)"
    );
}

/// The rendered fallback must re-check the URL it LANDED on, because a redirect
/// can move the browser somewhere the pre-navigation check allowed.
#[test]
fn rendered_fetch_rechecks_the_landed_url() {
    let src = read_in("src/brain/tools/web_scrape", "fetch.rs");
    let lines: Vec<&str> = src.lines().collect();
    let goto = lines
        .iter()
        .position(|l| l.contains("page.goto(url)"))
        .expect("the rendered path navigates");
    let after = lines[goto..].join("\n");
    assert!(
        after.contains("page.url()"),
        "the rendered path must read back the landed URL (#216)"
    );
    assert!(
        after.contains("validate_url("),
        "the landed URL must be re-validated, or a redirect can carry the \
         browser to a destination the pre-navigation check refused (#216)"
    );
}
