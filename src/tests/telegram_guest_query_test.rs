//! Guest mode (#2087): answering an `@botname` tag in a chat the bot is not a
//! member of.
//!
//! Bot API 10.0 added `Update.guest_message`, `Message.guest_query_id`, the
//! caller fields and the method `answerGuestQuery`; the pinned
//! teloxide-core 0.13.0 knows none of them, so a guest update reaches the
//! typed dispatcher as `UpdateKind::Error` and the raw update path
//! (`channels/telegram/raw_updates.rs` -> `channels/telegram/guest.rs`) is the
//! only way in. Covered here are the four required cases: the parse, the
//! not-a-guest case, the `answerGuestQuery` body (including the 4096 clamp
//! Telegram imposes on the message text), and the caller gate.

use serde_json::json;

use crate::channels::telegram::guest::{answer_guest_query_body, parse_guest_query};
use crate::config::types::{TelegramConfig, TelegramGroupConfig};

/// A guest update as Telegram delivers it: someone tagged the bot in a chat
/// the bot is not a member of.
fn guest_update() -> serde_json::Value {
    json!({
        "update_id": 77,
        "guest_message": {
            "message_id": 5,
            "date": 1_750_000_000,
            "guest_query_id": "gq-abc123",
            "guest_bot_caller_user": {
                "id": 4242,
                "is_bot": false,
                "first_name": "Ana",
                "username": "ana"
            },
            "guest_bot_caller_chat": {
                "id": -100999,
                "type": "supergroup",
                "title": "Ana's crew"
            },
            "text": "@opencrabs what is 2+2?"
        }
    })
}

fn telegram_cfg(
    guest_queries: bool,
    allowed: &[&str],
    owner: &[&str],
    groups: &[(&str, &[&str], bool)],
) -> TelegramConfig {
    let mut g = std::collections::HashMap::new();
    for (id, users, open) in groups {
        g.insert(
            (*id).to_string(),
            TelegramGroupConfig {
                allowed_users: users.iter().map(|s| s.to_string()).collect(),
                open: *open,
                ..Default::default()
            },
        );
    }
    TelegramConfig {
        guest_queries,
        allowed_users: allowed.iter().map(|s| s.to_string()).collect(),
        bot_owner: owner.iter().map(|s| s.to_string()).collect(),
        groups: g,
        ..Default::default()
    }
}

#[test]
fn guest_message_with_query_id_parses_and_the_typed_parse_cannot_see_it() {
    let q = parse_guest_query(&guest_update()).expect("a guest update parses");
    assert_eq!(q.query_id, "gq-abc123");
    assert_eq!(q.user_id, 4242);
    assert_eq!(q.username.as_deref(), Some("ana"));
    assert_eq!(q.chat_id, Some(-100999));
    assert_eq!(q.chat_title.as_deref(), Some("Ana's crew"));
    assert_eq!(q.text, "@opencrabs what is 2+2?");

    // The typed deserializer has no `guest_message` variant, so the same
    // payload arrives as UpdateKind::Error and no dispatcher branch matches
    // it. This is why the raw branch exists; if teloxide grows the variant,
    // this assertion fails and the raw branch can be simplified.
    let typed: teloxide::types::Update =
        serde_json::from_str(&guest_update().to_string()).expect("lenient parse");
    assert!(
        matches!(typed.kind, teloxide::types::UpdateKind::Error(_)),
        "teloxide parsed a guest update into a typed kind"
    );
}

#[test]
fn a_message_without_a_query_id_does_not_take_the_guest_path() {
    // Guest message, but nothing to answer into.
    let mut no_id = guest_update();
    no_id["guest_message"]
        .as_object_mut()
        .expect("guest_message is an object")
        .remove("guest_query_id");
    assert!(
        parse_guest_query(&no_id).is_none(),
        "an empty guest_query_id is not answerable"
    );

    // A guest message with no identifiable caller: the gate would have
    // nothing to check, so it is dropped rather than answered on faith.
    let mut no_caller = guest_update();
    no_caller["guest_message"]
        .as_object_mut()
        .expect("guest_message is an object")
        .remove("guest_bot_caller_user");
    assert!(
        parse_guest_query(&no_caller).is_none(),
        "no caller means no gate to apply"
    );

    // An ordinary DM is not a guest query.
    let dm = json!({
        "update_id": 1,
        "message": {
            "message_id": 10,
            "date": 1_750_000_000,
            "chat": {"id": 123, "type": "private", "first_name": "A"},
            "from": {"id": 123, "is_bot": false, "first_name": "A"},
            "text": "hello"
        }
    });
    assert!(
        parse_guest_query(&dm).is_none(),
        "a DM is not a guest query"
    );
}

