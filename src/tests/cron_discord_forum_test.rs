//! Cron reports delivered as Discord forum posts (#1851).
//!
//! One report, one post: the parent forum channel stays clean and each job's
//! history becomes a searchable list of titled threads. The platform half that
//! bites is tags. A `GUILD_FORUM` channel sets `REQUIRE_TAG`
//! (`developers/resources/channel.mdx:98`) and then refuses a post that carries
//! no `applied_tags`, so the naive version of this feature works in a bare test
//! forum and dies on the tagged servers the feature is for. These tests pin the
//! grammar, the tag policy and the title shape without touching the network.

use crate::cron::scheduler::{
    DiscordDelivery, forum_applied_tags, forum_post_body, forum_post_title, forum_requires_tag,
    forum_tag_names, is_thread_only_channel, parse_discord_target, resolve_forum_tag,
};
use chrono::{TimeZone, Utc};
use serde_json::json;

const FORUM_ID: &str = "123456789012345678";

fn stamp_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 2, 1, 30, 0).unwrap()
}

#[test]
fn a_bare_channel_target_keeps_the_existing_behaviour() {
    // Every job already on the board stores `discord:<id>` and must go on
    // posting plain messages: the forum mode is opt-in, never inferred.
    assert_eq!(
        parse_discord_target(FORUM_ID),
        Some((FORUM_ID.to_string(), DiscordDelivery::Channel))
    );
}

#[test]
fn the_forum_suffix_selects_a_post_and_keeps_the_id() {
    assert_eq!(
        parse_discord_target(&format!("{FORUM_ID}:forum")),
        Some((FORUM_ID.to_string(), DiscordDelivery::Forum))
    );
}

#[test]
fn the_forum_mode_is_not_a_second_address() {
    // The send scope permits the bare channel id, because that is the form
    // `discord_send` checks. A `:forum` job that also calls the tool has to
    // match its own target, so the mode must not become part of the address.
    let (channel_id, _) = parse_discord_target(&format!("{FORUM_ID}:forum")).unwrap();
    assert_eq!(channel_id, FORUM_ID);
}

#[test]
fn the_forum_mode_is_case_insensitive() {
    assert_eq!(
        parse_discord_target(&format!("{FORUM_ID}:FORUM")),
        Some((FORUM_ID.to_string(), DiscordDelivery::Forum))
    );
}

#[test]
fn a_non_numeric_channel_id_is_refused() {
    // Interpolated into a REST path, so a target that is not a snowflake never
    // reaches the wire.
    assert_eq!(parse_discord_target("not-a-channel:forum"), None);
    assert_eq!(parse_discord_target(""), None);
    assert_eq!(parse_discord_target("12345678901234567x"), None);
}

#[test]
fn an_unknown_mode_is_refused_rather_than_ignored() {
    // `discord:<id>:thread` must not quietly deliver as a plain channel
    // message: the operator asked for something this build does not do.
    assert_eq!(parse_discord_target(&format!("{FORUM_ID}:thread")), None);
    assert_eq!(parse_discord_target(&format!("{FORUM_ID}:4")), None);
    assert_eq!(parse_discord_target(&format!("{FORUM_ID}:forum:4")), None);
}

#[test]
fn require_tag_is_the_documented_bit() {
    // channel.mdx:98: REQUIRE_TAG = 1 << 4, and PINNED (1 << 1) is a
    // neighbouring bit that must not be mistaken for it.
    assert!(forum_requires_tag(&json!({ "flags": 1 << 4 })));
    assert!(forum_requires_tag(&json!({ "flags": (1 << 4) | (1 << 1) })));
    assert!(!forum_requires_tag(&json!({ "flags": 1 << 1 })));
    assert!(!forum_requires_tag(&json!({ "flags": 0 })));
}

#[test]
fn an_absent_flags_field_means_no_tag_is_required() {
    // `flags?` is optional on the channel object, so a forum that never set
    // any flag must not be refused.
    assert!(!forum_requires_tag(&json!({ "id": FORUM_ID })));
}

#[test]
fn the_refusal_names_the_tags_on_offer() {
    // The loud failure has to be actionable: name what the operator could have
    // used, from available_tags[].name (channel.mdx:48, :386-388).
    let channel = json!({
        "type": 15,
        "flags": 1 << 4,
        "available_tags": [{ "id": "1", "name": "Bug" }, { "id": "2", "name": "Release" }],
    });
    assert_eq!(forum_tag_names(&channel), vec!["Bug", "Release"]);
    assert!(forum_tag_names(&json!({ "type": 15 })).is_empty());
}

#[test]
fn forum_and_media_are_the_thread_only_channel_types() {
    // channel.mdx:79-80: GUILD_FORUM = 15, GUILD_MEDIA = 16, both "can only
    // contain threads". GUILD_TEXT (0) and PUBLIC_THREAD (11) are not.
    assert!(is_thread_only_channel(15));
    assert!(is_thread_only_channel(16));
    assert!(!is_thread_only_channel(0));
    assert!(!is_thread_only_channel(11));
}

#[test]
fn the_title_leads_with_the_job_and_ends_with_the_timestamp() {
    let title = forum_post_title("nightly-standup", stamp_time());
    assert_eq!(title, "nightly-standup | 2026-10-02 01:30 UTC");
}

#[test]
fn the_title_fits_the_name_limit_and_keeps_the_stamp() {
    // `name` is a 1-100 character channel name (channel.mdx:681). The stamp is
    // what makes the archive sortable, so the job name gives up room, not it.
    let long = "x".repeat(300);
    let title = forum_post_title(&long, stamp_time());
    assert!(
        title.chars().count() <= 100,
        "got {} chars",
        title.chars().count()
    );
    assert!(title.ends_with("2026-10-02 01:30 UTC"), "got: {title}");
    assert!(title.starts_with('x'));
}

