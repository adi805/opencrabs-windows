//! Mirror the cron job table onto Discord guild scheduled events (FR-010).
//!
//! A scheduled agent job is invisible in Discord until the moment it fires:
//! members see a report appear with no warning and no idea when the next one
//! lands. Discord has a native answer for "something happens at a known time",
//! the guild scheduled event, so this module projects the `cron_jobs` table
//! onto it. Every enabled job that delivers to Discord becomes one event whose
//! start time is that job's next fire time; an event whose job is gone, or
//! paused, is removed.
//!
//! The split is deliberate. [`project_events`] and [`plan_events`] are pure:
//! they take the job rows and the events that already exist, and return what to
//! create, move and delete, so the mapping is pinned by a test that needs
//! neither a network nor a clock. [`sync_if_due`] is the thin throttled driver
//! that reads the guild and applies that plan.
//!
//! Matching is by event name, and only events the bot itself created are
//! candidates (the driver filters on `creator_id`), so an event a moderator
//! made by hand in the same guild is never renamed or deleted by this code.
//!
//! Events are `External`, the type that needs a location line rather than a
//! voice channel: a cron job reports into a text or forum channel, so there is
//! no voice stage to attach it to. Discord requires an external event to carry
//! an end time, so each one claims a fixed hour that is a display requirement,
//! not a claim about how long the job takes.

use crate::db::models::CronJob;
use crate::utils::string::truncate_chars;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use uuid::Uuid;

/// Discord caps an event name at 100 characters and its description at 1000.
const EVENT_NAME_MAX_CHARS: usize = 100;
const EVENT_DESCRIPTION_MAX_CHARS: usize = 1000;

/// The name an event falls back to when the job's own name is blank. Discord
/// refuses an empty name, and a refusal at write time is a far worse way to
/// learn that a job is unnamed.
const FALLBACK_EVENT_NAME: &str = "Scheduled job";

/// How often the guild is reconciled.
///
/// This mirror is not a scheduler: nothing here has to be prompt, and every
/// pass costs one guild read plus one write per change, so fifteen minutes is
/// frequent enough that an edited job looks current and rare enough that the
/// feature stays invisible in the API budget. The scheduler tick is 60 seconds,
/// so without this the read alone would be 1,440 calls a day.
const SYNC_INTERVAL_SECS: i64 = 15 * 60;

/// The last sync attempt, in unix seconds; 0 means never.
static LAST_SYNC: AtomicI64 = AtomicI64::new(0);

/// The audit-log reason every mirrored write carries, so a moderator reading
/// the audit log can tell automation from a human edit.
#[cfg(feature = "discord")]
const MIRROR_REASON: &str = "OpenCrabs cron job mirror (FR-010)";

/// How long a mirrored event claims to run.
///
/// Discord requires an end time on an external event, and a cron job has no
/// duration: it fires and reports. An hour is what makes the event valid, not a
/// claim about how long the work takes, so it is a constant rather than a knob.
#[cfg(feature = "discord")]
const EVENT_DURATION_SECS: i64 = 60 * 60;

/// One event the job table says should exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEvent {
    /// The job this event mirrors.
    pub job_id: Uuid,
    /// The event's name, which is also its identity across passes.
    pub name: String,
    /// The job's next fire time, which is the event's start time.
    pub start: DateTime<Utc>,
    /// Where the job's report lands, for the event's location line.
    pub location: String,
    /// The schedule and timezone, for the event's description.
    pub description: String,
}

/// An event that already exists in the guild, reduced to the fields the
/// reconciliation compares.
///
/// The id stays a string so this module carries no `serenity` types and stays
/// testable without the `discord` feature compiled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingEvent {
    pub id: String,
    pub name: String,
    pub start: DateTime<Utc>,
}

/// What has to happen for the guild to match the table.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EventPlan {
    pub create: Vec<PlannedEvent>,
    pub update: Vec<(String, PlannedEvent)>,
    pub delete: Vec<String>,
}

impl EventPlan {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty() && self.update.is_empty() && self.delete.is_empty()
    }
}

/// The events the job table says should exist, plus the jobs that could not be
/// projected and why.
///
/// The skipped list is the point of returning a struct rather than a bare
/// `Vec`: a job whose timezone or expression cannot be read produces no event,
/// and "no event" is indistinguishable from "no such job" unless the reason is
/// carried out to a log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub events: Vec<PlannedEvent>,
    pub skipped: Vec<(Uuid, String)>,
}

/// The event name a job is mirrored under.
///
/// The name is owner-written, so both ends are handled here rather than
/// discovered as a 400 at write time: a blank name falls back, and an over-long
/// one is trimmed to Discord's ceiling instead of refusing the mirror.
pub fn event_name(job_name: &str) -> String {
    let trimmed = job_name.trim();
    if trimmed.is_empty() {
        return FALLBACK_EVENT_NAME.to_string();
    }
    truncate_chars(trimmed, EVENT_NAME_MAX_CHARS).to_string()
}

