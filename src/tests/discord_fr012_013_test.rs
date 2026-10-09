//! FR-012 / AC-015 and FR-013 / AC-016: announcements and AutoMod.
//!
//! Two surfaces join `discord_send`. `announce` posts through a channel webhook
//! carrying the bot's own name and avatar, then crossposts the message so Discord
//! marks it published (`Http::crosspost_message`, serenity 0.12.5
//! `src/http/client.rs:1992`). `automod_list` / `automod_create` / `automod_edit`
//! / `automod_delete` drive guild AutoMod rules through `EditAutoModRule`
//! (`src/builder/edit_automod_rule.rs:35`), and `audit_log` reads the guild log
//! back through `Http::get_audit_logs` (`src/http/client.rs:2833`) so a blocked
//! message can be traced to the rule that blocked it.
//!
//! What these tests pin is the part that decides whether a request is accepted or
//! whether a report is true, because that part is pure: which webhook may be
//! reused, which channel kinds can be crossposted, the keyword grammar and its
//! ceilings, and how a rule or an audit entry is rendered. The rendering half
//! matters because serenity gives `audit_log::Action` no `Display`, so the label
//! is ours and a silent change to it would be a silent change to every report.
//!
//! Nothing here touches the network: a live guild is needed for the calls
//! themselves, so those are covered by the live capture for AC-015 and AC-016.

use crate::brain::tools::discord_send::{
    ANNOUNCE_MAX_CHARS, AUTOMOD_AUDIT_REASON, AUTOMOD_MAX_KEYWORD_CHARS, AUTOMOD_MAX_KEYWORDS,
    AuditEntryView, RuleView, WebhookView, audit_action_label, automod_action_label,
    check_announce_length, crosspostable, parse_audit_action, parse_keywords,
    pick_announce_webhook, render_audit_entry, render_rule, trigger_label, truncate_for_display,
};
use serenity::builder::EditAutoModRule;
use serenity::model::channel::ChannelType;
use serenity::model::guild::audit_log::{Action as AuditAction, AutoModAction};
use serenity::model::guild::automod::{Action as AutomodAction, Trigger};
use std::path::Path;
use std::time::Duration;

// ── FR-012: which webhook, and which channel may be crossposted ──────────────

fn view(id: u64, name: &str, channel: u64, incoming: bool, has_token: bool) -> WebhookView {
    WebhookView {
        id,
        name: Some(name.to_string()),
        channel_id: Some(channel),
        incoming,
        has_token,
    }
}

#[test]
fn a_repeated_announcement_reuses_its_own_webhook() {
    let listed = vec![
        view(1, "OpenCrabs Announcements", 900, true, true),
        view(2, "Some Other Bot", 900, true, true),
    ];
    assert_eq!(
        pick_announce_webhook(&listed, 900, "OpenCrabs Announcements"),
        Some(1)
    );
}

#[test]
fn a_webhook_without_a_token_is_not_reusable() {
    // Discord only hands back a token for a webhook the bot may execute, so a
    // tokenless match cannot carry an announcement however well the name lines
    // up. Picking it would turn a repeat into a failure.
    let listed = vec![
        view(1, "OpenCrabs Announcements", 900, true, false),
        view(2, "OpenCrabs Announcements", 900, true, true),
    ];
    assert_eq!(
        pick_announce_webhook(&listed, 900, "OpenCrabs Announcements"),
        Some(2)
    );
}

#[test]
fn a_follower_webhook_or_another_channel_is_not_reused() {
    let other_channel = vec![view(1, "OpenCrabs Announcements", 901, true, true)];
    assert_eq!(
        pick_announce_webhook(&other_channel, 900, "OpenCrabs Announcements"),
        None
    );
    let follower = vec![view(1, "OpenCrabs Announcements", 900, false, true)];
    assert_eq!(
        pick_announce_webhook(&follower, 900, "OpenCrabs Announcements"),
        None
    );
    // A webhook another integration owns must not be borrowed.
    let foreign = vec![view(1, "Someone Else's Feed", 900, true, true)];
    assert_eq!(
        pick_announce_webhook(&foreign, 900, "OpenCrabs Announcements"),
        None
    );
}

