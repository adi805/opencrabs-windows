//! WhatsApp per-room access and mention-only mode (#161).
//!
//! What is pinned here is the decision logic: which room mode silences a room,
//! how a JID is reduced to the part that gets compared, and whether a mention
//! is read off the shape WhatsApp actually sends it in.
//!
//! `handle_message` itself is not exercised. It needs a live `Client`, and a
//! test that stubbed one out would be asserting on the stub rather than on the
//! channel. The wiring from config to these functions is covered by the config
//! tests below; nothing here pretends to prove the handler calls them.

use crate::channels::whatsapp::handler::{
    wa_allowed_for_room, wa_is_mentioned, wa_jid_user, wa_own_identities, wa_room_is_open,
    wa_room_should_respond, wa_should_respond,
};
use crate::config::types::{RespondTo, WaResponsePolicy, WhatsAppConfig, WhatsAppGroupConfig};
use waproto::whatsapp::message::{ExtendedTextMessage, FutureProofMessage, ImageMessage};

/// The paired account, spelled the way `owner_jid` stores it.
const BOT: &str = "628111111111@s.whatsapp.net";
const ROOM: &str = "120363012345678901@g.us";

fn ctx_mentioning(jids: &[&str]) -> waproto::whatsapp::ContextInfo {
    waproto::whatsapp::ContextInfo {
        mentioned_jid: jids.iter().map(|j| (*j).to_string()).collect(),
        ..Default::default()
    }
}