#[test]
fn a_stub_job_name_posts_under_the_stamp_alone() {
    // Serenity's builder documents a 2-character floor for the name
    // (builder/create_forum_post.rs:37), so a one-letter job name is dropped
    // rather than sent as a stub.
    let title = forum_post_title("a", stamp_time());
    assert_eq!(title, "2026-10-02 01:30 UTC");
    assert!(title.chars().count() >= 2);
}

#[test]
fn the_post_body_carries_the_name_and_the_first_chunk_only() {
    let title = "nightly-standup | 2026-10-02 01:30 UTC";
    let body = forum_post_body(title, "report text", &[]);
    assert_eq!(body["name"], title);
    assert_eq!(body["message"]["content"], "report text");
}

#[test]
fn the_post_body_omits_applied_tags_when_none_is_resolved() {
    // Unset is still the default and still legal, so the field is omitted
    // rather than sent empty: an empty array is a post carrying no tag, which
    // is exactly what a REQUIRE_TAG channel rejects.
    let body = forum_post_body("t", "c", &[]);
    assert!(body.get("applied_tags").is_none(), "got: {body}");
    assert!(body["message"].get("embeds").is_none());
}

#[test]
fn the_forum_report_tag_defaults_to_unset() {
    // The knob is opt-in: an install that never sets it keeps the pre-FR-008
    // behaviour, and a REQUIRE_TAG forum still refuses loudly instead of 400ing.
    let discord = crate::config::DiscordConfig::default();
    assert!(discord.forum_report_tag.is_none());
}

#[test]
fn the_configured_tag_name_resolves_to_the_id_the_request_needs() {
    // The config carries a name because an operator cannot read a snowflake;
    // the request needs the id. Matching is case-insensitive, because the name
    // is typed by hand and is not a code identifier.
    let channel = json!({
        "type": 15,
        "available_tags": [{ "id": "1035", "name": "cron-report" }],
    });
    assert_eq!(
        resolve_forum_tag(&channel, "cron-report"),
        Some("1035".to_string())
    );
    assert_eq!(
        resolve_forum_tag(&channel, "CRON-REPORT"),
        Some("1035".to_string())
    );
    assert_eq!(resolve_forum_tag(&channel, "not-on-offer"), None);
}

#[test]
fn a_name_that_is_not_on_offer_resolves_to_nothing() {
    // The delivery path refuses the post on this empty result rather than
    // sending an untagged one, so the empty case is load-bearing.
    let channel = json!({
        "type": 15,
        "available_tags": [{ "id": "1035", "name": "cron-report" }],
    });
    assert_eq!(
        forum_applied_tags(&channel, Some("cron-report")),
        vec!["1035".to_string()]
    );
    assert!(forum_applied_tags(&channel, Some("wrong-name")).is_empty());
    // A blank knob is unset, not a tag whose name is empty.
    assert!(forum_applied_tags(&channel, Some("   ")).is_empty());
    assert!(forum_applied_tags(&channel, None).is_empty());
    // A channel advertising no tags cannot satisfy any name.
    assert!(forum_applied_tags(&json!({ "type": 15 }), Some("cron-report")).is_empty());
}

#[test]
fn a_require_tag_channel_posts_with_the_configured_tag_applied() {
    // AC-011, the gap #1851 left open: a forum that requires a tag is the whole
    // reason the knob exists, so this is the case that has to carry a tag id
    // and not an empty array.
    let channel = json!({
        "type": 15,
        "flags": 1 << 4,
        "available_tags": [
            { "id": "1035", "name": "cron-report" },
            { "id": "1036", "name": "Release" },
        ],
    });
    assert!(forum_requires_tag(&channel));

    let applied = forum_applied_tags(&channel, Some("cron-report"));
    assert_eq!(applied, vec!["1035".to_string()]);

    let title = "nightly-standup | 2026-10-02 01:30 UTC";
    let body = forum_post_body(title, "report", &applied);
    assert_eq!(body["applied_tags"], json!(["1035"]));
    let tags = body["applied_tags"].as_array().expect("applied_tags array");
    assert!(!tags.is_empty(), "REQUIRE_TAG post carried no tag: {body}");
}

#[test]
fn a_tagless_forum_with_no_knob_still_posts_untagged() {
    // The pre-FR-008 path has to survive: the knob is opt-in, so a forum that
    // requires no tag must not start needing one.
    let channel = json!({
        "type": 15,
        "flags": 0,
        "available_tags": [{ "id": "1035", "name": "cron-report" }],
    });
    assert!(!forum_requires_tag(&channel));
    assert!(forum_applied_tags(&channel, None).is_empty());

    let body = forum_post_body("t", "c", &[]);
    assert!(body.get("applied_tags").is_none(), "got: {body}");
}

#[test]
fn a_require_tag_channel_with_no_knob_is_refused_rather_than_posted_untagged() {
    // The condition the delivery path refuses on, expressed over the pure
    // helpers: the channel demands a tag and nothing resolves to one. Sending
    // the post anyway is a 400 nobody reads, which is why the refusal exists.
    let channel = json!({
        "type": 15,
        "flags": 1 << 4,
        "available_tags": [{ "id": "1035", "name": "cron-report" }],
    });
    assert!(forum_requires_tag(&channel));
    assert!(forum_applied_tags(&channel, None).is_empty());

    // A knob naming a tag the channel does not offer lands in the same place:
    // the post would carry nothing, so it is refused, and the operator gets the
    // list of names that would have worked.
    assert!(forum_applied_tags(&channel, Some("typo-in-config")).is_empty());
    assert_eq!(forum_tag_names(&channel), vec!["cron-report"]);
}