#[test]
fn answer_body_is_a_wellformed_guest_result_and_honours_the_text_ceiling() {
    let body = answer_guest_query_body("gq-1", "Hello there\nsecond line", false);
    assert_eq!(body["guest_query_id"], json!("gq-1"));
    assert_eq!(body["result"]["type"], json!("article"));
    assert_eq!(body["result"]["id"], json!("opencrabs-guest"));
    assert_eq!(body["result"]["title"], json!("Hello there"));
    assert_eq!(
        body["result"]["input_message_content"]["message_text"],
        json!("Hello there\nsecond line")
    );
    assert!(
        body["result"]["input_message_content"]
            .get("parse_mode")
            .is_none(),
        "guest replies go out as plain text"
    );
    let id = body["result"]["id"]
        .as_str()
        .expect("result id is a string");
    assert!(id.len() <= 64, "inline result id over Telegram's cap: {id}");

    // Telegram rejects message_text past 4096; long replies get cut, not lost.
    let long = "x".repeat(5000);
    let clamped = answer_guest_query_body("gq-2", &long, false);
    let text = clamped["result"]["input_message_content"]["message_text"]
        .as_str()
        .expect("message_text is a string");
    assert_eq!(text.chars().count(), 4096, "clamped to the ceiling");
    assert!(text.ends_with("[truncated]"), "cut is marked: {text:?}");
    assert!(text.starts_with(&"x".repeat(64)));

    // The cut counts the units Telegram counts, so astral text runs out of
    // room sooner than a character count suggests (a crab is 2 units) and a
    // glyph is never split in half.
    let emoji = "\u{1F980}".repeat(5000);
    let multibyte = answer_guest_query_body("gq-3", &emoji, false);
    let text = multibyte["result"]["input_message_content"]["message_text"]
        .as_str()
        .expect("message_text is a string");
    assert!(utf16_len(text) <= 4096, "{} units", utf16_len(text));
    let cut = text.strip_suffix("[truncated]").expect("cut is marked");
    let kept = cut.trim_end_matches('\n');
    assert!(
        kept.chars().all(|c| c == '\u{1F980}'),
        "whole glyphs only, never half a crab"
    );
    assert!(
        kept.chars().count() >= 2000,
        "the room is spent, not left empty: {} crabs",
        kept.chars().count()
    );
}

#[test]
fn the_caller_gate_refuses_unlisted_callers_and_grants_nothing_when_off() {
    let groups: &[(&str, &[&str], bool)] = &[("-100g", &["222"], false), ("-100open", &[], true)];

    // Default is off: not even the owner is answered.
    let off = telegram_cfg(false, &["111"], &["111"], groups);
    assert!(!off.guest_allowed("111", Some("-100g")));
    assert!(!off.guest_allowed("111", None));

    let on = telegram_cfg(true, &["111"], &["111"], groups);
    // Owner and admins are answered anywhere, caller chat or not.
    assert!(on.guest_allowed("111", None));
    assert!(on.guest_allowed("111", Some("-100nowhere")));
    // A listed user is answered in their own chat only.
    assert!(on.guest_allowed("222", Some("-100g")));
    assert!(
        !on.guest_allowed("222", Some("-100other")),
        "a listing is per chat"
    );
    assert!(
        !on.guest_allowed("222", None),
        "no caller chat means nothing to check"
    );
    // Unlisted caller: refused, even in a chat the bot knows.
    assert!(!on.guest_allowed("333", Some("-100g")));
    // A group's open = true opens THAT group to the bot; it does not mean the
    // bot answers guest tags from its members.
    assert!(!on.guest_allowed("444", Some("-100open")));
}