/// The event description: the schedule in the words the owner wrote it in.
fn event_description(job: &CronJob) -> String {
    let text = format!(
        "OpenCrabs scheduled job '{}'. Runs {} ({}).",
        job.name.trim(),
        job.cron_expr.trim(),
        job.timezone.trim()
    );
    truncate_chars(&text, EVENT_DESCRIPTION_MAX_CHARS).to_string()
}

/// The Discord channel a job delivers to, if any.
///
/// A job may name several targets; the first Discord one wins, because the
/// event needs a single location line and the job is one event either way. A
/// job that delivers nowhere Discord-shaped is not a skip: it is simply not
/// this feature's business.
fn discord_channel(job: &CronJob) -> Option<String> {
    let deliver_to = job.deliver_to.as_deref()?;
    deliver_to
        .split(',')
        .map(str::trim)
        .filter_map(|target| target.strip_prefix("discord:"))
        .find_map(super::scheduler::parse_discord_target)
        .map(|(channel_id, _)| channel_id)
}

/// The events the job table says should exist, as of `now`.
///
/// The start time comes from `cron_expr` and `timezone`, not from the stored
/// `next_run_at`: that column is scheduler bookkeeping which only advances when
/// the scheduler runs, so a job whose scheduler never fired would publish an
/// event in the past. The expression is the schedule of record.
pub fn project_events(jobs: &[CronJob], now: DateTime<Utc>) -> Projection {
    let mut events: Vec<PlannedEvent> = Vec::new();
    let mut skipped: Vec<(Uuid, String)> = Vec::new();
    for job in jobs {
        // A paused job is not mirrored either: disabling is the reversible way
        // to say "stop announcing this", and an event that outlives its pause
        // would announce a run that is not going to happen.
        if !job.enabled {
            continue;
        }
        let Some(channel_id) = discord_channel(job) else {
            continue;
        };
        let Some(tz) = super::parse_timezone(&job.timezone) else {
            skipped.push((job.id, format!("unknown timezone '{}'", job.timezone)));
            continue;
        };
        let Some(start) = super::next_run_utc(&job.cron_expr, tz, now) else {
            skipped.push((
                job.id,
                format!("unreadable cron expression '{}'", job.cron_expr),
            ));
            continue;
        };
        events.push(PlannedEvent {
            job_id: job.id,
            name: event_name(&job.name),
            start,
            location: format!("<#{channel_id}>"),
            description: event_description(job),
        });
    }
    // Earliest first, so a duplicate name resolves to the job that fires first
    // and the order is stable across passes.
    events.sort_by_key(|event| event.start);
    Projection { events, skipped }
}

