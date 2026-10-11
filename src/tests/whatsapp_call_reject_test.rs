//! Call handling on the WhatsApp channel. This device has no call UI, so an
//! incoming 1:1 offer is declined as BUSY: the `reason` attribute is the whole
//! point, because a reject without one reads as the callee declining and tears
//! the call down on every device, the owner's own phone included. The event arms
//! need a live socket, so the wire shape and the alert wording are pinned here
//! and the wiring is pinned source-level.

use crate::channels::whatsapp::agent::{
    busy_reject_node, call_alert, caller_label, missed_call_alert,
};
use wacore::types::call::MissedReason;
use wacore_binary::Node;
use wacore_binary::jid::Jid;

const AGENT: &str = include_str!("../channels/whatsapp/agent.rs");

fn jid(s: &str) -> Jid {
    s.parse().unwrap_or_else(|e| panic!("parse {s}: {e:?}"))
}

/// Read one attribute off a node as an owned string. The node keeps its attrs as
/// `(Cow<str>, NodeValue)` pairs, and a `NodeValue` may hold a JID rather than a
/// raw string, so `as_str` is what normalises both shapes.
fn attr(node: &Node, key: &str) -> Option<String> {
    node.attrs
        .0
        .iter()
        .find(|(k, _)| **k == *key)
        .map(|(_, v)| v.as_str().into_owned())
}

#[test]
fn a_busy_reject_carries_the_reason_that_keeps_the_other_devices_ringing() {
    let peer = jid("15551234567@s.whatsapp.net");
    let creator = jid("15551234567:12@s.whatsapp.net");
    let peer_str = peer.to_string();
    let creator_str = creator.to_string();

    let node = busy_reject_node("CALLID", &peer, &creator, "WRAP");

    assert_eq!(node.tag.as_ref(), "call", "{node:?}");
    assert_eq!(attr(&node, "to").as_deref(), Some(peer_str.as_str()));
    assert_eq!(attr(&node, "id").as_deref(), Some("WRAP"));
    assert_eq!(
        node.children().map(|c| c.len()),
        Some(1),
        "the wrapper holds exactly the one action: {node:?}"
    );

    let reject = node.get_optional_child("reject").expect("a reject child");
    assert_eq!(reject.tag.as_ref(), "reject", "{reject:?}");
    assert_eq!(attr(reject, "call-id").as_deref(), Some("CALLID"));
    assert_eq!(attr(reject, "count").as_deref(), Some("0"));
    assert_eq!(
        attr(reject, "call-creator").as_deref(),
        Some(creator_str.as_str())
    );
    assert_eq!(
        attr(reject, "reason").as_deref(),
        Some("busy"),
        "a reasonless reject is a callee decline: it ends the call on every device"
    );
}

#[test]
fn a_call_alert_names_the_caller_and_the_medium() {
    let caller = jid("15551234567@s.whatsapp.net");

    let voice = call_alert(&caller, false);
    assert!(voice.contains("voice"), "{voice}");
    assert!(voice.contains("+15551234567"), "{voice}");
    assert!(
        voice.contains("other devices"),
        "the owner must know the call is still ringing elsewhere: {voice}"
    );

    let video = call_alert(&caller, true);
    assert!(video.contains("video"), "{video}");
    assert!(!video.contains("voice"), "{video}");
}

#[test]
fn a_caller_label_is_a_plus_number_for_a_phone_jid_and_the_raw_user_otherwise() {
    assert_eq!(
        caller_label(&jid("15551234567@s.whatsapp.net")),
        "+15551234567"
    );
    // A companion device: the `:device` suffix is not part of the number.
    assert_eq!(
        caller_label(&jid("15551234567:12@s.whatsapp.net")),
        "+15551234567"
    );
    // A LID carries no phone number, so there is nothing to format as one.
    let lid = caller_label(&jid("99887766554433@lid"));
    assert_eq!(lid, "99887766554433", "a LID user part is all there is");
}

#[test]
fn a_missed_call_says_whether_it_was_offline_or_the_caller_gave_up() {
    let caller = jid("15551234567@s.whatsapp.net");

    let offline = missed_call_alert(&caller, &MissedReason::Offline);
    assert!(offline.contains("offline"), "{offline}");
    assert!(offline.contains("+15551234567"), "{offline}");

    let gave_up = missed_call_alert(&caller, &MissedReason::Remote);
    assert!(gave_up.contains("gave up"), "{gave_up}");
    assert!(
        !gave_up.contains("offline"),
        "a caller who gave up was not an offline replay: {gave_up}"
    );
}

#[test]
fn the_call_arms_are_wired_and_the_reasonless_reject_is_not_used() {
    for arm in [
        "Event::IncomingCall(call)",
        "Event::MissedCall(missed)",
        "Event::CallEndedElsewhere(ended)",
    ] {
        assert!(
            AGENT.contains(arm),
            "{arm} is missing: the event falls into the debug catch-all"
        );
    }
    assert!(
        AGENT.matches("busy_reject_node(").count() >= 2,
        "busy_reject_node is defined but never sent"
    );
    assert!(
        AGENT.contains("decline_incoming_call("),
        "the IncomingCall arm no longer declines the offer"
    );
    assert!(
        !AGENT.contains("voip().reject"),
        "the library's reasonless reject is back: it ends the call on every device"
    );
}