// ── The progress block (#2109) ─────────────────────────────────────────────
//
// A guest summon gets one reply and no typing indicator, so the reply itself
// is the progress surface: it is posted when the turn starts and edited in
// place on the regular cadence. These pin the two halves of that. The state it
// renders is the state a regular turn renders, so the tool lines, the counters
// and the footer are not a guest-shaped copy; and the composed message always
// fits inside what `editMessageText` accepts.

use std::sync::{Arc, Mutex};

use crate::brain::agent::ProgressEvent;
use crate::channels::background_work::FlowOutcome;
use crate::channels::telegram::flow::{
    FlowEntry, StreamingState, drain_display_queue, render_flow,
};
use crate::channels::telegram::guest::{
    compose_guest_message, edit_html_body, edit_response_ok, edit_rich_body, guest_sanitize,
    settle_guest_answer, settled_guest_message, utf16_len, visible_len,
};
use crate::channels::telegram::handler::thinking_status_excerpt;
use crate::channels::telegram::progress::apply_progress_event;

fn turn_state() -> Arc<Mutex<StreamingState>> {
    // `is_dm: false` is the scrubbing scope: a reply into a chat we do not
    // control must never carry a credential, so the guest state is built the
    // way a group chat builds it.
    Arc::new(Mutex::new(StreamingState::new_turn(false, false)))
}

/// Run one tool through the shared event mapping and settle the block, the way
/// the guest path does at the end of a turn.
fn settled_block() -> String {
    let state = turn_state();
    apply_progress_event(
        &state,
        ProgressEvent::ToolStarted {
            tool_name: "bash".to_string(),
            tool_input: json!({"command": "ls"}),
            tool_call_id: "call-1".to_string(),
        },
    );
    apply_progress_event(
        &state,
        ProgressEvent::ToolCompleted {
            tool_name: "bash".to_string(),
            tool_input: json!({"command": "ls"}),
            success: true,
            summary: "ok".to_string(),
            tool_call_id: "call-1".to_string(),
        },
    );
    let mut s = state.lock().unwrap();
    drain_display_queue(&mut s, |text, _| text.to_string());
    s.flow_outcome = Some(FlowOutcome::Finished);
    s.tools_started_at = None;
    s.sections.ctx = Some("ctx: 1.2K/128K 1%".to_string());
    render_flow(&s)
}

#[test]
fn a_settled_guest_reply_carries_the_block_the_footer_and_the_answer() {
    let block = settled_block();
    let message = compose_guest_message(&block, "Here is the answer.", false);

    assert!(
        message.contains("✅ Finished"),
        "the settled outcome leads the block: {message}"
    );
    assert!(
        message.contains("1 tool calls"),
        "the tool the turn ran is counted, so its line reached the log: {message}"
    );
    assert!(
        message.contains("ctx: 1.2K/128K 1%"),
        "the context budget rides the settled block: {message}"
    );
    assert!(
        message.contains("Here is the answer."),
        "the answer is in the same message: {message}"
    );
    assert!(
        message.ends_with("Here is the answer."),
        "the answer sits under the block, not inside it: {message}"
    );
}

#[test]
fn a_live_guest_reply_shows_activity_and_no_settled_chrome() {
    let state = turn_state();
    apply_progress_event(
        &state,
        ProgressEvent::ReasoningChunk {
            // The shared excerpt capitalises its first letter, so the line
            // reads exactly as it does in a normal chat.
            text: "Checking the repository layout".to_string(),
        },
    );
    let live = {
        let mut s = state.lock().unwrap();
        drain_display_queue(&mut s, |text, _| text.to_string());
        s.header_preview = thinking_status_excerpt(&s.thinking);
        render_flow(&s)
    };

    assert!(
        live.contains("Checking the repository layout"),
        "the live status line rides the block while the turn runs: {live}"
    );
    assert!(
        !live.contains("Finished"),
        "nothing settled while the turn is live: {live}"
    );
    assert!(
        !live.contains("ctx:"),
        "the context budget is settle-time chrome only: {live}"
    );
}