#[test]
fn only_an_announcement_channel_can_be_crossposted() {
    assert!(crosspostable(ChannelType::News));
    // Everything else answers 400 at Discord, so the gate has to say no first.
    for kind in [
        ChannelType::Text,
        ChannelType::Voice,
        ChannelType::Category,
        ChannelType::Forum,
        ChannelType::Private,
    ] {
        assert!(!crosspostable(kind), "{kind:?} must not be crossposted");
    }
}

#[test]
fn an_announcement_is_refused_past_the_ceiling_rather_than_split() {
    let ok = "a".repeat(ANNOUNCE_MAX_CHARS);
    assert!(check_announce_length(&ok).is_ok());
    let over = "a".repeat(ANNOUNCE_MAX_CHARS + 1);
    let why = check_announce_length(&over).unwrap_err();
    assert!(
        why.contains("2001"),
        "the measured length must be in it: {why}"
    );
    // Counting is by character, not byte: a multi-byte glyph is one character.
    let accented = "é".repeat(ANNOUNCE_MAX_CHARS);
    assert!(check_announce_length(&accented).is_ok());
}

// ── FR-013: the keyword grammar ──────────────────────────────────────────────

#[test]
fn keywords_split_on_commas_and_newlines_and_drop_duplicates() {
    assert_eq!(
        parse_keywords("bad, worse\nworst").unwrap(),
        vec!["bad", "worse", "worst"]
    );
    // Blank entries are separators, not keywords.
    assert_eq!(parse_keywords(" bad ,, \n ").unwrap(), vec!["bad"]);
    // A duplicate spends the budget without widening the rule, so it collapses.
    assert_eq!(parse_keywords("bad,bad").unwrap(), vec!["bad"]);
    // A phrase containing spaces is one keyword.
    assert_eq!(parse_keywords("two words").unwrap(), vec!["two words"]);
}

#[test]
fn an_empty_keyword_list_is_refused() {
    for spec in ["", "  ", ",", "\n,\n"] {
        assert!(parse_keywords(spec).is_err(), "{spec:?} must be refused");
    }
}

#[test]
fn a_keyword_past_the_character_ceiling_is_refused_with_its_length() {
    let long = "x".repeat(AUTOMOD_MAX_KEYWORD_CHARS + 1);
    let why = parse_keywords(&long).unwrap_err();
    assert!(
        why.contains("61"),
        "the measured length must be in it: {why}"
    );
    let edge = "x".repeat(AUTOMOD_MAX_KEYWORD_CHARS);
    assert!(parse_keywords(&edge).is_ok());
}

#[test]
fn a_keyword_list_past_the_count_ceiling_is_refused() {
    let many = (0..=AUTOMOD_MAX_KEYWORDS)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let why = parse_keywords(&many).unwrap_err();
    assert!(
        why.contains(&(AUTOMOD_MAX_KEYWORDS + 1).to_string()),
        "{why}"
    );
}

#[test]
fn a_long_phrase_is_shortened_for_display_without_splitting_a_character() {
    assert_eq!(truncate_for_display("short"), "short");
    let long = "é".repeat(60);
    let shown = truncate_for_display(&long);
    assert!(shown.ends_with("..."), "{shown}");
    assert_eq!(shown.chars().count(), 43, "40 characters plus the ellipsis");
}

// ── FR-013: rendering a rule ─────────────────────────────────────────────────

#[test]
fn a_keyword_trigger_reports_its_counts() {
    let trigger = Trigger::Keyword {
        strings: vec!["a".into(), "b".into()],
        regex_patterns: vec!["x".into()],
        allow_list: vec!["ok".into()],
    };
    assert_eq!(trigger_label(&trigger), "2 keyword(s), 1 regex, 1 allowed");
    let plain = Trigger::Keyword {
        strings: vec!["a".into()],
        regex_patterns: Vec::new(),
        allow_list: Vec::new(),
    };
    assert_eq!(trigger_label(&plain), "1 keyword(s)");
    assert_eq!(trigger_label(&Trigger::Spam), "spam");
}

