//! Tests for the FR-010 scheduled-event mirror.
//!
//! Everything here is on the pure half of `cron::scheduled_events`: the job
//! table in, the plan out, no network and no wall clock. The driver (`sync`)
//! is a thin loop over that plan, so pinning the plan is pinning the behaviour
//! that matters, and AC-013's "table versus event list" comparison is exactly
//! what these tests perform.

use crate::cron::scheduled_events::{ExistingEvent, event_name, plan_events, project_events};
use crate::db::models::CronJob;
use chrono::{DateTime, Utc};

/// 2027-01-15T08:00:00Z, chosen so a 09:00 daily job has an unambiguous next
/// run one hour later.
const NOW_SECS: i64 = 1_800_000_000;
/// The next `0 9 * * *` fire after [`NOW_SECS`].
const NEXT_NINE_UTC: i64 = NOW_SECS + 3600;
const CHANNEL: &str = "1234567890123456789";

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(NOW_SECS, 0).expect("valid test instant")
}

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid test instant")
}

fn job(name: &str, cron: &str, timezone: &str, deliver_to: Option<String>) -> CronJob {
    CronJob::new(
        name.to_string(),
        cron.to_string(),
        timezone.to_string(),
        "Test prompt".to_string(),
        None,
        None,
        "off".to_string(),
        true,
        deliver_to,
        None,
    )
}

/// A daily 09:00 UTC job reporting into [`CHANNEL`].
fn discord_job(name: &str) -> CronJob {
    job(name, "0 9 * * *", "UTC", Some(format!("discord:{CHANNEL}")))
}

fn existing(id: &str, name: &str, start: DateTime<Utc>) -> ExistingEvent {
    ExistingEvent {
        id: id.to_string(),
        name: name.to_string(),
        start,
    }
}

#[test]
fn an_enabled_discord_job_becomes_one_event_at_its_next_fire_time() {
    let projection = project_events(&[discord_job("morning-brief")], now());
    assert!(
        projection.skipped.is_empty(),
        "a readable job must not be skipped: {:?}",
        projection.skipped
    );
    assert_eq!(projection.events.len(), 1);
    assert_eq!(projection.events[0].start.timestamp(), NEXT_NINE_UTC);
}

#[test]
fn the_table_maps_to_exactly_the_expected_event_list() {
    let mut paused = discord_job("nightly-digest");
    paused.enabled = false;
    let jobs = vec![
        discord_job("morning-brief"),
        job(
            "telegram-only",
            "0 9 * * *",
            "UTC",
            Some("telegram:12345".to_string()),
        ),
        paused,
    ];
    let projection = project_events(&jobs, now());
    let mapped: Vec<(&str, i64)> = projection
        .events
        .iter()
        .map(|event| (event.name.as_str(), event.start.timestamp()))
        .collect();
    assert_eq!(mapped, vec![("morning-brief", NEXT_NINE_UTC)]);
    assert!(projection.skipped.is_empty());
}

#[test]
fn a_job_that_delivers_nowhere_on_discord_is_not_mirrored() {
    let jobs = vec![job(
        "telegram-only",
        "0 9 * * *",
        "UTC",
        Some("telegram:12345".to_string()),
    )];
    let projection = project_events(&jobs, now());
    assert!(projection.events.is_empty());
    // Not mirrored is not the same as unreadable: nothing was wrong with it.
    assert!(projection.skipped.is_empty());
}

#[test]
fn a_job_with_no_destination_at_all_is_not_mirrored() {
    let jobs = vec![job("session-only", "0 9 * * *", "UTC", None)];
    let projection = project_events(&jobs, now());
    assert!(projection.events.is_empty());
    assert!(projection.skipped.is_empty());
}

#[test]
fn a_paused_job_stops_being_mirrored() {
    let mut paused = discord_job("morning-brief");
    paused.enabled = false;
    let projection = project_events(&[paused], now());
    assert!(
        projection.events.is_empty(),
        "a paused job must not keep announcing a run"
    );
}

#[test]
fn an_unknown_timezone_is_reported_rather_than_dropped_silently() {
    let jobs = vec![job(
        "morning-brief",
        "0 9 * * *",
        "Mars/Phobos",
        Some(format!("discord:{CHANNEL}")),
    )];
    let projection = project_events(&jobs, now());
    assert!(projection.events.is_empty());
    assert_eq!(projection.skipped.len(), 1);
    assert!(
        projection.skipped[0].1.contains("Mars/Phobos"),
        "the reason must name the bad value: {:?}",
        projection.skipped[0].1
    );
}

#[test]
fn an_unreadable_cron_expression_is_reported_rather_than_dropped_silently() {
    let jobs = vec![job(
        "morning-brief",
        "every morning please",
        "UTC",
        Some(format!("discord:{CHANNEL}")),
    )];
    let projection = project_events(&jobs, now());
    assert!(projection.events.is_empty());
    assert_eq!(projection.skipped.len(), 1);
    assert!(projection.skipped[0].1.contains("every morning please"));
}

