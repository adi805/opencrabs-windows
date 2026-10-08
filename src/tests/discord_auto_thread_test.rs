//! FR-007 / AC-010: a long answer is routed into a thread, with the in-place
//! chunker as the fallback when the thread is refused.
//!
//! The pure decision (`wants_thread`) is unit-tested inline in
//! `src/channels/discord/long_answer.rs`. What a unit test cannot reach is the
//! ORDER inside `handle_message`: the thread has to be decided BEFORE the
//! pager, because the pager claims every answer past `PAGE_CHARS` and gating
//! the thread behind `!paged` made a thread unreachable for exactly the long
//! answers it exists for. A doc comment cannot fail a build, so this test reads
//! the send path and pins its shape, the same way
//! `discord_long_answer_decision_test` pins the pager.
//!
//! Scope, stated so a later reader does not widen or narrow it: this pins the
//! routing decision in `handler.rs`. It does not re-test the pager's rendering
//! and it says nothing about Telegram.

use std::path::Path;

/// `handler.rs` as written, for the positional checks.
fn handler_src() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/discord/handler.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The same source with all whitespace removed, so a call split across lines
/// still matches a needle written on one line.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The thread decision must not be a function of the pager's page count.
#[test]
fn the_thread_is_decided_before_the_pager() {
    let src = handler_src();
    let flat_src = flat(&src);

    assert!(
        flat_src.contains("super::long_answer::wants_thread("),
        "the send path no longer asks wants_thread, so the thread route is \
         gone (AC-010)"
    );
    assert!(
        !flat_src.contains("auto_thread=!paged"),
        "the thread is gated on `!paged` again: that is the bug FR-007 fixes, \
         because the pager claims every answer past PAGE_CHARS and the thread \
         becomes unreachable for the answers it is for (AC-010)"
    );
    assert!(
        flat_src.contains("auto_thread=want_thread"),
        "the thread decision no longer reuses the value computed before the \
         pager, so it can drift from the pager gate (AC-010)"
    );

    let decide = src
        .find("super::long_answer::wants_thread(")
        .expect("the thread decision is gone (AC-010)");
    let pager = src
        .find("let paged =")
        .expect("the pager is gone (AC-020)");
    assert!(
        decide < pager,
        "the thread decision now runs after the pager, which is the ordering \
         that made a thread unreachable for long answers (AC-010)"
    );
}

/// The pager must stand down when a thread is wanted, or a long answer would
/// be posted twice: once as page 0 and once in the thread.
#[test]
fn a_wanted_thread_suppresses_the_pager() {
    let flat_src = flat(&handler_src());
    assert!(
        flat_src.contains("if!want_thread&&chunks.len()>1"),
        "the pager no longer excludes an answer that wants a thread, so the \
         answer would go out as page 0 AND again in the thread (AC-010)"
    );
}

/// The fallback: when `create_thread_from_message` fails, the chunks still go
/// out in place. This is the milestone's "the in-place chunker still works
/// when thread creation is refused".
#[test]
fn a_refused_thread_falls_back_to_the_in_place_chunker() {
    let src = handler_src();
    let flat_src = flat(&src);

    assert!(
        flat_src.contains("create_thread_from_message("),
        "the thread is no longer created, so the fallback it guards is \
         unreachable (AC-010)"
    );
    let refused = src.contains("auto-thread refused")
        || src.contains("auto-thread failed, posting inline");
    assert!(
        refused,
        "the thread-refused arm is gone: a refused thread now has no \
         fallback, so the answer is lost (AC-010)"
    );
    assert!(
        flat_src.contains("forchunkin&chunks{"),
        "the in-place chunker loop is gone, so a refused thread drops the \
         answer instead of posting it (AC-010)"
    );
}

/// A thread is only a valid target when the answer is not already inside one.
/// Discord refuses to anchor a thread to a thread, so without this guard every
/// long answer in a thread would fire a request Discord is guaranteed to
/// reject and only then fall back to posting in place.
#[test]
fn a_thread_is_not_opened_inside_a_thread_or_a_dm() {
    let flat_src = flat(&handler_src());
    assert!(
        flat_src.contains("want_thread=long_enough&&!is_dm&&!channel_is_thread("),
        "the thread decision no longer excludes a DM or an answer that already \
         sits in a thread, so it fires a request Discord rejects (AC-010)"
    );
    assert!(
        flat_src.contains("asyncfnchannel_is_thread(http:&Http,channel:ChannelId)->bool"),
        "the channel-kind lookup is gone, so nothing tells the send path \
         whether it is already inside a thread (AC-010)"
    );
    assert!(
        flat_src.contains("ChannelType::PublicThread|ChannelType::PrivateThread"),
        "the thread kinds are no longer recognised, so a forum post or a \
         thread reads as a plain channel (AC-010)"
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
    assert!(
        flat(&src).contains("writes::say("),
        "handler.rs no longer owns the final send path, so this scan is \
         looking at the wrong file"
    );
}