/// What to create, move and delete so the guild matches the table.
///
/// Matching is by name, which is why the name is the identity: a renamed job
/// arrives as a delete plus a create, and that is the honest description of
/// what happened. Start times are compared to the second, because Discord
/// echoes a timestamp with its own sub-second precision and an exact equality
/// test would rewrite every event on every pass.
///
/// The description and location are not compared: the start time is what a
/// member reads, and both of the others are rewritten whenever an event is
/// created or moved anyway.
pub fn plan_events(existing: &[ExistingEvent], planned: &[PlannedEvent]) -> EventPlan {
    let mut plan = EventPlan::default();
    let mut wanted: HashMap<&str, &PlannedEvent> = HashMap::new();
    for event in planned {
        // A duplicate name keeps the first entry, and `planned` is sorted by
        // start time, so the earliest job owns the event. Two jobs cannot both
        // be mirrored under one name.
        wanted.entry(event.name.as_str()).or_insert(event);
    }
    for event in existing {
        match wanted.get(event.name.as_str()) {
            Some(target) if event.start.timestamp() != target.start.timestamp() => {
                plan.update.push((event.id.clone(), (*target).clone()));
            }
            Some(_) => {}
            None => plan.delete.push(event.id.clone()),
        }
    }
    let have: HashSet<&str> = existing.iter().map(|event| event.name.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for event in planned {
        let name = event.name.as_str();
        if !seen.insert(name) {
            continue;
        }
        if !have.contains(name) {
            plan.create.push(event.clone());
        }
    }
    plan
}

/// Reconcile the guild with the job table, at most once per
/// [`SYNC_INTERVAL_SECS`].
///
/// Called from the scheduler tick, so it never returns an error: a mirror that
/// cannot be refreshed must not stop the schedule it mirrors. The failure is
/// logged with its full chain and retried on the next pass.
///
/// The work runs inline rather than in a spawned task on purpose. `tokio::spawn`
/// drops the task-local profile home the scheduler runs inside, and this path
/// reads both `keys.toml` and the config, so a spawned mirror would read the
/// wrong profile's credentials in a multi-profile daemon. The throttle is what
/// keeps that honest: one possibly slow tick every fifteen minutes.
pub(crate) async fn sync_if_due(jobs: &[CronJob]) {
    let now = Utc::now().timestamp();
    let last = LAST_SYNC.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < SYNC_INTERVAL_SECS {
        return;
    }
    LAST_SYNC.store(now, Ordering::Relaxed);
    apply(jobs).await;
}

#[cfg(feature = "discord")]
async fn apply(jobs: &[CronJob]) {
    if let Err(e) = sync(jobs).await {
        tracing::warn!("Discord scheduled-event mirror failed: {e:#}");
    }
}

/// Without the `discord` feature there is no client to mirror through, so the
/// tick's call is a no-op rather than a compile error.
#[cfg(not(feature = "discord"))]
async fn apply(_jobs: &[CronJob]) {}

/// The guild this instance mirrors into, or `None` when the feature is off.
///
/// Unset (the default) means nothing is read, written or deleted, so an install
/// that never sets the knob behaves exactly as it did before this module
/// existed.
#[cfg(feature = "discord")]
fn mirror_guild() -> Option<u64> {
    let raw = crate::config::Config::current()
        .channels
        .discord
        .scheduled_events_guild
        .clone()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse() {
        Ok(id) => Some(id),
        Err(_) => {
            tracing::warn!(
                "channels.discord.scheduled_events_guild is '{trimmed}', not a numeric guild id; \
                 the scheduled-event mirror stays off"
            );
            None
        }
    }
}

/// The guild's own scheduled events, reduced to the ones this bot created.
#[cfg(feature = "discord")]
async fn existing_events(
    http: &serenity::http::Http,
    guild: serenity::model::id::GuildId,
    bot: serenity::model::id::UserId,
) -> anyhow::Result<Vec<ExistingEvent>> {
    let events = http.get_scheduled_events(guild, false).await?;
    Ok(events
        .into_iter()
        .filter(|event| event.creator_id == Some(bot))
        .map(|event| ExistingEvent {
            id: event.id.to_string(),
            name: event.name.clone(),
            start: DateTime::from_timestamp(event.start_time.unix_timestamp(), 0)
                .unwrap_or_default(),
        })
        .collect())
}

/// Read the guild, diff it against the table, and apply the difference.
#[cfg(feature = "discord")]
async fn sync(jobs: &[CronJob]) -> anyhow::Result<()> {
    use serenity::builder::{CreateScheduledEvent, EditScheduledEvent};
    use serenity::model::Timestamp;
    use serenity::model::guild::ScheduledEventType;
    use serenity::model::id::{GuildId, ScheduledEventId};

    let Some(guild_id) = mirror_guild() else {
        return Ok(());
    };
    let Some(token) = super::scheduler::read_channel_secret("discord", "token") else {
        anyhow::bail!("no Discord bot token in keys.toml");
    };

    let http = serenity::http::Http::new(&token);
    let bot = http.get_current_user().await?.id;
    let guild = GuildId::new(guild_id);
    let existing = existing_events(&http, guild, bot).await?;

    let projection = project_events(jobs, Utc::now());
    for (job_id, reason) in &projection.skipped {
        tracing::warn!("Scheduled-event mirror left cron job {job_id} alone: {reason}");
    }

    let plan = plan_events(&existing, &projection.events);
    if plan.is_empty() {
        return Ok(());
    }
    tracing::info!(
        "Discord scheduled-event mirror on guild {guild_id}: {} to create, {} to move, \
         {} to delete",
        plan.create.len(),
        plan.update.len(),
        plan.delete.len()
    );

    for event in &plan.create {
        let start = Timestamp::from_unix_timestamp(event.start.timestamp())?;
        let end = Timestamp::from_unix_timestamp(event.start.timestamp() + EVENT_DURATION_SECS)?;
        let builder = CreateScheduledEvent::new(ScheduledEventType::External, &event.name, start)
            .description(&event.description)
            .end_time(end)
            .location(&event.location)
            .audit_log_reason(MIRROR_REASON);
        if let Err(e) = http
            .create_scheduled_event(guild, &builder, Some(MIRROR_REASON))
            .await
        {
            tracing::warn!("Could not create scheduled event '{}': {e}", event.name);
        }
    }

    for (event_id, event) in &plan.update {
        let Ok(id) = event_id.parse::<u64>() else {
            tracing::warn!("Scheduled-event mirror has an unreadable event id '{event_id}'");
            continue;
        };
        let start = Timestamp::from_unix_timestamp(event.start.timestamp())?;
        let end = Timestamp::from_unix_timestamp(event.start.timestamp() + EVENT_DURATION_SECS)?;
        let builder = EditScheduledEvent::new()
            .name(&event.name)
            .start_time(start)
            .description(&event.description)
            .end_time(end)
            .audit_log_reason(MIRROR_REASON);
        if let Err(e) = http
            .edit_scheduled_event(
                guild,
                ScheduledEventId::new(id),
                &builder,
                Some(MIRROR_REASON),
            )
            .await
        {
            tracing::warn!("Could not move scheduled event '{}': {e}", event.name);
        }
    }

    for event_id in &plan.delete {
        let Ok(id) = event_id.parse::<u64>() else {
            tracing::warn!("Scheduled-event mirror has an unreadable event id '{event_id}'");
            continue;
        };
        if let Err(e) = http
            .delete_scheduled_event(guild, ScheduledEventId::new(id))
            .await
        {
            tracing::warn!("Could not delete scheduled event {event_id}: {e}");
        }
    }
    Ok(())
}
