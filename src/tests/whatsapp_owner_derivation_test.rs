//! `channels.whatsapp.bot_owner` must drive the owner JID.
//!
//! The owner used to be derived from `allowed_phones[0]` only, which made
//! `bot_owner` a silent no-op: setting it changed nothing, and
//! `config_manager reload` could not help because the value was fixed when the
//! channel started. These cases pin the precedence that the config field's own
//! doc comment promises ("When unset, defaults to the first entry in
//! `allowed_phones`").

use crate::channels::whatsapp::agent::{derive_owner_jid, owner_after_reload};

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

#[test]
fn reload_reports_a_changed_owner() {
    let new = owner_after_reload(
        &v(&["+6282361295343"]),
        &v(&["+6285270679623"]),
        Some("6285270679623@s.whatsapp.net"),
    );
    assert_eq!(
        new.as_deref(),
        Some("6282361295343@s.whatsapp.net"),
        "a reload that changes the owner must report the new JID, never a no-op"
    );
}

#[test]
fn reload_reports_nothing_when_the_owner_is_already_current() {
    assert_eq!(
        owner_after_reload(
            &v(&["+6282361295343"]),
            &v(&["+6285270679623"]),
            Some("6282361295343@s.whatsapp.net"),
        ),
        None,
        "an unchanged owner must report no-op so the watcher logs nothing"
    );
}

#[test]
fn reload_publishes_a_previously_unset_owner() {
    assert_eq!(
        owner_after_reload(&[], &v(&["+6282361295343"]), None).as_deref(),
        Some("6282361295343@s.whatsapp.net"),
        "an owner that was unset at start must be published by the reload"
    );
}

#[test]
fn reload_never_invents_an_owner_from_nothing() {
    assert_eq!(
        owner_after_reload(&[], &[], Some("6285270679623@s.whatsapp.net")),
        None,
        "an empty config must not clear or invent an owner"
    );
}
