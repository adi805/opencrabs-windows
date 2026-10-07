use crate::channels::telegram::cowork::*;

#[test]
fn parse_startgroup_valid() {
    assert_eq!(parse_startgroup_param("cowork_abc123"), Some("abc123"));
}

#[test]
fn parse_startgroup_not_cowork() {
    assert_eq!(parse_startgroup_param("other_param"), None);
}

#[test]
fn parse_startgroup_empty() {
    assert_eq!(parse_startgroup_param(""), None);
}

#[test]
fn parse_startgroup_just_prefix() {
    assert_eq!(parse_startgroup_param("cowork_"), None);
}

#[test]
fn is_cowork_session_true() {
    assert!(is_cowork_session("cowork_xxx"));
}

#[test]
fn is_cowork_session_false() {
    assert!(!is_cowork_session("other"));
}

#[test]
fn build_deep_link_format() {
    // #709: the link requests admin rights inline so the bot joins promoted.
    let link = build_cowork_deep_link("mybot", "abc123");
    assert_eq!(
        link,
        "https://t.me/mybot?startgroup=cowork_abc123&admin=invite_users+delete_messages+pin_messages+manage_chat"
    );
}

#[test]
fn build_deep_link_requests_invite_users() {
    // create_chat_invite_link needs can_invite_users; it must be in the request.
    let link = build_cowork_deep_link("mybot", "abc123");
    let (_, admin) = link.split_once("&admin=").expect("admin param present");
    assert!(admin.split('+').any(|r| r == "invite_users"));
}

#[test]
fn build_deep_link_with_bot_suffix() {
    let link = build_cowork_deep_link("team_crab_bot", "xyz");
    assert!(link.starts_with("https://t.me/team_crab_bot?startgroup=cowork_xyz&admin="));
}

#[test]
fn cowork_state_lifecycle() {
    let state = CoworkState::new(123, 456, "abc".to_string());
    assert_eq!(state.user_id, 123);
    assert_eq!(state.chat_id, 456);
    assert_eq!(state.session_id, "abc");
    assert!(!state.is_expired());
}

#[test]
#[cfg(feature = "whatsapp")]
fn invite_qr_generation() {
    let result = build_invite_qr("https://t.me/+AbCdEfGh");
    assert!(result.is_some());
    let (bytes, path) = result.unwrap();
    // PNG magic number
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert!(path.exists());
    // Cleanup
    let _ = std::fs::remove_file(path);
}

#[test]
fn cowork_keyboard_forms_for_a_valid_deep_link() {
    let link = build_cowork_deep_link("team_crab_bot", "xyz");
    let kb = cowork_keyboard(&link).expect("a valid deep link must still produce the button");

    // Serialise instead of reaching into teloxide's internals: this is the
    // payload the API receives, so it proves both the label and the link.
    let json = serde_json::to_string(&kb).expect("keyboard serialises");
    assert!(json.contains("Add to Group"), "button label missing: {json}");
    assert!(
        json.contains("t.me/team_crab_bot?startgroup=cowork_xyz"),
        "deep link missing from the button: {json}"
    );
}

#[test]
fn cowork_keyboard_is_none_when_the_link_is_not_a_url() {
    // The username comes from the API, so a link that does not parse is
    // possible in principle. The handler must degrade to a text-only prompt
    // instead of panicking on a cosmetic button.
    assert!(cowork_keyboard("").is_none(), "empty link must not panic");
    assert!(
        cowork_keyboard("t.me/no-scheme?startgroup=cowork_x").is_none(),
        "a link without a scheme must not panic"
    );
}

#[test]
fn cowork_keyboard_is_not_a_telegram_link_validator() {
    // Near miss: the helper only checks that the link parses, it does not
    // check that it points at Telegram. Tightening it into a validator would
    // silently drop the button for links this test still expects to work.
    assert!(cowork_keyboard("https://example.com/not-telegram").is_some());
}