#[test]
fn a_guest_reply_never_exceeds_the_edit_ceiling() {
    let block = settled_block();
    let answer = "x".repeat(9000);
    let message = compose_guest_message(&block, &answer, false);

    assert!(
        visible_len(&message) <= 4096,
        "over the ceiling at {} units",
        visible_len(&message)
    );
    assert!(message.contains("[truncated]"), "the cut is marked");
    assert!(
        message.contains("✅ Finished"),
        "the block survives the cut: the ceiling belongs to the answer"
    );
}

#[test]
fn a_block_that_leaves_no_room_drops_out_of_the_reply() {
    let block = format!("<blockquote>{}</blockquote>", "x".repeat(5000));
    let message = compose_guest_message(&block, "short answer", false);

    assert_eq!(
        message, "short answer",
        "the answer ships alone rather than squeezed under a footer"
    );
}

#[test]
fn the_ceiling_counts_utf16_units_not_chars() {
    // 4096 crab emoji are 8192 UTF-16 units, which is what Telegram counts.
    // A character count lets this through to a "message is too long" error.
    let body = answer_guest_query_body("gq-crabs", &"🦀".repeat(4096), false);
    let sent = body["result"]["input_message_content"]["message_text"]
        .as_str()
        .expect("message_text");

    assert!(utf16_len(sent) <= 4096, "{} units", utf16_len(sent));
    assert!(sent.ends_with("[truncated]"), "the cut is marked");
}

#[test]
fn a_no_op_edit_is_success_not_failure() {
    assert!(
        edit_response_ok(&json!({"ok": true, "result": true})),
        "a landed edit"
    );
    assert!(
        edit_response_ok(&json!({"ok": false, "error_code": 400,
                    "description": "Bad Request: message is not modified"})),
        "writing identical text is what the caller wanted"
    );
    assert!(
        !edit_response_ok(&json!({"ok": false, "error_code": 400,
                    "description": "Bad Request: message to edit not found"})),
        "a reply that is gone is a real failure"
    );
    assert!(
        !edit_response_ok(&json!({"ok": false, "error_code": 429,
                    "description": "Too Many Requests: retry after 30"})),
        "a rate limit is not a success"
    );
}

#[test]
fn guest_lines_never_carry_markers_or_artifacts() {
    let line = guest_sanitize(
        "Here you go <<IMG:/Users/adolfo/secret.png>> <<react:👍>> done",
        false,
    );

    assert!(
        !line.contains("<<IMG"),
        "no local path in a stranger's chat"
    );
    assert!(
        !line.contains("<<react"),
        "no channel directive in the block"
    );
}

#[test]
fn an_answer_that_also_streamed_into_the_block_is_not_repeated() {
    // 2026-10-18 06:20, guest session b809ac31: a no-tool turn streamed its
    // whole answer as intermediate text, so the block carried it and the reply
    // carried it again (#2113).
    let answer = "Loud and clear. The plumbing works.";
    let mut entries = vec![FlowEntry::Text(answer.to_string())];

    let reply = settle_guest_answer(&mut entries, answer, false);

    assert_eq!(reply, answer, "the reply is the answer, unchanged");
    assert!(
        entries.is_empty(),
        "the folded copy leaves the block, so it renders once: {entries:?}"
    );
}

#[test]
fn a_folded_answer_becomes_the_reply_when_the_turn_returned_nothing() {
    let mut entries = vec![
        FlowEntry::Tool(0),
        FlowEntry::Text("The answer.".to_string()),
    ];

    let reply = settle_guest_answer(&mut entries, "", false);

    assert_eq!(
        reply, "The answer.",
        "the block's trailing run is the answer"
    );
    assert_eq!(entries.len(), 1, "the tool history stays in the block");
    assert!(
        matches!(&entries[0], FlowEntry::Tool(0)),
        "the tool entry is untouched: {entries:?}"
    );
}

#[test]
fn narration_that_is_not_the_answer_stays_in_the_block() {
    let mut entries = vec![
        FlowEntry::Tool(0),
        FlowEntry::Text("Reading the config.".to_string()),
    ];

    let reply = settle_guest_answer(&mut entries, "Here is the real answer.", false);

    assert_eq!(reply, "Here is the real answer.");
    assert_eq!(
        entries.len(),
        2,
        "a distinct narration line is not the answer and must not be dropped"
    );
}

