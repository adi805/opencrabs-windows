//! Picking one of a sender's two addresses, the same way every time (#1533).
//!
//! The cases that matter are the ones where the obvious rule and the correct
//! rule disagree, so most of these pin a PN-addressed sender: "prefer
//! `sender_alt`" gets every LID case right and every PN case backwards, which
//! is why it survived review.

use crate::channels::whatsapp::identity::{canonical_chat_jid, canonical_chat_key, canonical_user};
use wacore::types::message::MessageSource;
use wacore_binary::jid::Jid;

fn jid(s: &str) -> Jid {
    s.parse().unwrap_or_else(|e| panic!("parse {s}: {e:?}"))
}

#[test]
fn a_pn_sender_keys_on_its_own_number_not_its_lid_twin() {
    // THE inversion. `sender_alt` for a PN sender is the LID, so preferring it
    // files a phone-number contact under an opaque id, and every allow list,
    // blocklist and session label in the channel is written in phone numbers.
    let canonical = canonical_user(
        &jid("351933536442@s.whatsapp.net"),
        Some(&jid("83911178752119@lid")),
    );
    assert_eq!(canonical, "351933536442");
}

#[test]
fn a_lid_sender_keys_on_the_pn_twin_the_stanza_carried() {
    let canonical = canonical_user(
        &jid("83911178752119@lid"),
        Some(&jid("351933536442@s.whatsapp.net")),
    );
    assert_eq!(canonical, "351933536442");
}

#[test]
fn both_addressing_modes_of_one_contact_agree() {
    // The whole point of a canonical key: the same person reaching us two ways
    // must produce one value, or they get two sessions and an approval that can
    // never be resolved.
    let pn = jid("351933536442@s.whatsapp.net");
    let lid = jid("83911178752119@lid");
    assert_eq!(
        canonical_user(&pn, Some(&lid)),
        canonical_user(&lid, Some(&pn))
    );
}

#[test]
fn a_lid_sender_with_no_twin_keeps_its_lid() {
    // Not every stanza carries `participant_pn`. The choice there is that LID
    // or nothing, and nothing is not a key.
    assert_eq!(
        canonical_user(&jid("83911178752119@lid"), None),
        "83911178752119"
    );
}

#[test]
fn a_pn_sender_with_no_twin_keeps_its_number() {
    assert_eq!(
        canonical_user(&jid("351933536442@s.whatsapp.net"), None),
        "351933536442"
    );
}

#[test]
fn the_hosted_variants_sort_into_the_right_families() {
    // `is_pn_family` is `Pn | Hosted` and `is_lid_family` is `Lid | HostedLid`,
    // so a two-branch if/else on either one alone puts the hosted pair in the
    // wrong half. Asking both is what keeps these right.
    assert_eq!(
        canonical_user(&jid("351933536442@hosted"), Some(&jid("839111@lid"))),
        "351933536442",
        "hosted is PN-family, so it is already the number"
    );
    assert_eq!(
        canonical_user(
            &jid("83911178752119@hosted.lid"),
            Some(&jid("351933536442@s.whatsapp.net"))
        ),
        "351933536442",
        "hosted.lid is LID-family, so it resolves through its twin"
    );
}

#[test]
fn an_alt_that_is_not_a_phone_number_is_not_used() {
    // Defensive: the parser should never hand a LID sender a LID alt, but a key
    // built from an unchecked field would be silently wrong rather than loudly
    // wrong if it ever did.
    assert_eq!(
        canonical_user(&jid("83911178752119@lid"), Some(&jid("99999@lid"))),
        "83911178752119"
    );
}

#[test]
fn a_linked_device_keys_the_same_as_the_phone() {
    // WhatsApp Web and Desktop carry a `:34` device suffix. One person on two
    // devices is one person.
    assert_eq!(
        canonical_user(&jid("351933536442:34@s.whatsapp.net"), None),
        canonical_user(&jid("351933536442@s.whatsapp.net"), None)
    );
}

#[test]
fn a_group_jid_is_left_alone() {
    // Group, broadcast and newsletter servers are in neither identity family:
    // they are not a person with two names, so there is nothing to canonicalize.
    assert_eq!(canonical_user(&jid("120363001@g.us"), None), "120363001");
}

