//! Newsletter (channel) actions on the WhatsApp send tool: reading a channel's
//! metadata and history, and joining, leaving, creating, editing, and revoking.
//!
//! The arms need a live socket, so the wire calls cannot run here. What is
//! checkable without a connection is the part that decides what the model can
//! reach and which call each action makes: the dispatch table, the argument
//! rules the arms refuse on, and which JIDs the resolver will accept.

const SOURCE: &str = include_str!("../brain/tools/whatsapp_send.rs");
const POLLER: &str = include_str!("../channels/whatsapp/newsletter.rs");

/// Every action this task added, paired with the call it must make.
///
/// The receiver is part of the pin: a channel description read through the
/// group surface, or a history page fetched with the wrong pagination call,
/// would still compile.
const ACTIONS: &[(&str, &str)] = &[
    ("newsletter_get_metadata", "newsletter().get_metadata("),
    (
        "newsletter_get_metadata_by_invite",
        "newsletter().get_metadata_by_invite(",
    ),
    ("newsletter_get_messages", "newsletter().get_messages("),
    ("newsletter_join", "newsletter().join("),
    ("newsletter_leave", "newsletter().leave("),
    ("newsletter_create", "newsletter().create("),
    ("newsletter_update", "newsletter().update("),
    ("newsletter_revoke_message", "newsletter().revoke_message("),
];

/// The text with all whitespace removed, so an assertion does not depend on
/// where rustfmt chose to wrap a chain across lines.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The body of the match arm that handles `action`: from its pattern to the
/// start of the next arm.
///
/// Pins are scoped to the arm on purpose. A file-wide `contains` would be
/// satisfied by a call living anywhere in a three-thousand-line file, so an
/// action whose arm was deleted while its helper stayed in use elsewhere would
/// still pass.
fn arm_body(action: &str) -> &'static str {
    let needle = format!("\"{action}\"");
    let mut from = 0;
    loop {
        let at = SOURCE[from..]
            .find(&needle)
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("'{action}' does not appear in the source"));
        let after = &SOURCE[at + needle.len()..];
        // An arm pattern reaches `=> {` on its own line, right after the name:
        // a grouped arm such as `"a" | "b" => {` included. A hit further away,
        // or one that crosses a newline, is the schema enum or an error string.
        if let Some(rel) = after.find("=> {")
            && rel < 80
            && !after[..rel].contains('\n')
        {
            let rest = &after[rel..];
            let ends = [
                rest.find("\n            \""),
                rest.find("\n            unknown"),
            ];
            let end = ends.into_iter().flatten().min().unwrap_or(rest.len());
            return &rest[..end];
        }
        from = at + needle.len();
    }
}

/// The body of the newsletter JID resolver, from its signature to its closing
/// brace, so its rules can be pinned without matching the sibling resolvers
/// that share the same scope call.
fn resolver_body() -> &'static str {
    let at = SOURCE
        .find("fn resolve_newsletter_jid")
        .expect("the newsletter resolver exists");
    let rest = &SOURCE[at..];
    let end = rest.find("\n}\n").expect("the resolver closes");
    &rest[..end]
}

#[test]
fn every_newsletter_action_has_a_match_arm_and_a_schema_entry() {
    let compact = squash(SOURCE);
    for (action, _) in ACTIONS {
        let occurrences = compact.matches(&format!("\"{action}\"")).count();
        assert!(
            occurrences >= 2,
            "'{action}' appears {occurrences} time(s): it needs both a schema enum entry \
             (so the model can discover it) and a match arm"
        );
    }
}

#[test]
fn the_newsletter_arms_call_the_feature_that_answers_them() {
    for (action, call) in ACTIONS {
        assert!(
            squash(arm_body(action)).contains(call),
            "'{action}' does not call `{call}`"
        );
    }
}

/// A channel JID is `<id>@newsletter`, and a group id is `<id>@g.us`. Accepting
/// the second spelling would point a public post at a conversation, so the
/// resolver checks the server rather than trusting the shape of the string.
#[test]
fn a_non_newsletter_jid_is_refused() {
    let body = squash(resolver_body());
    assert!(
        body.contains("jid.server!=Server::Newsletter"),
        "the resolver must reject a JID whose server is not Newsletter"
    );
    assert!(
        body.contains("isnotanewsletterJID"),
        "the refusal must say what was wrong with the JID"
    );
}