#[test]
fn a_post_halt_trailer_rides_after_a_reclaimed_answer() {
    // The turn halted on the options surface: the answer sits before the tool
    // entry, the sign-off after it. Both leave the block, the answer first.
    let mut entries = vec![
        FlowEntry::Text("The answer.".to_string()),
        FlowEntry::Tool(0),
        FlowEntry::Text("Anything else?".to_string()),
    ];

    let reply = settle_guest_answer(&mut entries, "", true);

    assert_eq!(reply, "The answer.\n\nAnything else?");
    assert_eq!(
        entries.len(),
        1,
        "both text runs left the block and the tool history stayed: {entries:?}"
    );
    assert!(
        matches!(&entries[0], FlowEntry::Tool(0)),
        "the trailing tool entry is restored in place: {entries:?}"
    );
}

#[test]
fn an_answer_before_a_trailing_tool_entry_leaves_the_block_once() {
    // The turn halted on the options surface: the answer streamed before the
    // suggest call, and the settled content carries the same text (#2113).
    let answer = "The answer.";
    let mut entries = vec![
        FlowEntry::Tool(0),
        FlowEntry::Text(answer.to_string()),
        FlowEntry::Tool(1),
    ];

    let reply = settle_guest_answer(&mut entries, answer, true);

    assert_eq!(reply, answer);
    assert_eq!(
        entries.len(),
        2,
        "both tool entries stay and only the answer leaves: {entries:?}"
    );
    assert!(matches!(&entries[0], FlowEntry::Tool(0)));
    assert!(matches!(&entries[1], FlowEntry::Tool(1)));
}

#[test]
fn narration_before_a_trailing_tool_entry_is_not_the_answer() {
    let mut entries = vec![
        FlowEntry::Tool(0),
        FlowEntry::Text("Reading the config.".to_string()),
        FlowEntry::Tool(1),
    ];

    let reply = settle_guest_answer(&mut entries, "Here is the real answer.", true);

    assert_eq!(reply, "Here is the real answer.");
    assert_eq!(
        entries.len(),
        3,
        "a distinct narration line stays in the block: {entries:?}"
    );
}

#[test]
fn a_no_tool_guest_turn_renders_the_footer_and_the_answer_and_nothing_else() {
    // 2026-10-18 06:20, guest session b809ac31 ("Guest Test"): the turn ran
    // zero tools (`Found 0 tool uses to execute`), yet the reply shipped a
    // `<blockquote expandable>` holding the answer, the footer under it, and
    // the answer again below (#2113). A regular chat shows no expandable at
    // all when no tool ran, because the log body is what the expandable
    // wraps. The guest reply has to reach the same shape inside one message.
    let answer = "Loud and clear, whoever you are. Your test message landed.";
    let state = turn_state();
    apply_progress_event(
        &state,
        ProgressEvent::IntermediateText {
            text: answer.to_string(),
            reasoning: None,
        },
    );
    // The whole settle step, exactly as the runner runs it.
    let (message, reply) =
        settled_guest_message(&state, answer, "ctx: 48K/200K 24%".to_string(), false);
    eprintln!("--- 0-tool guest reply ---\n{message}\n--- end ---");

    assert_eq!(reply, answer, "the reply is the answer, unchanged");
    assert!(
        !message.contains("<blockquote"),
        "no tool ran, so there is no log to collapse: {message}"
    );
    assert_eq!(
        message, answer,
        "a zero-log turn is the answer and nothing else, the shape a regular \
         turn lands on when it opens no block: {message}"
    );
}

#[test]
fn a_rich_guest_body_carries_the_rich_message_markdown() {
    let body = answer_guest_query_body("gq-rich", "**bold** and `code`", true);
    let content = &body["result"]["input_message_content"];

    assert_eq!(
        content["rich_message"]["markdown"],
        json!("**bold** and `code`"),
        "a rich reply hands Telegram the markdown to render: {content}"
    );
    assert!(
        content.get("message_text").is_none(),
        "one content shape per reply, never both: {content}"
    );
    assert!(
        content.get("parse_mode").is_none(),
        "rich content carries no parse mode: {content}"
    );
    assert_eq!(body["result"]["type"], json!("article"));
    assert_eq!(body["guest_query_id"], json!("gq-rich"));
    assert_eq!(body["result"]["title"], json!("**bold** and `code`"));
}

