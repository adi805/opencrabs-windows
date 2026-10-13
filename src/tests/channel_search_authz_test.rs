//! Authorization for channel history reads (#215).
//!
//! `channel_search` is registered in the shared core tool registry and needs no
//! approval, while the captured message store holds DMs and unrelated group
//! history. So the tool cannot treat "the caller passed a chat id" as
//! permission: the caller is the model, and a guest turn could otherwise name a
//! private DM and read it. The boundary is the session's audience
//! classification, resolved through the same durable decision the memory gates
//! use.
//!
//! These tests pin the decision table and, in source form, the fact that every
//! operation consults it: the four operations are enumerated from the tool's own
//! schema, so a fifth operation added later cannot skip the gate silently.

use crate::brain::tools::channel_search::{ChatReadScope, chat_id_from_key};
use crate::config::types::MemoryConfig;
use crate::tests::source_shape::{flatten, read_in};

// ── the decision table ──────────────────────────────────────────────

#[test]
fn only_the_owners_own_session_reads_across_chats() {
    let owner = ChatReadScope::CrossChat;
    assert!(owner.deny_listing().is_none());
    assert!(owner.check(None).is_ok(), "the owner may read across chats");
    assert!(owner.check(Some("any-chat")).is_ok());
}

#[test]
fn a_guest_session_is_limited_to_its_own_chat() {
    let guest = ChatReadScope::OwnChat("-100".to_string());
    assert!(
        guest.check(Some("-100")).is_ok(),
        "the guest's own chat stays readable"
    );
    assert!(
        guest.check(Some("-999")).is_err(),
        "#215: another chat id must not widen access"
    );
    assert!(
        guest.check(None).is_err(),
        "#215: omitting the chat id must not widen access either"
    );
    assert!(
        guest.deny_listing().is_some(),
        "#215: list_chats enumerates DMs, so it is owner-only"
    );
}

#[test]
fn an_unresolvable_guest_scope_refuses_rather_than_guessing() {
    let refused = ChatReadScope::Refused;
    assert!(
        refused.check(Some("-100")).is_err(),
        "#215: with no derivable chat binding there is nothing to authorize"
    );
    assert!(refused.check(None).is_err());
    assert!(refused.deny_listing().is_some());
}

#[test]
fn the_cross_chat_policy_defaults_to_deny() {
    let cfg = MemoryConfig::default();
    assert!(
        !cfg.channel_cross_chat_allowed_in_shared,
        "#215: cross-chat reads from a shared session must be opt-in"
    );
    let cfg = toml::from_str::<MemoryConfig>("channel_cross_chat_allowed_in_shared = true")
        .expect("valid [memory] TOML");
    assert!(cfg.channel_cross_chat_allowed_in_shared);
}

// ── the chat binding ────────────────────────────────────────────────

#[test]
fn the_session_chat_binding_parses_both_key_shapes() {
    assert_eq!(
        chat_id_from_key("[chat:-1001234]").as_deref(),
        Some("-1001234")
    );
    assert_eq!(
        chat_id_from_key("[chat:-1001234:topic:55]").as_deref(),
        Some("-1001234"),
        "a topic suffix must not become part of the chat id"
    );
    // Anything that is not a chat binding yields no scope, which refuses.
    for key in ["", "guild-1", "[chat:]", "[chat:  ]"] {
        assert_eq!(chat_id_from_key(key), None, "key={key:?}");
    }
}

// ── the wiring ──────────────────────────────────────────────────────

/// Every operation the schema declares must consult the scope. This is
/// enumerated from the tool's own `enum` list rather than written out by hand,
/// so adding a fifth operation without a gate fails here instead of shipping.
#[test]
fn every_declared_operation_consults_the_read_scope() {
    let src = flatten(read_in("src/brain/tools", "channel_search.rs"));

    // The declared operations, read from the schema literal.
    let schema_start = src.find("\"operation\":{").expect("operation in schema");
    let schema = &src[schema_start..];
    let enum_start = schema.find("\"enum\":[").expect("enum in schema") + 8;
    let enum_end = schema[enum_start..].find(']').expect("enum close");
    let ops: Vec<String> = schema[enum_start..enum_start + enum_end]
        .split("\",\"")
        .map(|s| s.trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();

    assert!(
        ops.len() >= 4,
        "expected the four declared operations, got {ops:?}"
    );
    assert!(
        ops.iter().any(|o| o == "list_chats"),
        "the schema must still declare list_chats; got {ops:?}"
    );

    // Every arm must reach the scope decision. `list_chats` uses deny_listing,
    // the three retrievals use check, so require the arm to mention read_scope.
    for op in &ops {
        let arm = format!("\"{op}\"=>{{");
        let start = src
            .find(&arm)
            .unwrap_or_else(|| panic!("arm {op} not found in the dispatch"));
        let body = &src[start..];
        // The arm runs to the next arm marker; a coarse window is enough to
        // tell whether the gate is present before the first repository call.
        let window = &body[..body.len().min(1200)];
        assert!(
            window.contains("read_scope"),
            "arm {op} never consults read_scope (#215): an operation without the \
             gate reads chats the caller is not authorized for"
        );
    }
}
