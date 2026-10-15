//! `channels.whatsapp.bot_owner` must drive the owner JID.
//!
//! The owner used to be derived from `allowed_phones[0]` only, which made
//! `bot_owner` a silent no-op: setting it changed nothing, and
//! `config_manager reload` could not help because the value was fixed when the
//! channel started. These cases pin the precedence that the config field's own
//! doc comment promises ("When unset, defaults to the first entry in
//! `allowed_phones`").

use crate::channels::whatsapp::agent::derive_owner_jid;

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn bot_owner_wins_over_allowed_phones() {
    let owner = derive_owner_jid(&v(&["+6282361295343"]), &v(&["+6285270679623"]));
    assert_eq!(
        owner.as_deref(),
        Some("6282361295343@s.whatsapp.net"),
        "an explicit bot_owner entry must win over allowed_phones"
    );
}

#[test]
fn falls_back_to_first_allowed_phone_when_bot_owner_is_empty() {
    let owner = derive_owner_jid(&[], &v(&["+6285270679623", "+6282361295343"]));
    assert_eq!(
        owner.as_deref(),
        Some("6285270679623@s.whatsapp.net"),
        "an empty bot_owner must fall back to the first allowed phone"
    );
}

#[test]
fn no_owner_when_both_lists_are_empty() {
    assert_eq!(
        derive_owner_jid(&[], &[]),
        None,
        "no configured owner must stay None rather than inventing one"
    );
}