#[test]
fn a_forum_delivery_target_is_mirrored_at_the_bare_channel_id() {
    let jobs = vec![job(
        "morning-brief",
        "0 9 * * *",
        "UTC",
        Some(format!("discord:{CHANNEL}:forum")),
    )];
    let projection = project_events(&jobs, now());
    assert_eq!(projection.events.len(), 1);
    // `:forum` is a delivery mode, not a second address (#1851).
    assert_eq!(projection.events[0].location, format!("<#{CHANNEL}>"));
}

#[test]
fn the_event_carries_the_schedule_in_the_words_the_owner_wrote() {
    let jobs = vec![job(
        "morning-brief",
        "0 9 * * *",
        "Asia/Jakarta",
        Some(format!("discord:{CHANNEL}")),
    )];
    let projection = project_events(&jobs, now());
    let description = &projection.events[0].description;
    assert!(description.contains("0 9 * * *"), "{description}");
    assert!(description.contains("Asia/Jakarta"), "{description}");
    assert!(description.contains("morning-brief"), "{description}");
}

#[test]
fn an_event_that_already_matches_the_table_is_left_alone() {
    let projection = project_events(&[discord_job("morning-brief")], now());
    let existing = vec![existing("900", "morning-brief", at(NEXT_NINE_UTC))];
    let plan = plan_events(&existing, &projection.events);
    assert!(plan.is_empty(), "{plan:?}");
}

#[test]
fn an_event_whose_job_is_gone_is_deleted() {
    let projection = project_events(&[discord_job("morning-brief")], now());
    let existing = vec![
        existing("900", "morning-brief", at(NEXT_NINE_UTC)),
        existing("901", "retired-job", at(NEXT_NINE_UTC)),
    ];
    let plan = plan_events(&existing, &projection.events);
    assert_eq!(plan.delete, vec!["901".to_string()]);
    assert!(plan.create.is_empty());
    assert!(plan.update.is_empty());
}

#[test]
fn a_job_whose_time_moved_moves_its_event_instead_of_replacing_it() {
    let projection = project_events(&[discord_job("morning-brief")], now());
    let existing = vec![existing("900", "morning-brief", at(NOW_SECS))];
    let plan = plan_events(&existing, &projection.events);
    assert!(plan.create.is_empty());
    assert!(plan.delete.is_empty());
    assert_eq!(plan.update.len(), 1);
    assert_eq!(plan.update[0].0, "900");
    assert_eq!(plan.update[0].1.start.timestamp(), NEXT_NINE_UTC);
}

#[test]
fn a_start_time_that_differs_below_the_second_is_not_a_move() {
    let projection = project_events(&[discord_job("morning-brief")], now());
    // Discord echoes its own sub-second precision, so an exact equality test
    // would rewrite every event on every pass.
    let echoed = DateTime::from_timestamp(NEXT_NINE_UTC, 500_000_000).expect("valid instant");
    let existing = vec![existing("900", "morning-brief", echoed)];
    let plan = plan_events(&existing, &projection.events);
    assert!(plan.is_empty(), "{plan:?}");
}

#[test]
fn a_job_added_to_the_table_is_created_without_touching_the_others() {
    let projection = project_events(
        &[discord_job("morning-brief"), discord_job("evening-wrap")],
        now(),
    );
    let existing = vec![existing("900", "morning-brief", at(NEXT_NINE_UTC))];
    let plan = plan_events(&existing, &projection.events);
    assert!(plan.delete.is_empty());
    assert!(plan.update.is_empty());
    assert_eq!(plan.create.len(), 1);
    assert_eq!(plan.create[0].name, "evening-wrap");
}

#[test]
fn two_jobs_sharing_a_name_are_mirrored_once() {
    let projection = project_events(
        &[discord_job("morning-brief"), discord_job("morning-brief")],
        now(),
    );
    let plan = plan_events(&[], &projection.events);
    assert_eq!(
        plan.create.len(),
        1,
        "one name is one event, however many jobs carry it"
    );
}

#[test]
fn a_long_job_name_is_trimmed_to_the_event_name_ceiling() {
    let long = "j".repeat(150);
    assert_eq!(event_name(&long).chars().count(), 100);
}

#[test]
fn a_blank_job_name_still_yields_a_usable_event_name() {
    assert_eq!(event_name("   "), "Scheduled job");
}

#[test]
fn a_job_name_is_trimmed_before_it_is_used_as_the_event_name() {
    assert_eq!(event_name("  morning-brief  "), "morning-brief");
}

#[test]
fn the_scheduled_event_guild_defaults_to_unset() {
    // The knob is opt-in: an install that never sets it makes no
    // scheduled-event request at all, which is the pre-FR-010 behaviour.
    let discord = crate::config::DiscordConfig::default();
    assert!(discord.scheduled_events_guild.is_none());
}