#[test]
fn an_action_is_rendered_with_its_detail() {
    assert_eq!(
        automod_action_label(&AutomodAction::BlockMessage {
            custom_message: None
        }),
        "block"
    );
    let with_text = automod_action_label(&AutomodAction::BlockMessage {
        custom_message: Some("no thanks".into()),
    });
    assert_eq!(with_text, "block (says: no thanks)");
    let timeout = automod_action_label(&AutomodAction::Timeout(Duration::from_secs(600)));
    assert_eq!(timeout, "timeout 600s");
}

#[test]
fn a_rule_renders_as_one_line_carrying_its_state_and_id() {
    let view = RuleView {
        id: 77,
        name: "blocked phrase".into(),
        enabled: true,
        event: "MessageSend".into(),
        trigger: "1 keyword(s)".into(),
        actions: "block".into(),
    };
    let line = render_rule(&view);
    assert!(line.starts_with("- blocked phrase [enabled]"), "{line}");
    assert!(line.contains("id=77"), "{line}");
    assert!(line.contains("trigger=1 keyword(s)"), "{line}");
    assert!(line.contains("actions=block"), "{line}");
    let off = RuleView {
        enabled: false,
        ..view
    };
    assert!(render_rule(&off).contains("[disabled]"));
}

// ── FR-013: the audit log ────────────────────────────────────────────────────

#[test]
fn an_automod_entry_is_labelled_and_named_after_its_rule() {
    // The rule name is what turns "something was blocked" into "this rule blocked
    // something", which is the whole point of surfacing the log.
    let entry = AuditEntryView {
        id: 5,
        action: audit_action_label(AuditAction::AutoMod(AutoModAction::BlockMessage)),
        user_id: 11,
        target_id: Some(22),
        reason: None,
        rule_name: Some("blocked phrase".into()),
    };
    let line = render_audit_entry(&entry);
    assert!(line.contains("automod BlockMessage"), "{line}");
    assert!(line.contains("by user 11"), "{line}");
    assert!(line.contains("on 22"), "{line}");
    assert!(line.contains("(rule: blocked phrase)"), "{line}");
    assert!(line.contains("[entry 5]"), "{line}");
}

#[test]
fn an_entry_without_a_target_or_reason_still_renders() {
    let entry = AuditEntryView {
        id: 6,
        action: audit_action_label(AuditAction::GuildUpdate),
        user_id: 11,
        target_id: None,
        reason: None,
        rule_name: None,
    };
    assert_eq!(
        render_audit_entry(&entry),
        "- guild update by user 11 [entry 6]"
    );
}

#[test]
fn a_filter_name_maps_to_its_discord_number_and_a_number_passes_through() {
    let block = parse_audit_action("automod_block_message").unwrap();
    assert_eq!(block.num(), 143);
    // `audit_log::Action` carries no `PartialEq`, so the number is the pin. It is
    // the same 143 Discord filters on, which is what makes the filter exact.
    assert_eq!(
        block.num(),
        AuditAction::AutoMod(AutoModAction::BlockMessage).num()
    );
    // A name is case insensitive, and a raw number is accepted for the rest.
    assert_eq!(parse_audit_action("MEMBER_KICK").unwrap().num(), 20);
    assert_eq!(parse_audit_action("72").unwrap().num(), 72);
    // An unknown name is refused rather than guessed at: Discord's filter is
    // exact, so a wrong one silently returns the wrong slice of the log.
    assert!(parse_audit_action("not_a_real_action").is_none());
    // Out of range for the wire format too, so it cannot be a raw number either.
    assert!(parse_audit_action("999").is_none());
    assert_eq!(parse_audit_action("200").unwrap().num(), 200);
}

// ── FR-013: the body Discord actually reads ──────────────────────────────────