/// `None` leaves the field absent, which is how a message with no context info
/// actually arrives: distinct from a present-but-empty one.
fn text_message(ctx: Option<waproto::whatsapp::ContextInfo>) -> waproto::whatsapp::Message {
    waproto::whatsapp::Message {
        extended_text_message: ExtendedTextMessage {
            text: Some("halo".to_string()),
            context_info: ctx.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

#[test]
fn a_dm_is_never_silenced_by_a_room_mode() {
    // `dm_only` has to read as "ignore rooms", not "ignore DMs". The other
    // reading leaves a `dm_only` install unable to answer the one thing that
    // mode is named for, so it is the reading worth pinning.
    for mode in [
        RespondTo::All,
        RespondTo::DmOnly,
        RespondTo::Mention,
        RespondTo::Auto,
    ] {
        assert!(
            wa_room_should_respond(mode, false, false),
            "{mode:?} must not gate a DM"
        );
    }
}

#[test]
fn the_room_mode_decides_whether_the_bot_speaks() {
    assert!(wa_room_should_respond(RespondTo::All, true, false));
    // `dm_only` means the bot does not talk in rooms at all, called or not.
    assert!(!wa_room_should_respond(RespondTo::DmOnly, true, true));
    assert!(!wa_room_should_respond(RespondTo::Mention, true, false));
    assert!(wa_room_should_respond(RespondTo::Mention, true, true));
    // `auto` degrades to mention-only on this channel: there is no per-room
    // sender census to flip on, so it cannot mean anything else here.
    assert!(!wa_room_should_respond(RespondTo::Auto, true, false));
    assert!(wa_room_should_respond(RespondTo::Auto, true, true));
}

#[test]
fn a_jid_is_compared_by_its_user_part() {
    assert_eq!(wa_jid_user("628111111111@s.whatsapp.net"), "628111111111");
    assert_eq!(
        wa_jid_user("628111111111:12@s.whatsapp.net"),
        "628111111111"
    );
    assert_eq!(wa_jid_user("+628111111111"), "628111111111");
    assert_eq!(wa_jid_user("628111111111"), "628111111111");
    assert_eq!(wa_jid_user(""), "");
}

#[test]
fn the_bots_identities_cover_the_pair_and_the_operators() {
    let ops = vec!["+628222222222".to_string(), "628111111111".to_string()];
    let own = wa_own_identities(Some(BOT), &ops);
    // The operator list repeats the paired number in another spelling. It must
    // not become a second entry, or a mention check ends up with duplicates it
    // never uses.
    assert_eq!(
        own,
        vec!["628111111111".to_string(), "628222222222".to_string()]
    );
}

#[test]
fn no_owner_and_no_operators_means_no_identity_to_match() {
    assert!(wa_own_identities(None, &[]).is_empty());
    // An empty owner string must not become an empty identity, which would
    // then match every message with a malformed mention.
    assert!(wa_own_identities(Some(""), &[]).is_empty());
}

#[test]
fn a_mention_in_a_text_message_is_seen() {
    let msg = text_message(Some(ctx_mentioning(&[BOT])));
    assert!(wa_is_mentioned(&msg, Some(BOT), &[]));
}

#[test]
fn a_mention_addressed_by_an_operator_number_is_seen() {
    let msg = text_message(Some(ctx_mentioning(&["628222222222@s.whatsapp.net"])));
    let ops = vec!["+628222222222".to_string()];
    // The paired account is a different number here, so only the operator
    // entry can match.
    assert!(wa_is_mentioned(&msg, Some(BOT), &ops));
}

#[test]
fn a_mention_with_a_device_suffix_is_seen() {
    let msg = text_message(Some(ctx_mentioning(&["628111111111:12@s.whatsapp.net"])));
    assert!(wa_is_mentioned(&msg, Some(BOT), &[]));
}

#[test]
fn a_caption_mention_counts_the_same_as_a_text_one() {
    // The list rides on the media messages too. A mention in a photo caption
    // is the same act as one in a text line, and reading only the text
    // carriers would drop it.
    let msg = waproto::whatsapp::Message {
        image_message: ImageMessage {
            caption: Some("@bot lihat ini".to_string()),
            context_info: ctx_mentioning(&[BOT]).into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    assert!(wa_is_mentioned(&msg, Some(BOT), &[]));
}

#[test]
fn the_last_carrier_in_the_chain_is_reached() {
    // The lookup tries extended text, then image, then video, then document.
    // Document is the end of that chain: if it were missing from the list, the
    // other cases would still pass and only this one would fail.
    let msg = waproto::whatsapp::Message {
        document_message: waproto::whatsapp::message::DocumentMessage {
            context_info: ctx_mentioning(&[BOT]).into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    assert!(wa_is_mentioned(&msg, Some(BOT), &[]));
}

#[test]
fn a_wrapped_message_is_unwrapped_before_the_mention_is_read() {
    // Disappearing and view-once messages arrive inside a wrapper. A mention
    // inside one is still a mention, and reading the wrapper would find no
    // context info at all.
    let inner = text_message(Some(ctx_mentioning(&[BOT])));
    let msg = waproto::whatsapp::Message {
        view_once_message: FutureProofMessage {
            message: inner.into(),
        }
        .into(),
        ..Default::default()
    };
    assert!(wa_is_mentioned(&msg, Some(BOT), &[]));
}

#[test]
fn a_message_without_context_info_is_not_a_mention() {
    assert!(!wa_is_mentioned(&text_message(None), Some(BOT), &[]));
}

#[test]
fn an_empty_mention_list_is_not_a_mention() {
    assert!(!wa_is_mentioned(
        &text_message(Some(ctx_mentioning(&[]))),
        Some(BOT),
        &[]
    ));
}

#[test]
fn mentioning_someone_else_is_not_mentioning_the_bot() {
    let msg = text_message(Some(ctx_mentioning(&["628999999999@s.whatsapp.net"])));
    assert!(!wa_is_mentioned(&msg, Some(BOT), &[]));
}

#[test]
fn a_room_can_override_the_respond_mode() {
    let cfg = WhatsAppConfig {
        groups: std::collections::HashMap::from([(
            ROOM.to_string(),
            WhatsAppGroupConfig {
                respond_to: Some(RespondTo::Mention),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    assert_eq!(cfg.respond_to_for(ROOM), RespondTo::Mention);
    // Another room is untouched by that override.
    assert_eq!(cfg.respond_to_for("999@g.us"), RespondTo::All);
}

#[test]
fn a_room_with_no_override_answers_everything() {
    // The default is `All`, not Telegram's `Mention`. WhatsApp has always
    // answered every allowed sender in every group it is in, so defaulting a
    // room to mention-only would silently stop an existing install answering
    // where it works today.
    let cfg = WhatsAppConfig {
        groups: std::collections::HashMap::from([(
            ROOM.to_string(),
            WhatsAppGroupConfig::default(),
        )]),
        ..Default::default()
    };
    assert_eq!(cfg.respond_to_for(ROOM), RespondTo::All);
    assert_eq!(
        WhatsAppConfig::default().respond_to_for(ROOM),
        RespondTo::All
    );
}

#[test]
fn a_room_table_parses_from_toml() {
    let wa: WhatsAppConfig = toml::from_str(
        r#"
        enabled = true
        allowed_phones = ["+628100000000"]

        [groups."120363012345678901@g.us"]
        name = "Ops"
        allowed_phones = ["+6281234567890"]
        respond_to = "mention"
        open = true
        "#,
    )
    .unwrap();

    let room = wa.groups.get(ROOM).expect("the room table parsed");
    assert_eq!(room.name.as_deref(), Some("Ops"));
    assert_eq!(room.allowed_phones, vec!["+6281234567890".to_string()]);
    assert_eq!(room.respond_to, Some(RespondTo::Mention));
    assert!(room.open);
    // The channel-wide list is still there; a room ADDS to it rather than
    // replacing it.
    assert_eq!(wa.allowed_phones, vec!["+628100000000".to_string()]);
    assert_eq!(wa.respond_to_for(ROOM), RespondTo::Mention);
}

#[test]
fn a_room_table_may_name_only_what_it_overrides() {
    let wa: WhatsAppConfig = toml::from_str(
        r#"
        [groups."120363012345678901@g.us"]
        respond_to = "dm_only"
        "#,
    )
    .unwrap();

    let room = wa.groups.get(ROOM).expect("the room table parsed");
    assert_eq!(room.respond_to, Some(RespondTo::DmOnly));
    assert!(room.allowed_phones.is_empty());
    assert!(!room.open);
    assert!(room.name.is_none());
}

#[test]
fn a_group_name_that_reads_as_a_number_still_loads() {
    // Group names are ordinary text, and "2026" is one. A hand-edited config
    // will carry it unquoted, and the whole config load must not fail over a
    // field that is pure display metadata.
    let wa: WhatsAppConfig = toml::from_str(
        r#"
        [groups."120363012345678901@g.us"]
        name = 2026
        "#,
    )
    .unwrap();
    assert_eq!(
        wa.groups.get(ROOM).and_then(|g| g.name.as_deref()),
        Some("2026")
    );
}

#[test]
fn an_unconfigured_install_has_no_rooms() {
    // Empty by default: a fresh install behaves exactly as it did before this
    // feature existed.
    assert!(WhatsAppConfig::default().groups.is_empty());
}

#[test]
fn a_room_list_adds_to_the_channel_list() {
    let room = WhatsAppGroupConfig {
        allowed_phones: vec!["+6281234567890".to_string()],
        ..Default::default()
    };
    let merged = wa_allowed_for_room(&["+628100000000".to_string()], Some(&room));
    assert_eq!(
        merged,
        vec!["+628100000000".to_string(), "+6281234567890".to_string()]
    );
}

#[test]
fn a_room_with_no_table_keeps_the_channel_list() {
    let global = vec!["+628100000000".to_string()];
    assert_eq!(wa_allowed_for_room(&global, None), global);
}

#[test]
fn an_open_room_admits_a_member_the_acl_would_refuse() {
    // The composition the handler performs. A room's `open` has to carry a
    // sender the channel-wide allow list refuses, or the switch does nothing,
    // and it has to do that without the sender being listed anywhere.
    let room = WhatsAppGroupConfig {
        open: true,
        ..Default::default()
    };
    let allowed = wa_allowed_for_room(&["+628100000000".to_string()], Some(&room));
    assert!(!wa_should_respond(
        WaResponsePolicy::Allowlist,
        false,
        "628999999999",
        None,
        "120363012345678901",
        &allowed,
        &[],
    ));
    assert!(wa_room_is_open(Some(&room), false));
}

#[test]
fn an_open_room_does_not_admit_the_bots_own_message() {
    // `is_from_me` in a room is the paired account talking, which this gate
    // stays silent in. Only a member asking gets the room's blanket pass.
    let room = WhatsAppGroupConfig {
        open: true,
        ..Default::default()
    };
    assert!(wa_room_is_open(Some(&room), false));
    assert!(!wa_room_is_open(Some(&room), true));
}

#[test]
fn a_room_that_is_not_open_admits_nobody_on_its_own() {
    assert!(!wa_room_is_open(None, false));
    assert!(!wa_room_is_open(
        Some(&WhatsAppGroupConfig::default()),
        false
    ));
}