/// Channel ids are handed out bare (an invite, a config entry), so the resolver
/// wraps one the way the group resolver does. Without this, every caller would
/// have to know the `@newsletter` suffix.
#[test]
fn a_bare_channel_id_is_wrapped() {
    assert!(
        squash(resolver_body()).contains("@newsletter"),
        "a bare channel id must be wrapped as <id>@newsletter"
    );
}

/// Creating, joining, leaving, and revoking all change a public surface, so the
/// resolver runs them past the same send scope as any outbound act. The check
/// lives in the resolver rather than in each arm so a new channel action cannot
/// be added without it.
#[test]
fn the_resolver_runs_the_send_scope() {
    let body = squash(resolver_body());
    assert!(
        body.contains("send_scope::may_send(\"whatsapp\",&jid_str)"),
        "the resolver must consult the send scope"
    );
    assert!(
        body.contains("refusal_for(\"whatsapp\",&jid_str)"),
        "a refusal must carry the reason, not just a boolean"
    );
}

/// The two readers only report what the server already knows about a channel,
/// so charging them to the send budget would spend it on an answer that posts
/// nothing.
#[test]
fn reading_a_channel_is_not_charged_to_the_send_budget() {
    for action in [
        "newsletter_get_metadata",
        "newsletter_get_metadata_by_invite",
        "newsletter_get_messages",
    ] {
        assert!(
            !squash(arm_body(action)).contains("gate_send!"),
            "'{action}' posts nothing and must not be charged to the send budget"
        );
    }
}

/// Five arms report the same object back. Five copies of the formatter would
/// drift into five shapes, and a caller cannot tell which fields an action
/// chose to omit, so they share one.
#[test]
fn one_formatter_reports_every_channel_description() {
    for action in [
        "newsletter_get_metadata",
        "newsletter_get_metadata_by_invite",
        "newsletter_join",
        "newsletter_create",
        "newsletter_update",
    ] {
        assert!(
            squash(arm_body(action)).contains("describe_newsletter(&m)"),
            "'{action}' must render its metadata through the shared formatter"
        );
    }
}

/// A history page is a network round trip and a slice of the model's context,
/// so a caller asking for ten thousand posts must not get a ten-thousand-post
/// request. The cap is clamped rather than refused: the newest posts are the
/// ones worth reading anyway.
#[test]
fn a_history_page_is_clamped() {
    assert!(
        squash(arm_body("newsletter_get_messages")).contains(".clamp(1,100)asu32"),
        "newsletter_get_messages must clamp 'count' into 1..=100"
    );
}

/// An update with neither a new name nor a new description is a wire call that
/// can only report success while changing nothing, which reads to the caller as
/// "the edit worked". The arm refuses instead.
#[test]
fn an_update_with_nothing_to_change_is_refused() {
    let body = squash(arm_body("newsletter_update"));
    assert!(
        body.contains("ifname.is_none()&&description.is_none()"),
        "newsletter_update must refuse when neither field was given"
    );
    assert!(
        body.contains("newsletter_updateneeds'message'"),
        "the refusal must say which parameters it wants"
    );
}

/// A channel post is a caption, a plain body, or an extended text, and the
/// digest poller already knows how to read all three. A second extractor here
/// would make the same post read differently in a digest and in a history page.
#[test]
fn channel_history_reuses_the_digest_text_extractor() {
    assert!(
        squash(SOURCE).contains("usecrate::channels::whatsapp::newsletter::post_body;"),
        "the arm must import the poller's text extractor rather than re-implement it"
    );
    assert!(
        squash(arm_body("newsletter_get_messages")).contains("describe_newsletter_messages("),
        "newsletter_get_messages must render the page through its formatter"
    );
    assert!(
        squash(SOURCE).contains(".map(post_body)"),
        "the page formatter must call the shared extractor"
    );
    assert!(
        squash(POLLER).contains("pub(crate)fnpost_body(m:&waproto::whatsapp::Message)"),
        "the extractor must be reachable from the tool module"
    );
}