#[test]
fn a_new_rule_puts_the_fields_discord_requires_on_the_wire() {
    // `EditAutoModRule::new()` is `Self::default()`, and serenity's default sets
    // `event_type` to `MessageSend`. That field is not an `Option` and carries no
    // `skip_serializing_if`, so it is always on the wire, and Discord answers 400
    // BASE_TYPE_REQUIRED when it is missing. Pinned on the serialised body rather
    // than on the builder because the body is what Discord reads, and because a
    // serenity upgrade that made the field optional would otherwise only show up
    // as a live 400 that no unit test would catch.
    let builder = EditAutoModRule::new()
        .name("probe")
        .trigger(Trigger::Keyword {
            strings: vec!["x".to_string()],
            regex_patterns: Vec::new(),
            allow_list: Vec::new(),
        })
        .actions(vec![AutomodAction::BlockMessage {
            custom_message: None,
        }])
        .enabled(true);
    let encoded = serde_json::to_value(builder);
    let body = encoded.expect("EditAutoModRule serialises");
    let event_type = body.get("event_type").and_then(|v| v.as_u64());
    assert_eq!(
        event_type,
        Some(1),
        "MessageSend must be on the wire: {body}"
    );
    // `trigger` is `#[serde(flatten)]`, so its keys have to land at the top level
    // for Discord to see a trigger at all.
    let trigger_type = body.get("trigger_type").and_then(|v| v.as_u64());
    assert_eq!(
        trigger_type,
        Some(1),
        "the trigger must be flattened: {body}"
    );
    assert!(
        body.get("actions").is_some(),
        "actions are required: {body}"
    );
}

// ── the wiring, pinned against the source ────────────────────────────────────

fn tool_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/brain/tools/discord_send.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Strip every whitespace character, so a call split across lines still reads as
/// one needle. Same trick as `discord_write_discipline_test`.
fn flattened() -> String {
    tool_source()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[test]
fn every_new_verb_is_advertised_and_dispatched() {
    let src = flattened();
    for verb in [
        "announce",
        "automod_list",
        "automod_create",
        "automod_edit",
        "automod_delete",
        "audit_log",
    ] {
        assert!(
            src.contains(&format!("\"{verb}\"=>")),
            "{verb} has no dispatch arm"
        );
        assert!(
            src.contains(&format!("\"{verb}\",")),
            "{verb} is missing from the schema enum"
        );
    }
}

#[test]
fn only_the_three_mutating_automod_arms_are_guarded() {
    let src = flattened();
    // The reads are deliberately unguarded, so an exact count is the pin: three
    // mutations, and a fourth guard would mean a read grew one.
    assert_eq!(
        src.matches("moderation_guard(None)").count(),
        3,
        "automod_create/edit/delete must each be guarded"
    );
    // Four, not three: the const's own declaration is the fourth hit, since the
    // scan reads the whole file rather than only the call sites.
    assert_eq!(
        src.matches("AUTOMOD_AUDIT_REASON").count(),
        4,
        "each mutation must carry the reason into the guild's own audit log"
    );
    // The identifier only proves the const is mentioned. The value is what
    // Discord actually records, so pin that too: a silent reword would make the
    // guild's own log stop matching what this tool reports.
    assert!(
        AUTOMOD_AUDIT_REASON.contains("OpenCrabs agent"),
        "the audit reason must name its origin: {AUTOMOD_AUDIT_REASON}"
    );
    // The member actions keep their target, so the shared guard did not lose it.
    assert_eq!(src.matches("moderation_guard(Some(user_id))").count(), 6);
}

#[test]
fn the_scan_reads_the_file_it_governs() {
    // A pin over an empty or misresolved read would pass silently.
    let src = flattened();
    assert!(src.len() > 40_000, "read only {} bytes", src.len());
    assert!(src.contains("structDiscordSendTool"));
}

#[test]
fn the_tool_source_is_where_the_test_expects() {
    let root = env!("CARGO_MANIFEST_DIR");
    let path = Path::new(root).join("src/brain/tools/discord_send.rs");
    assert!(path.is_file(), "missing {}", path.display());
}