// --- chat keys (#F2) -------------------------------------------------------
//
// `channel_messages.channel_chat_id` is the key `whatsapp_history` looks up by,
// so it has to agree with what a human -- and the session label -- calls the
// chat. These pin the LID-addressed case that produced the bug: every captured
// DM landed under `…@lid` while the session for the same conversation was
// `wa-<phone>`, so querying the phone form matched nothing.

#[test]
fn a_lid_addressed_dm_keys_on_the_phone_number_the_stanza_carried() {
    assert_eq!(
        canonical_chat_jid(
            &jid("210122852823201@lid"),
            Some(&jid("210122852823201@lid")),
            Some(&jid("6282361295343@s.whatsapp.net")),
        ),
        "6282361295343@s.whatsapp.net"
    );
}

#[test]
fn a_lid_addressed_dm_with_no_pn_twin_keeps_the_lid() {
    // Nothing better to key on: the stanza named only the LID, so inventing a
    // phone number would be worse than keeping the address that arrived.
    assert_eq!(
        canonical_chat_jid(
            &jid("210122852823201@lid"),
            Some(&jid("210122852823201@lid")),
            None
        ),
        "210122852823201@lid"
    );
}

#[test]
fn a_pn_addressed_dm_keeps_the_phone_form() {
    assert_eq!(
        canonical_chat_jid(&jid("6282361295343@s.whatsapp.net"), None, None),
        "6282361295343@s.whatsapp.net"
    );
}

#[test]
fn a_group_keeps_its_own_address_even_with_a_phone_twin_in_hand() {
    // A group is not a person with two names. Keying it on a member's number
    // would merge the room into that member's DM thread.
    assert_eq!(
        canonical_chat_jid(
            &jid("120363001@g.us"),
            Some(&jid("6282361295343@s.whatsapp.net")),
            Some(&jid("6282361295343@s.whatsapp.net")),
        ),
        "120363001@g.us"
    );
}

#[test]
fn both_directions_of_the_owner_chat_produce_one_key() {
    // THE bug. Inbound carries the peer in `sender`, our own echo carries it in
    // `recipient`; if the two paths disagree the transcript splits in half and
    // a history query sees only one side of it.
    let inbound = canonical_chat_key(&MessageSource {
        chat: jid("210122852823201@lid"),
        sender: jid("210122852823201@lid"),
        sender_alt: Some(jid("6282361295343@s.whatsapp.net")),
        ..Default::default()
    });
    let outbound = canonical_chat_key(&MessageSource {
        chat: jid("210122852823201@lid"),
        sender: jid("6285270679623@s.whatsapp.net"),
        is_from_me: true,
        recipient: Some(jid("210122852823201@lid")),
        recipient_alt: Some(jid("6282361295343@s.whatsapp.net")),
        ..Default::default()
    });
    assert_eq!(inbound, "6282361295343@s.whatsapp.net");
    assert_eq!(inbound, outbound);
}

#[test]
fn our_own_echo_never_keys_the_chat_on_our_own_number() {
    // `is_from_me` means `sender` is US. A shared code path that read
    // `sender_alt` regardless would file the owner's chat under the bot's own
    // number: a different, plausible-looking, completely wrong thread.
    let key = canonical_chat_key(&MessageSource {
        chat: jid("210122852823201@lid"),
        sender: jid("6285270679623@s.whatsapp.net"),
        sender_alt: Some(jid("83911178752119@lid")),
        is_from_me: true,
        recipient: None,
        recipient_alt: None,
        ..Default::default()
    });
    assert_eq!(key, "210122852823201@lid");
}

#[test]
fn a_hosted_phone_twin_folds_to_the_plain_pn_form() {
    // `is_pn_family` spans Pn and Hosted. Both name the same person, and the
    // session label is the plain form, so the key is too.
    assert_eq!(
        canonical_chat_jid(
            &jid("210122852823201@lid"),
            Some(&jid("210122852823201@lid")),
            Some(&jid("6282361295343@hosted")),
        ),
        "6282361295343@s.whatsapp.net"
    );
}
