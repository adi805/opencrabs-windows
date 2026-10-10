//! FR-009 / AC-020 + AC-021: a long answer is a summary plus a pager, never a
//! wall of consecutive messages, and the pager is exactly one Action Row.
//!
//! The pure parts — the page body, the arrow labels, the custom-id round trip
//! and the two-arrows-one-row rule — are unit-tested inline in
//! `src/channels/discord/long_answer.rs`. What those tests cannot reach is the
//! *decision* that routes a long answer into the pager at all, because it lives
//! inside `handle_message` behind an `Http` call.
//!
//! That decision is what AC-020 names, and a doc comment cannot fail a build,
//! so this test reads the send path and pins its shape, the same way
//! `discord_write_discipline_test` pins AC-024 and `discord_fr004_one_message_test`
//! pins AC-008.
//!
//! Four properties, each of which can silently regress:
//!
//! * paging is keyed on the split result (`chunks.len() > 1`), not on a second
//!   char threshold re-derived from the text, which would drift;
//! * page 0 is what goes out in-channel, with the pager row attached;
//! * the pages are stored BEFORE the row is attached, or the button indexes
//!   pages nobody kept;
//! * the pager stands down when a thread is wanted (`!want_thread`), and the
//!   `for chunk in &chunks` loop sits inside the thread branch — that pair is
//!   what keeps an answer from being posted twice. FR-007 moved the thread
//!   decision ahead of the pager, so the exclusion is `!want_thread`, not the
//!   old `!paged`.
//!
//! Scope, stated so a later reader does not "fix" it by widening or narrowing:
//! this pins the routing decision in `handler.rs`. It does not re-test the
//! pager's rendering, and it says nothing about Telegram.

use std::path::Path;

fn discord_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/discord")
}

/// `handler.rs` as written, for the positional checks.
fn handler_src() -> String {
    let path = discord_dir().join("handler.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The same source with all whitespace removed, so a call split across lines
/// still matches a needle written on one line.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// AC-020: a long answer routes into the pager rather than into a run of
/// messages, and the message that goes out in-channel is page 0.
///
/// The split needle deliberately stops at `split_message(` and does not pin
/// the variable that is split: the evidence footer (FR-007) changes the
/// outgoing text from the answer alone to the answer plus its footer, and a
/// test that pinned the old argument would fail on a change that is correct.
#[test]
fn a_long_answer_routes_into_the_pager_not_a_wall() {
    let flat_src = flat(&handler_src());

    assert!(
        flat_src.contains("super::long_answer::PAGE_CHARS"),
        "the final answer is no longer split at the pager's page size, so the \
         page count the pager is keyed on is not the one Discord receives \
         (AC-020)"
    );
    assert!(
        flat_src.contains("chunks.len()>1"),
        "the send path no longer gates the pager on the body splitting into \
         more than one page (AC-020). Without that gate a long answer either \
         goes out as one oversized message or falls back to a wall."
    );
    assert!(
        flat_src.contains("content(&chunks[0])"),
        "page 0 is no longer what gets posted in-channel: re-check that a \
         long answer still posts a summary plus a pager (AC-020)"
    );
    assert!(
        flat_src.contains("pager_row("),
        "the pager row is no longer attached to page 0, so there is nothing \
         to press and the rest of the answer is unreachable (AC-020/AC-021)"
    );
}

/// The button is useless unless the pages it indexes were kept first. A
/// reorder that attaches the row before storing would ship a pager pointing at
/// pages that do not exist.
#[test]
fn the_pages_are_stored_before_the_pager_row_is_attached() {
    let flat_src = flat(&handler_src());

    let store = flat_src
        .find("store_long_answer(mid,chunks.clone())")
        .expect("page 0 must be stored so the pager has something to serve (AC-020)");
    let attach = flat_src
        .find("pager_row(mid,0,chunks.len())")
        .expect("the pager row must be built from the stored message id (AC-020)");
    assert!(
        store < attach,
        "store_long_answer must run BEFORE pager_row is attached, or the \
         button indexes pages that were never kept (AC-020)"
    );
}

/// AC-020 from the other side: an answer that already went out as page 0 must
/// not also be auto-threaded or split into plain messages. Both paths are
/// gated on `paged`.
#[test]
fn paging_suppresses_the_thread_and_the_plain_split() {
    let src = handler_src();
    let flat_src = flat(&src);

    assert!(
        flat_src.contains("if!want_thread&&chunks.len()>1"),
        "the pager no longer stands down for an answer that wants a thread, \
         so a long answer would post page 0 AND then the full body again \
         (AC-020/AC-010)"
    );
    assert!(
        flat_src.contains("}elseif!paged{"),
        "the plain-split fallback no longer excludes a paged answer, so a long \
         answer would post page 0 AND the whole answer again (AC-020)"
    );

    // Positional: the loop that posts every chunk must sit inside the thread
    // branch. `!paged` guards that branch, so this is what keeps the loop
    // unreachable for a paged answer.
    let thread_guard = src.find("if auto_thread {").expect(
        "the auto-thread branch is gone: re-check that the chunk loop is still \
         unreachable for a paged answer (AC-020)",
    );
    let chunk_loop = src.find("for chunk in &chunks").expect(
        "the chunk loop moved or was renamed: re-check which branch posts the \
         remaining chunks (AC-020)",
    );
    assert!(
        chunk_loop > thread_guard,
        "the chunk loop now runs outside the auto-thread branch, so a long \
         answer can be posted as a wall of consecutive messages (AC-020)"
    );
}

/// The scan must be able to fail. A path typo or a rename would make the tests
/// above pass while checking nothing, which reports safety it never verified.
#[test]
fn the_scan_sees_the_send_path_it_governs() {
    let src = handler_src();
    assert!(
        src.len() > 10_000,
        "handler.rs came back suspiciously small ({} bytes): the scan is \
         reading the wrong file",
        src.len()
    );
    let flat_src = flat(&src);
    for needle in ["writes::send(", "writes::edit("] {
        assert!(
            flat_src.contains(needle),
            "handler.rs no longer calls {needle}, so this scan is looking at a \
             file that no longer owns the send path"
        );
    }
}

/// AC-020 from the reader's side. Page 0 posts in-channel, so the row attached
/// to it must be drawn FOR page 0: `◀` sits there disabled and `▶` walks into
/// the first hidden page. Drawing it for page 1 instead would put a live `◀`
/// on the in-channel row, so the first press would jump backwards to text the
/// reader already has, and a later page could never be walked back to. The
/// ephemeral answer must re-draw the same row for whatever page it is showing,
/// or every page after the first press is stored but unreachable. The tests
/// above only pin that *a* row exists, so they cannot see this.
#[test]
fn the_pager_opens_a_hidden_page_and_carries_the_next_row_forward() {
    let flat_handler = flat(&handler_src());
    assert!(
        flat_handler.contains("pager_row(mid,0,chunks.len())"),
        "the row attached to page 0 no longer starts at page 0, so `◀` on the \
         in-channel row is live and jumps to the wrong page (AC-020)"
    );

    let agent_path = discord_dir().join("agent.rs");
    let agent_src = std::fs::read_to_string(&agent_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", agent_path.display()));
    assert!(
        flat(&agent_src).contains("pager_row("),
        "the ephemeral answer no longer carries a row, so every page after the \
         first press is stored but unreachable (AC-020)"
    );
}