#[test]
fn a_classic_guest_body_still_carries_message_text() {
    let body = answer_guest_query_body("gq-plain", "**not** rendered", false);
    let content = &body["result"]["input_message_content"];

    assert_eq!(
        content["message_text"],
        json!("**not** rendered"),
        "rich off is the classic payload, unchanged: {content}"
    );
    assert!(
        content.get("rich_message").is_none(),
        "no rich content when the setting is off: {content}"
    );
}

#[test]
fn a_rich_guest_edit_carries_the_rich_message_and_the_inline_id() {
    let body = edit_rich_body("inline-42", "**answer**");

    assert_eq!(body["inline_message_id"], json!("inline-42"));
    assert_eq!(
        body["rich_message"]["markdown"],
        json!("**answer**"),
        "the composed reply is markdown, so the rich edit hands it over as markdown"
    );
    assert!(
        body["rich_message"].get("html").is_none(),
        "one input mode per rich message: {body}"
    );
    assert!(
        body.get("text").is_none(),
        "one content shape per edit, never both: {body}"
    );
}

#[test]
fn the_html_fallback_edit_body_is_unchanged() {
    let body = edit_html_body("inline-42", "<b>answer</b>");

    assert_eq!(body["inline_message_id"], json!("inline-42"));
    assert_eq!(body["text"], json!("<b>answer</b>"));
    assert_eq!(body["parse_mode"], json!("HTML"));
    assert!(
        body.get("rich_message").is_none(),
        "the fallback is the plain text edit: {body}"
    );
}

#[test]
fn a_rich_reply_keeps_the_markdown_its_html_plane_would_have_collapsed() {
    let answer = "## Findings\n\n| tool | setup |\n|---|---|\n| ob1 | brew |\n\n- one\n- two";
    let message = compose_guest_message("", answer, true);

    assert_eq!(
        message, answer,
        "the rich path ships the answer's own markdown untouched"
    );
    assert!(
        message.contains("\n\n"),
        "blank lines survive, so paragraphs do not collapse into a wall: {message:?}"
    );
    assert!(
        !message.contains("<b>") && !message.contains("<code>"),
        "no Telegram HTML on the markdown plane: {message}"
    );
}

#[test]
fn a_rich_reply_joins_its_block_as_markdown() {
    let block = "**✅ Finished (1 tool calls, 3s)**\n\n**✅ bash** `git status`";
    let message = compose_guest_message(block, "**answer**", true);

    assert_eq!(
        message,
        format!("{block}\n\n**answer**"),
        "block and answer share the markdown plane, separated by one blank line"
    );
}

#[test]
fn a_long_answer_ships_whole_when_rich_is_enabled() {
    let answer = "x".repeat(20_000);

    // The body: 20000 characters is far past the 4096 text ceiling and well
    // inside the 32768 rich one.
    let body = answer_guest_query_body("gq-long", &answer, true);
    let sent = body["result"]["input_message_content"]["rich_message"]["markdown"]
        .as_str()
        .expect("markdown");
    assert_eq!(sent.chars().count(), 20_000, "the whole answer, no cut");
    assert!(
        !sent.contains("[truncated]"),
        "the rich path never stamps a truncation marker"
    );

    // And the composed reply, block included.
    let block = settled_block();
    let message = compose_guest_message(&block, &answer, true);
    assert!(
        !message.contains("[truncated]"),
        "a 20000-char answer is not truncated on the rich path: {} chars",
        message.chars().count()
    );
    assert!(
        message.contains(&"x".repeat(1000)),
        "the answer body survived the compose"
    );
}

#[test]
fn the_classic_path_still_marks_its_cut() {
    // The plain field really is capped at 4096 by Telegram, so the marker is
    // honest there and must stay.
    let body = answer_guest_query_body("gq-classic-long", &"y".repeat(20_000), false);
    let sent = body["result"]["input_message_content"]["message_text"]
        .as_str()
        .expect("message_text");

    assert!(utf16_len(sent) <= 4096, "{} units", utf16_len(sent));
    assert!(sent.ends_with("[truncated]"), "the plain cut is marked");
}
