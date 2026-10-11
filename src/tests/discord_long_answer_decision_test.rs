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
//! Six properties, each of which can silently regress:
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
//!   old `!paged`;
//! * a page press rewrites the card in place (`UpdateMessage`), so the arrows
//!   and the text they control never split into two separate messages;
//! * that in-place rewrite renders the SAME shape as the page-0 create. Page 0
//!   goes through `writes::send`, which under `auto_embed` turns it into a
//!   card, and the press answers with a raw `CreateInteractionResponse` that
//!   never passes through `writes::edit`. Without the same pure decision
//!   applied on both sides the card flips to plain text on the first press
//!   while the page-0 embed stays pinned above it.
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

/// The pager-press branch of `agent.rs`, sliced out so the assertions below
/// talk about the page-turn path and nothing else. `agent.rs` answers
/// components in a dozen places and most of them create a fresh message on
/// purpose, so a whole-file scan cannot tell an in-place page turn from an
/// unrelated bubble. The slice runs from the prefix match that opens the
/// branch to the `toolgroup:` match that opens the next one.
fn pager_branch() -> String {
    let agent_path = discord_dir().join("agent.rs");
    let src = std::fs::read_to_string(&agent_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", agent_path.display()));
    let start = src
        .find("strip_prefix(super::long_answer::PAGER_PREFIX)")
        .expect("agent.rs no longer matches PAGER_PREFIX: the pager press moved");
    let rest = &src[start..];
    let end = rest
        .find(r#"strip_prefix("toolgroup:")"#)
        .expect("the toolgroup branch that closes the pager press moved");
    rest[..end].to_string()
}

/// AC-020 from the reader's side. Page 0 posts in-channel, so the row attached
/// to it must be drawn FOR page 0: `◀` sits there disabled and `▶` walks into
/// the first hidden page. Drawing it for page 1 instead would put a live `◀`
/// on the in-channel row, so the first press would jump backwards to text the
/// reader already has, and a later page could never be walked back to. The
/// press must re-draw the same row for whatever page it is showing, or every
/// page after the first press is stored but unreachable. The tests above only
/// pin that *a* row exists, so they cannot see this.
#[test]
fn the_pager_opens_a_hidden_page_and_carries_the_next_row_forward() {
    let flat_handler = flat(&handler_src());
    assert!(
        flat_handler.contains("pager_row(mid,0,chunks.len())"),
        "the row attached to page 0 no longer starts at page 0, so `◀` on the \
         in-channel row is live and jumps to the wrong page (AC-020)"
    );

    assert!(
        flat(&pager_branch()).contains("pager_row("),
        "the pager press no longer carries a row, so every page after the \
         first press is stored but unreachable (AC-020)"
    );
}

/// A pager row the governor is allowed to drop is a pager that never arrives.
/// The row is the only handle to pages 1..N. Page 0 is admitted as
/// `Class::Final`, so a `Class::Edit` attach could be refused while the text it
/// belongs to lands, leaving every later page stored but unreachable with no
/// second chance to attach the controls. The attach must therefore be a class
/// the governor never drops. The tests above only pin that a row is BUILT, so
/// none of them can see this.
#[test]
fn the_pager_attach_is_not_a_droppable_class() {
    let flat_handler = flat(&handler_src());
    let row = flat_handler
        .find("pager_row(mid,0,chunks.len())")
        .expect("the page-0 pager row moved: this test pins the attach class");
    let after = &flat_handler[row..];
    let attach = after
        .find("writes::edit(")
        .expect("the pager attach no longer edits the page-0 message");
    let tail = &after[attach..];
    let end = tail
        .find(')')
        .expect("the pager attach call has no argument list");
    assert!(
        tail[..end].contains("Class::Final"),
        "the page-0 pager attach must be admitted as `Class::Final`: a \
         droppable class can be refused, and this row is the only handle to \
         pages 1..N (AC-020)"
    );
    assert!(
        !tail[..end].contains("Class::Edit") && !tail[..end].contains("Class::Create"),
        "a droppable class on the pager attach lets the governor drop the \
         controls while the text they belong to lands (AC-020)"
    );
}

/// AC-020, the shape the reader actually sees. A press must **rewrite the card
/// that carries the arrows**, not post a fresh ephemeral bubble: answering
/// ephemerally left the card frozen on page 0 while the requested page landed
/// somewhere else in the channel, so the reader had to scroll for the page they
/// had just asked for and the arrows they pressed never moved.
///
/// This pins the response *variant*, which is the entire difference between the
/// two behaviours and is invisible to every other test here:
/// `CreateInteractionResponse::UpdateMessage` rewrites the message the
/// component is attached to, `::Message` creates one. A refactor that swapped
/// them back would keep the row, keep the page body, keep the store intact, and
/// silently reintroduce the bug, which is exactly what happened once already.
#[test]
fn a_page_press_edits_the_card_instead_of_posting_an_ephemeral_bubble() {
    let branch = flat(&pager_branch());
    let update = branch
        .find("CreateInteractionResponse::UpdateMessage(")
        .expect(
            "the pager press no longer updates the card in place, so an arrow \
             press spawns a new message and leaves the arrows behind (AC-020)",
        );

    // The aged-out fallback may still answer ephemerally (its pages are gone,
    // and overwriting the card would destroy the page the reader can see), but
    // it must not be reachable before the in-place edit, or the page body can
    // still be routed into a fresh bubble.
    if let Some(ephemeral) = branch.find(".ephemeral(true)") {
        assert!(
            ephemeral > update,
            "the pager press answers ephemerally before the in-place edit, so \
             the requested page lands in a bubble instead of on the card the \
             arrows are attached to (AC-020)"
        );
    }
}

/// AC-020, the shape rather than the destination. Rewriting the card in place
/// is only half the job: the rewrite has to render the same *kind* of message
/// the create did, or the reader watches the card change form mid-answer.
///
/// Page 0 is posted through `writes::send` with `Class::Final`, and `writes.rs`
/// turns that into a card whenever `channels.discord.auto_embed` is on (it is
/// on by default). The press answers with a raw `CreateInteractionResponse`,
/// which never passes through `writes::edit`, so the `auto_embed_edit` choke
/// point cannot reach it. `embed.rs` states the invariant in its own words:
/// the embed decision is a pure function of the body, which is what keeps a
/// create and its later edits symmetric. This press was the one card write that
/// escaped it, so the fix is to run the same pure decision over the response.
///
/// The check is positional on purpose: `auto_embed_response(` merely appearing
/// somewhere in the branch would not prove it was applied to the message that
/// goes into `UpdateMessage`, which is the only thing that matters.
#[test]
fn a_page_press_renders_the_same_card_shape_as_page_zero() {
    let branch = flat(&pager_branch());

    let embed = branch.find("auto_embed_response(").expect(
        "the pager press no longer applies the auto-embed decision, so a \
             card created as an embed is rewritten as plain text on the first \
             press and the page-0 card stays pinned above it (AC-020)",
    );
    let update = branch
        .find("CreateInteractionResponse::UpdateMessage(")
        .expect("the pager press no longer updates the card in place (AC-020)");

    assert!(
        embed < update,
        "the auto-embed decision is applied after the response is built, so it \
         never reaches the message the card is rewritten with (AC-020)"
    );

    // The create half of the symmetry. If page 0 ever stops going through the
    // governed send path, this test would be pinning a decision against a
    // create that no longer embeds, and the pair would be symmetric by
    // accident rather than by rule.
    let handler = flat(&handler_src());
    assert!(
        handler.contains("writes::send(") && handler.contains("Class::Final)"),
        "page 0 no longer goes through the governed send path, so the embed \
         decision the press mirrors is no longer the one the create made \
         (AC-020)"
    );
}
