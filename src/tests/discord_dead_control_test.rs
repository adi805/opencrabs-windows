//! A control that can no longer answer must not stay on the card (#1880, L3
//! and M4).
//!
//! Two ways a card outlives the thing behind it:
//!
//! * the store forgot the entry, so the press answers "aged out" while the row
//!   keeps offering the action it just refused;
//! * the pager store evicted an older answer to make room, and the card it
//!   pushed out is still sitting in its channel with live-looking arrows.
//!
//! Both are the same defect from the reader's side: a control that looks
//! pressable and cannot succeed, so the reader presses again and collects
//! another notice. The fix is the same in both places, and it is deliberately
//! narrow: take the controls off the card and leave its text alone, because the
//! text is what the reader is looking at.
//!
//! These are source-shape tests. Every arm they pin lives inside
//! `Interaction::create_response` handling behind an `Http` call, so the
//! decision is not reachable from a unit test. A doc comment cannot fail a
//! build when someone drops the strip; these can, which is the whole reason
//! they exist.

use crate::tests::source_shape::{flatten, read_in};

/// The named file in the discord channel module, read from the crate root.
fn read(name: &str) -> String {
    read_in("src/channels/discord", name)
}

/// The pager press: from the prefix match that opens it to the tool-group match
/// that closes it. `agent.rs` answers components in a dozen places, so a
/// whole-file scan cannot tell this arm from an unrelated one.
fn pager_arm() -> String {
    let agent = read("agent.rs");
    let start = agent
        .find("strip_prefix(super::long_answer::PAGER_PREFIX)")
        .expect("agent.rs no longer matches PAGER_PREFIX: the pager press moved");
    let rest = &agent[start..];
    let end = rest
        .find(r#"strip_prefix("toolgroup:")"#)
        .expect("the tool-group branch that closes the pager press moved");
    rest[..end].to_string()
}

/// The tool-group toggle branch, which has two dead ends of its own: a
/// `custom_id` this build cannot parse, and a message id the store has aged out.
fn tool_group_arm() -> String {
    let agent = read("agent.rs");
    let start = agent
        .find(r#"strip_prefix("toolgroup:")"#)
        .expect("agent.rs no longer matches the tool-group prefix: the toggle moved");
    let rest = &agent[start..];
    let end = rest
        .find("is_plan_callback(custom_id)")
        .expect("the plan branch that closes the tool-group toggle moved");
    rest[..end].to_string()
}

/// A press whose pages left the store answers with a notice, and the notice is
/// ephemeral on purpose: the card still shows the page the reader is looking
/// at, and overwriting it would destroy that page. The arrows are a different
/// promise. They were kept only so the store could serve them, and it no longer
/// can, so leaving them on offers a press that can never land (#1880, L3).
///
/// The gate is the store's answer, not the rendered body: an out-of-range page
/// still has its pages, and its row still works, so a strip gated on the body
/// would remove a control that is fine.
#[test]
fn a_pager_whose_pages_aged_out_takes_its_arrows_off() {
    let arm = flatten(pager_arm());
    assert!(
        arm.contains("strip_controls("),
        "the pager's aged-out arm no longer takes the arrows off, so the card \
         keeps a control whose only outcome is another ephemeral notice (#1880, \
         L3)"
    );
    assert!(
        arm.contains("ifpages.is_none()"),
        "the strip is no longer gated on the store's answer. An out-of-range \
         page still has its pages and its row still works, so gating on the \
         rendered body would strip a control that is fine"
    );
}

/// The toggle has two ways to be dead, and both must take the button off: a
/// `custom_id` from a build that no longer exists, and a message id the store
/// has aged out. The card keeps its last rendering in both cases, because the
/// notice is ephemeral; only the control goes (#1880, L3).
#[test]
fn a_tool_group_that_aged_out_takes_its_button_off() {
    let branch = flatten(tool_group_arm());
    assert_eq!(
        branch.matches("strip_controls(").count(),
        2,
        "both dead ends in the tool-group toggle must take the button off: the \
         unparseable custom_id and the aged-out toggle. A card left holding a \
         live button whose only outcome is an ephemeral notice is the defect \
         (#1880, L3)"
    );
    assert!(
        branch.contains("comp.message.id"),
        "the unparseable-id arm must strip the card the press came from: it has \
         no parsed message id, and the interaction itself names the message"
    );
}

/// The strip is the repair, so it has to be shaped like one: no content (a
/// message edit only touches the fields it names, so the card survives), no
/// components (the row is the thing being removed), and a class the governor
/// cannot drop. A droppable class would let the governor throw away the one
/// write that removes the dead control, restoring the defect (#1880, L3).
#[test]
fn the_strip_drops_the_controls_and_keeps_the_card() {
    let writes = read("writes.rs");
    let start = writes
        .find("pub(crate) async fn strip_controls(")
        .expect("the dead-control strip is gone");
    let rest = &writes[start..];
    let end = rest
        .find("\n}\n")
        .expect("the dead-control strip has no body end");
    let body = flatten(&rest[..end]);

    assert!(
        body.contains("EditMessage::new().components(Vec::new())"),
        "the strip no longer clears the components, so the dead control stays \
         on the card"
    );
    assert!(
        body.contains("Class::Final"),
        "the strip must be admitted as `Class::Final`: this write IS the \
         repair, and a droppable class lets the governor drop it"
    );
    assert!(
        !body.contains(".content("),
        "the strip must not set content. The card's text is what the reader is \
         looking at, and the notice is ephemeral precisely so that text \
         survives; a content-bearing edit would overwrite it"
    );
}

/// Eviction is the one moment the pager store can still name the card it is
/// forgetting, so it hands the evicted entries back with the channel they live
/// in. The card stays on screen either way, so an eviction that stays silent
/// leaves live-looking arrows over an answer nothing can serve, and the reader
/// keeps pressing (#1880, M4).
#[test]
fn the_pager_store_hands_back_what_it_evicted() {
    let long_answer = read("long_answer.rs");
    let start = long_answer
        .find("pub(crate) async fn store_long_answer(")
        .expect("the pager store moved");
    let rest = &long_answer[start..];
    let sig_end = rest.find('{').expect("the pager store has no body");
    let signature = flatten(&rest[..sig_end]);

    assert!(
        signature.contains("channel:ChannelId"),
        "the store no longer takes the channel the card lives in, so eviction \
         cannot reach the card it forgets: the evicted card need not be in the \
         channel doing the evicting (#1880, M4)"
    );
    assert!(
        signature.contains("->Vec<(u64,ChannelId)>"),
        "the store no longer hands back what it evicted, so the caller has \
         nothing to strip and the forgotten card keeps its arrows (#1880, M4)"
    );
}

/// The eviction result has to be drained where the store is called, or the
/// returned list is decoration. The drain goes through `writes::edit` with an
/// empty component list and a class the governor cannot drop, for the same
/// reason the aged-out strip does: the write removing a dead control must not
/// itself be droppable (#1880, M4).
#[test]
fn eviction_takes_the_row_off_the_card_it_forgets() {
    let handler = flatten(read("handler.rs"));
    let store = handler
        .find("store_long_answer(mid,target,chunks.clone())")
        .expect("the pager store call moved: this test pins what happens to its result");
    let after = &handler[store..];
    let drain = after.find("for(gone,gone_channel)inevicted").expect(
        "the eviction result is no longer drained, so an evicted card keeps \
         its arrows over an answer nothing can serve (#1880, M4)",
    );
    let tail = &after[drain..];
    let body_end = tail
        .find('}')
        .expect("the eviction drain loop has no body end");
    let body = &tail[..body_end];

    assert!(
        body.contains("writes::edit("),
        "the drain no longer edits the evicted card, so its arrows stay live"
    );
    assert!(
        body.contains("components(Vec::new())"),
        "the drain no longer clears the row it came to remove"
    );
    assert!(
        body.contains("Class::Final"),
        "the eviction strip must not be droppable: a governor drop leaves the \
         arrows on a card the store has already forgotten"
    );
}
