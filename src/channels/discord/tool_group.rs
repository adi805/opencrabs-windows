//! Collapsible tool-call group for Discord (#380), matching the Telegram
//! block and the Slack Block Kit port: ONE message per turn, collapsed to
//! a live summary with an Expand button, toggled in place via component
//! interaction. State lives in [`super::DiscordState`] keyed by message id
//! so the click handler can re-render after the turn's closures are gone.
//! Expansion is per-message: everyone in the channel shares it.
//!
//! With `trace_narration` enabled the same bubble also carries the turn's
//! intermediate narration as dim subtext notes (agent-disco-style live
//! trace): one editable work-log per turn instead of one message per
//! intermediate.

use serenity::builder::{CreateActionRow, CreateButton};
use serenity::model::application::ButtonStyle;
use std::time::{Duration, Instant};

use super::DiscordState;

/// One tool row in a group.
#[derive(Debug, Clone)]
pub(crate) struct GroupEntry {
    pub name: String,
    pub context: String,
    /// None = running, Some(success) = finished.
    pub status: Option<bool>,
}

/// A turn's tool group: contents plus display state.
#[derive(Debug, Clone)]
pub(crate) struct GroupState {
    pub entries: Vec<GroupEntry>,
    /// Narration lines folded into the bubble (live trace). Authoritative
    /// state lives in [`DiscordState`]; only [`DiscordState::append_note`]
    /// and [`DiscordState::drop_note_if`] mutate them —
    /// [`DiscordState::upsert_tool_group`] preserves the stored notes the
    /// way it preserves `expanded`.
    pub notes: Vec<String>,
    pub expanded: bool,
    /// Turn-start anchor for the live `🕒` segment and the settled `⏱` one
    /// (#1841). Stamped at first insert and preserved across updates so the
    /// clock never restarts mid-turn.
    pub started_at: Instant,
    /// Post-delivery status (#1841). `None` while the turn is live; stamped
    /// once by [`DiscordState::settle_tool_group`] and preserved by every
    /// later upsert. `elapsed` freezes at settle so toggling Expand later
    /// never grows the clock.
    pub settled: Option<SettledStatus>,
    /// When the card last changed, stamped on every tool/note update
    /// (FR-006). The live line compares it against the configured
    /// silence threshold so a stalled turn says so instead of looking
    /// frozen.
    pub last_activity_at: Instant,
    /// Live ctx budget line while the turn runs (#1841 parity). The
    /// settled chrome already carries `ctx`; this is the same string
    /// streamed from `ProgressEvent::TokenCount` mid-turn, so the card shows
    /// the budget as it grows instead of only at settle. `None` until the
    /// first token-count event; preserved by every upsert like `notes`.
    pub live_ctx: Option<String>,
}

/// The durable form of a group (H1).
///
/// `Instant` is process-relative and cannot be serialised, so the two clock
/// fields travel as ages in seconds and are rebuilt against the new process's
/// own epoch on load. Nothing else in the card is derived from wall time, so
/// the render a restarted process produces matches the one the dead process
/// left on screen.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct GroupSnapshot {
    pub entries: Vec<EntrySnapshot>,
    pub notes: Vec<String>,
    pub expanded: bool,
    /// Whole seconds since `started_at`, measured at the moment of the write.
    pub started_secs_ago: u64,
    /// Whole seconds since `last_activity_at`, measured at the write.
    pub activity_secs_ago: u64,
    pub settled: Option<SettledSnapshot>,
    pub live_ctx: Option<String>,
}

/// One tool row, in the shape that survives a restart.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct EntrySnapshot {
    pub name: String,
    pub context: String,
    pub status: Option<bool>,
}

/// The frozen post-delivery chrome, in the shape that survives a restart.
///
/// `icon` is deliberately NOT stored. It is a function of the outcome and of
/// whether the turn settled with background work still alive, and both of
/// those travel already (`outcome`, `waiting`), so a stored copy would be a
/// second source of truth that the next icon change desyncs silently.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SettledSnapshot {
    pub outcome: String,
    pub elapsed_secs: u64,
    pub ctx: Option<String>,
    pub verb: String,
    /// True when settle stamped the `⏳ Waiting for …` pair because detached
    /// work was still alive (#1144/#1183). The verb itself is stored verbatim,
    /// since it names the counts.
    pub waiting: bool,
}

impl GroupState {
    /// The durable form of this card (H1).
    pub(crate) fn to_snapshot(&self) -> GroupSnapshot {
        let now = Instant::now();
        GroupSnapshot {
            entries: self
                .entries
                .iter()
                .map(|e| EntrySnapshot {
                    name: e.name.clone(),
                    context: e.context.clone(),
                    status: e.status,
                })
                .collect(),
            notes: self.notes.clone(),
            expanded: self.expanded,
            started_secs_ago: now.duration_since(self.started_at).as_secs(),
            activity_secs_ago: now.duration_since(self.last_activity_at).as_secs(),
            settled: self.settled.as_ref().map(|s| SettledSnapshot {
                outcome: s.outcome.as_wire().to_string(),
                elapsed_secs: s.elapsed.as_secs(),
                ctx: s.ctx.clone(),
                verb: s.verb.clone(),
                waiting: s.icon == WAITING_ICON,
            }),
            live_ctx: self.live_ctx.clone(),
        }
    }

    /// Rebuild a card from its snapshot. `None` when the snapshot carries an
    /// outcome this build cannot name: an unrenderable card must answer "too
    /// old" rather than invent a status for the one card the user is watching.
    pub(crate) fn from_snapshot(snap: GroupSnapshot) -> Option<Self> {
        let settled = match snap.settled {
            Some(s) => {
                let outcome = TurnOutcome::from_wire(&s.outcome)?;
                Some(SettledStatus {
                    outcome,
                    elapsed: Duration::from_secs(s.elapsed_secs),
                    ctx: s.ctx,
                    icon: if s.waiting {
                        WAITING_ICON
                    } else {
                        outcome.icon_verb().0
                    },
                    verb: s.verb,
                })
            }
            None => None,
        };
        let now = Instant::now();
        Some(Self {
            entries: snap
                .entries
                .into_iter()
                .map(|e| GroupEntry {
                    name: e.name,
                    context: e.context,
                    status: e.status,
                })
                .collect(),
            notes: snap.notes,
            expanded: snap.expanded,
            // Ages, not instants: a restarted process has its own epoch, so the
            // card resumes the clock it was showing instead of restarting it.
            started_at: now - Duration::from_secs(snap.started_secs_ago),
            last_activity_at: now - Duration::from_secs(snap.activity_secs_ago),
            live_ctx: snap.live_ctx,
            settled,
        })
    }
}

/// Frozen post-delivery chrome (#1841): the Discord twin of Slack's
/// `SettledStatus`. The clock stops at settle and the ctx budget line moves
/// into the flow group, so the chrome owns it (the answer-message footer
/// goes away in #1842, leaving the settled line as the single home).
#[derive(Debug, Clone)]
pub(crate) struct SettledStatus {
    pub outcome: TurnOutcome,
    pub elapsed: Duration,
    pub ctx: Option<String>,
    /// Icon + verb as they render on the settled line (#1144/#1183 parity).
    /// Stored, not re-derived: a turn that finished with detached work still
    /// alive overrides the `✅ Finished` pair to `⏳ Waiting for …`, and the
    /// render must show the pair it was settled with.
    pub icon: &'static str,
    pub verb: String,
}

/// Test-only constructor: the plain-finish shape with no background work
/// alive. Production settles through `settle_tool_group`, which stamps the
/// icon/verb from the live counts via [`settled_icon_verb`]; tests that pin
/// the terminal verbs (and the zero-count finish path) need the same literal
/// without a live agent, so it lives behind `cfg(test)` rather than as dead
/// code in the lib unit.
#[cfg(test)]
impl SettledStatus {
    pub(crate) fn new(outcome: TurnOutcome, elapsed: Duration, ctx: Option<String>) -> Self {
        let (icon, verb) = settled_icon_verb(outcome, 0, None);
        Self {
            outcome,
            elapsed,
            ctx,
            icon,
            verb,
        }
    }
}

/// Settled icon + verb, overridden to a waiting state when the turn finished
/// with background work still alive (#1144, #1183): the Discord twin of
/// Telegram's `settled_icon_verb` and Slack's `GroupState::settle`. A card
/// that ended with detached shell tasks used to read `✅ Finished` while work
/// was still running, and alive sub-agents live in a separate registry the
/// background-task count never read. The verb folds both, e.g. `Waiting for
/// 1 background task + 2 working agents`. Only a `Finished` outcome is
/// overridden: a failed, timed-out, or cancelled turn keeps its terminal
/// verb even when detached work is still alive.
pub(crate) fn settled_icon_verb(
    outcome: TurnOutcome,
    bg: usize,
    agent_phrase: Option<&str>,
) -> (&'static str, String) {
    if outcome == TurnOutcome::Finished && (bg > 0 || agent_phrase.is_some()) {
        let mut parts: Vec<String> = Vec::new();
        if bg > 0 {
            parts.push(if bg == 1 {
                "1 background task".to_string()
            } else {
                format!("{bg} background tasks")
            });
        }
        if let Some(phrase) = agent_phrase {
            parts.push(phrase.to_string());
        }
        return (WAITING_ICON, format!("Waiting for {}", parts.join(" + ")));
    }
    let (icon, verb) = outcome.icon_verb();
    (icon, verb.to_string())
}

/// Stamp a settled status onto a group.
///
/// Pure and store-free on purpose. The live settle path stamps through
/// `DiscordState::settle_tool_group`, while the flow ticker's orphan path has to
/// produce the exact same chrome from a snapshot it already holds, because by
/// then the group is no longer in the store to settle. Two implementations would
/// drift, and the orphan card would freeze on a different line than every other
/// card (#1880).
pub(crate) fn stamp_settled(
    group: &mut GroupState,
    outcome: TurnOutcome,
    bg: usize,
    agents: crate::channels::background_work::SubagentCounts,
    ctx: Option<String>,
) {
    let prev_ctx = group.settled.as_ref().and_then(|s| s.ctx.clone());
    let agent_phrase = (!agents.is_empty())
        .then(|| crate::channels::background_work::subagent_waiting_phrase(agents));
    let (icon, verb) = settled_icon_verb(outcome, bg, agent_phrase.as_deref());
    group.settled = Some(SettledStatus {
        outcome,
        elapsed: group.started_at.elapsed(),
        ctx: ctx.or(prev_ctx),
        icon,
        verb,
    });
}

/// Terminal outcome of a turn, stamped on the group at settle (FR-005, #1880).
/// The Discord twin of Slack's `TurnOutcome` and Telegram's `FlowOutcome`.
///
/// Without it the settled icon was derived from *tool* status, so a turn that
/// timed out or was cancelled with no failing tool rendered a green check:
/// a false success signal on the card the user is actually watching.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TurnOutcome {
    Finished,
    Failed,
    TimedOut,
    Cancelled,
    /// The group left the store before the turn settled, so the repair path
    /// has no cause to report. Deliberately NOT `TimedOut`: on the store-miss
    /// branch the turn may still be running, and a timeout verdict there tells
    /// the reader the turn died when it did not (#196).
    Orphaned,
}

impl TurnOutcome {
    /// Icon + verb for the settled line. Mirrors Slack's `icon_verb` so the
    /// three channels read the same.
    fn icon_verb(self) -> (&'static str, &'static str) {
        match self {
            Self::Finished => ("✅", "Finished"),
            Self::Failed => ("❌", "Failed"),
            Self::TimedOut => ("⏱", "Timed out"),
            Self::Cancelled => ("❌", "Cancelled"),
            Self::Orphaned => ("⚠", "Orphaned"),
        }
    }

    /// Wire name for the durable snapshot. Kept apart from `icon_verb`
    /// because this string is written to the database and read back by a later
    /// build: it is a compatibility surface, not a display detail, so renaming
    /// it is a migration and not a copy edit.
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Orphaned => "orphaned",
        }
    }

    /// Inverse of [`Self::as_wire`]. `None` for a name this build does not
    /// know: a card written by a newer build cannot be rendered honestly, and
    /// defaulting it to `Finished` would put a false success check on the one
    /// card the user is watching.
    pub(crate) fn from_wire(name: &str) -> Option<Self> {
        match name {
            "finished" => Some(Self::Finished),
            "failed" => Some(Self::Failed),
            "timed_out" => Some(Self::TimedOut),
            "cancelled" => Some(Self::Cancelled),
            "orphaned" => Some(Self::Orphaned),
            _ => None,
        }
    }
}

/// The icon settle stamps when the turn finished with detached work still
/// alive (#1144/#1183). Named because two places have to agree on it: the
/// stamp, and the snapshot that has to say on the way back whether the stored
/// pair was the waiting one.
pub(crate) const WAITING_ICON: &str = "⏳";

/// The `kind` this module's cards carry in `discord_card_state` (H1). One
/// table serves every Discord card that has to survive a restart, and this
/// column is what keeps a tool group from being read back as a pager.
pub(crate) const TOOL_GROUP_KIND: &str = "tool_group";

/// Keep at most this many narration lines in the bubble (newest win).
pub(crate) const NOTE_CAP: usize = 6;

/// Clip each narration line to this many chars — the bubble stays a glance,
/// not a transcript.
pub(crate) const NOTE_MAX_CHARS: usize = 160;

/// First non-empty line, trimmed to [`NOTE_MAX_CHARS`] — the bubble form of
/// one narration event.
pub(crate) fn clip_note(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(NOTE_MAX_CHARS).collect();
    if line.chars().count() > NOTE_MAX_CHARS {
        out.push('…');
    }
    out
}

/// Narration lines as Discord subtext (`-# ` renders dim and small).
fn notes_block(notes: &[String]) -> String {
    notes
        .iter()
        .map(|n| format!("-# {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn entry_icon(status: Option<bool>) -> &'static str {
    match status {
        None => "⚙️",
        Some(true) => "✅",
        Some(false) => "❌",
    }
}

/// `M:SS` elapsed clock (`H:MM:SS` past an hour), the Discord twin of
/// Telegram's flow clock. The glyph lives with the caller so the live and
/// settled segments can differ (`🕒` rolls, `⏱️` freezes at settle).
fn format_clock(elapsed: Duration) -> String {
    let (h, m, s) = (
        elapsed.as_secs() / 3600,
        (elapsed.as_secs() % 3600) / 60,
        elapsed.as_secs() % 60,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Idle threshold for the "still working" line (FR-006), read from
/// `[channels.discord.progress] silence_warning_secs`. `0` disables.
fn silence_threshold() -> Option<Duration> {
    let secs = crate::config::Config::current()
        .channels
        .discord
        .progress
        .silence_warning_secs;
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// The "still working" segment for a live turn that has gone quiet longer
/// than the configured threshold (FR-006, AC-012). Names the last activity so
/// a stalled card never reads as a hang. `None` while fresh, or once settled.
fn silence_segment(group: &GroupState) -> Option<String> {
    if group.settled.is_some() {
        return None;
    }
    let threshold = silence_threshold()?;
    let idle = group.last_activity_at.elapsed();
    if idle < threshold {
        return None;
    }
    let idle = format_clock(idle);
    Some(match activity_segment(group) {
        Some(activity) => format!("⚠️ still working · {activity} · no update for {idle}"),
        None => format!("⚠️ still working · no update for {idle}"),
    })
}

/// Longest activity segment in the live flow line (#1844). Display-only
/// cap: the line stays a glance, not a transcript.
const ACTIVITY_MAX_CHARS: usize = 100;

fn activity_segment(group: &GroupState) -> Option<String> {
    let text = latest_activity(group)?;
    let clipped: String = text.chars().take(ACTIVITY_MAX_CHARS).collect();
    let out = if text.chars().count() > ACTIVITY_MAX_CHARS {
        format!("{clipped}…")
    } else {
        clipped
    };
    (!out.is_empty()).then_some(out)
}

/// Live activity preview for the flow line (#1844): what the agent is doing
/// right now, the Discord twin of Slack's `latest_activity` (#1809) and
/// Telegram's `latest_activity_preview` (telegram/flow.rs). Same
/// priorities: latest human-readable narration note, then line-start `#`
/// comments from the latest bash command, then the latest tool label +
/// context. Discord keeps notes in their own `Vec` (`GroupState.notes`),
/// not as entries.
fn latest_activity(group: &GroupState) -> Option<String> {
    if let Some(text) = group
        .notes
        .iter()
        .rev()
        .find_map(|n| crate::channels::telegram::flow::human_readable_preview(n))
    {
        return Some(text);
    }
    if let Some(comments) = group.entries.iter().rev().find_map(|e| {
        if e.name == "bash" {
            crate::channels::telegram::flow::extract_status_from_text(&e.context)
        } else {
            None
        }
    }) {
        return Some(comments);
    }
    group
        .entries
        .iter()
        .rev()
        .map(|e| {
            let ctx = e.context.trim_start();
            if ctx.is_empty() {
                e.name.clone()
            } else {
                // Contexts are stored with a leading space (` (arg0)`): trim,
                // then join with one canonical space (the #1809 double-space fix).
                format!("{} {ctx}", e.name)
            }
        })
        .next()
}

/// `show_activity` is false when the caller renders the narration notes
/// underneath anyway (an expanded card): the live arm leads with the latest
/// note as its activity segment, so leaving it in *and* printing the notes
/// block repeated the same sentence — once above the tool rows, once below.
fn summary_line(group: &GroupState, show_activity: bool) -> String {
    let n = group.entries.len();
    let failed = group
        .entries
        .iter()
        .filter(|e| e.status == Some(false))
        .count();
    let counts = format!("**{n} tool call{}**", if n == 1 { "" } else { "s" });
    match &group.settled {
        Some(s) => {
            // Settled chrome (#1841), outcome-honest since FR-005 (#1880):
            // the icon and verb are the pair stamped at settle, outcome
            // derived, except a Finished turn still waiting on background
            // work, which settled to `⏳ Waiting for …` (#1144/#1183). The
            // failure count stays as supporting detail, never as the
            // primary signal.
            let (icon, verb) = (s.icon, s.verb.as_str());
            let tail = if failed > 0 {
                format!(" · {failed} failed")
            } else {
                String::new()
            };
            let mut line = format!("{icon} {verb} · {counts}{tail}");
            // AC-010: a turn that did not finish cleanly must SAY so. The user
            // is looking at a partial result and has no other signal that the
            // work stopped early — the tool count alone cannot tell them.
            if s.outcome != TurnOutcome::Finished {
                line.push_str(" · result may be incomplete");
            }
            if let Some(ctx) = &s.ctx {
                line.push_str(&format!(" · {ctx}"));
            }
            line.push_str(&format!(" · ⏱️ {}", format_clock(s.elapsed)));
            line
        }
        None => {
            // Live line (#1844): activity text leads when there is any
            // (latest note > bash # comments > tool label), bare counts
            // shape otherwise (zero-entry turn-start shell). Settled arm
            // above stays clean.
            let running = group.entries.iter().filter(|e| e.status.is_none()).count();
            let (icon, tail) = if running > 0 {
                ("⚙️", format!(" · {running} running"))
            } else if failed > 0 {
                ("❌", format!(" · {failed} failed"))
            } else {
                ("✅", String::new())
            };
            let clock = format!("🕒 {}", format_clock(group.started_at.elapsed()));
            let ctx_segment = match &group.live_ctx {
                Some(ctx) => format!(" · {ctx}"),
                None => String::new(),
            };
            let base = match activity_segment(group) {
                Some(activity) if show_activity => {
                    format!("{icon} {activity} · {counts}{tail}{ctx_segment} · {clock}")
                }
                _ => format!("{icon} {counts}{tail}{ctx_segment} · {clock}"),
            };
            match silence_segment(group) {
                Some(silence) => format!("{base}\n{silence}"),
                None => base,
            }
        }
    }
}

/// Discord hard-caps message content at 2000 chars (#1949): anything
/// longer is rejected on the wire, and a rejected toggle response leaves
/// the interaction unresolved — the client then blames a timeout. Every
/// render path (live edits, settle, Expand responses) fits this cap by
/// construction.
pub(crate) const CONTENT_MAX_CHARS: usize = 2000;

/// Room reserved for the omission marker so the clamped body stays under
/// [`CONTENT_MAX_CHARS`] once the marker is appended.
const OMIT_MARKER_RESERVE: usize = 80;

/// Keep newest rows of an expansion that overshoots the cap. Walks rows
/// backwards (the freshest activity is what people expand for), restores
/// chronology, and states how many rows were dropped — never silently.
fn clamp_rows(summary: String, rows: Vec<String>) -> String {
    let budget = CONTENT_MAX_CHARS - OMIT_MARKER_RESERVE;
    let mut used = summary.chars().count() + 1;
    let mut kept: Vec<String> = Vec::new();
    let mut dropped = 0usize;
    for row in rows.into_iter().rev() {
        let cost = row.chars().count() + 1;
        if used + cost > budget {
            dropped += 1;
            continue;
        }
        used += cost;
        kept.push(row);
    }
    let mut out = format!(
        "{summary}\n{}",
        kept.into_iter().rev().collect::<Vec<_>>().join("\n")
    );
    if dropped > 0 {
        out.push_str(&format!(
            "\n_{dropped} omitted to fit Discord's message limit_"
        ));
    }
    out
}

/// Final wire guard: hard-cut at the cap on a char boundary. Entry rows
/// and notes are clipped upstream, so this only exists for pathological
/// callers — an oversized message must never reach the API.
fn hard_clip(body: String) -> String {
    if body.chars().count() <= CONTENT_MAX_CHARS {
        return body;
    }
    let cut: String = body.chars().take(CONTENT_MAX_CHARS - 1).collect();
    format!("{cut}…")
}

/// Mechanical evidence footer for the turn's final answer (FR-007, #1880).
///
/// Built from the SAME `entries` the tool card renders, which are appended
/// from `ProgressEvent::ToolStarted` — the tool loop's real executions. The
/// model's prose never reaches this function, so the footer cannot claim a
/// tool the turn did not run (NFR-003, AC-014/AC-015). Wording, dedup, and
/// the cap live in [`crate::channels::evidence`], shared with Telegram so
/// the two surfaces cannot drift (NFR-002).
pub(crate) fn evidence_line(group: &GroupState) -> Option<String> {
    crate::channels::evidence::evidence_line(group.entries.iter().map(|e| e.name.as_str()))
}

/// Message body for the group in its current display state.
pub(crate) fn render_content(group: &GroupState) -> String {
    let tools_part = if group.entries.len() == 1 && !group.expanded && group.settled.is_none() {
        // Single live tool, the common case: this branch renders the bare row
        // and never reaches `summary_line`, so the live ctx budget has to be
        // appended here as well or it would only ever show on 2+ tool cards.
        let e = &group.entries[0];
        let ctx_segment = match &group.live_ctx {
            Some(ctx) => format!(" · {ctx}"),
            None => String::new(),
        };
        format!(
            "{} **{}**{}{ctx_segment}",
            entry_icon(e.status),
            e.name,
            e.context
        )
    } else if group.entries.len() == 1 && !group.expanded {
        // Single-tool card that has settled (#1144/#1183 parity): the settled
        // chrome (waiting verb, ctx budget, clock) must stay visible, so the
        // lone row rides UNDER the summary line instead of replacing it. Before
        // this, a one-tool turn rendered as the bare tool row and the settled
        // status — including `⏳ Waiting for 1 background task` — never appeared.
        let e = &group.entries[0];
        format!(
            "{}\n{} **{}**{}",
            summary_line(group, false),
            entry_icon(e.status),
            e.name,
            e.context
        )
    } else if group.expanded {
        let lines: Vec<String> = group
            .entries
            .iter()
            .map(|e| format!("{} **{}**{}", entry_icon(e.status), e.name, e.context))
            .collect();
        clamp_rows(summary_line(group, false), lines)
    } else {
        summary_line(group, true)
    };
    // Narration rows are EXPANSION-ONLY (#1990 parity). Collapsed, the card
    // stays a glance: the summary line already carries the latest narration
    // as its activity segment, so repeating every note as `-#` subtext below
    // it made the bubble a transcript nobody asked for. Expanding reveals
    // them, which is why `render_components` must offer the toggle whenever
    // notes exist — otherwise a narration-only card has no way to open them.
    let mut body = if group.expanded && !group.notes.is_empty() {
        format!("{tools_part}\n{}", notes_block(&group.notes))
    } else {
        tools_part.clone()
    };
    // Notes ride after the rows; if the whole body still overshoots, drop
    // the oldest notes first (the newest is what the ticker just wrote),
    // and hard-cut as the last resort.
    let mut notes: Vec<String> = if group.expanded {
        group.notes.clone()
    } else {
        Vec::new()
    };
    while body.chars().count() > CONTENT_MAX_CHARS && notes.len() > 1 {
        notes.remove(0);
        body = format!("{tools_part}\n{}", notes_block(&notes));
    }
    hard_clip(body)
}

/// Toggle components for the group message; empty when there is nothing to
/// reveal. Two rows of tools always count, and so does ANY narration note:
/// since notes are expansion-only (#1990), a card whose only extra content is
/// narration must still offer the way in, or the transcript is unreachable.
pub(crate) fn render_components(group: &GroupState, message_id: u64) -> Vec<CreateActionRow> {
    if group.entries.len() < 2 && group.notes.is_empty() {
        return Vec::new();
    }
    let label = if group.expanded {
        "Collapse ▲"
    } else {
        "Expand ▼"
    };
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("toolgroup:{message_id}"))
            .label(label)
            .style(ButtonStyle::Secondary),
    ])]
}

impl DiscordState {
    /// Retained tool groups; older ones stop being toggleable (their last
    /// rendered state stays on screen, like Telegram's frozen blocks).
    const TOOL_GROUP_CAP: usize = 20;

    /// Insert or update a group, PRESERVING the stored expanded/collapsed
    /// choice on updates (a completing tool must not snap an expanded group
    /// shut) and the stored narration notes (only `append_note`/`drop_note_if`
    /// mutate those). Returns the stored state so callers render what is kept.
    pub(crate) async fn upsert_tool_group(&self, message_id: u64, group: GroupState) -> GroupState {
        let (group, evicted) = self.upsert_group_in_memory(message_id, group).await;
        // Write through so a restart can still answer this card's Expand press
        // instead of "too old to expand" (H1). Best effort: the render already
        // reached the channel, so a store failure is logged and dropped.
        self.persist_group(message_id, &group).await;
        // Lockstep with the in-memory eviction: the table must never track a
        // card the map has deliberately forgotten.
        for id in evicted {
            self.forget_card(id).await;
        }
        group
    }

    /// The in-process half of [`Self::upsert_tool_group`]: insert or update the
    /// group in the retained map, and report the ids the cap took. Split out so
    /// the durable write happens with the map lock already released.
    async fn upsert_group_in_memory(
        &self,
        message_id: u64,
        mut group: GroupState,
    ) -> (GroupState, Vec<u64>) {
        // Ids the cap takes, so their durable rows go with them.
        let mut evicted: Vec<u64> = Vec::new();
        let mut guard = self.tool_groups.lock().await;
        let (order, map) = &mut *guard;
        match map.get(&message_id) {
            Some(existing) => {
                group.expanded = existing.expanded;
                group.notes = existing.notes.clone();
                group.started_at = existing.started_at;
                group.settled = existing.settled.clone();
                group.live_ctx = existing.live_ctx.clone();
            }
            None => {
                order.push(message_id);
                // Evict SETTLED groups only (#1880 ticker parity). A live group
                // is never evicted: dropping one strands its card, because the
                // flow ticker's next snapshot returns None, the ticker leaves
                // without settling, and the card keeps rendering the live `🕒`
                // chrome with a frozen clock, which on screen is
                // indistinguishable from a turn still working. When every
                // retained group is still live the map is allowed to run over
                // the cap instead: that overflow is bounded by how many turns
                // are in flight at once, never by time, and the next settle
                // gives eviction something to take.
                while order.len() > Self::TOOL_GROUP_CAP {
                    let Some(victim) = order
                        .iter()
                        .position(|id| map.get(id).is_some_and(|g| g.settled.is_some()))
                    else {
                        break;
                    };
                    let oldest = order.remove(victim);
                    map.remove(&oldest);
                    evicted.push(oldest);
                }
            }
        }
        map.insert(message_id, group.clone());
        (group, evicted)
    }

    /// Flip a group's expanded state; None when the card is neither retained nor
    /// durable (aged out of both, or no store wired).
    pub(crate) async fn toggle_tool_group(&self, message_id: u64) -> Option<GroupState> {
        // Fast path: the card is still in the retained map.
        let hit = {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            map.get_mut(&message_id).map(|group| {
                group.expanded = !group.expanded;
                group.clone()
            })
        };
        let flipped = match hit {
            Some(group) => group,
            None => {
                // Miss: the card aged out of retention, or this process just
                // started and the map is empty. Rebuild it from the durable
                // store, or the press answers "too old to expand" on a card
                // that is still sitting in the channel (H1).
                let mut group =
                    GroupState::from_snapshot(self.load_group_snapshot(message_id).await?)?;
                group.expanded = !group.expanded;
                // Re-admit it so the toggle that follows this press, and the
                // flow ticker, find it in memory. Pushed last, so it is the
                // newest entry and the cap takes it last; a re-admitted card is
                // always settled, so the next insert of a new card reclaims the
                // slot.
                let mut guard = self.tool_groups.lock().await;
                let (order, map) = &mut *guard;
                if !map.contains_key(&message_id) {
                    order.push(message_id);
                }
                map.insert(message_id, group.clone());
                group
            }
        };
        // A press is the user's own action, so it is written through: the next
        // restart has to read back the choice this press just made.
        self.persist_group(message_id, &flipped).await;
        Some(flipped)
    }

    /// Append one narration line to the stored group, keeping only the
    /// newest [`NOTE_CAP`]. Returns the updated state, or None when the
    /// message has no stored group (aged out of retention).
    pub(crate) async fn append_note(&self, message_id: u64, note: String) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        group.last_activity_at = Instant::now();
        group.notes.push(note);
        if group.notes.len() > NOTE_CAP {
            group.notes.remove(0);
        }
        Some(group.clone())
    }

    /// Store the live ctx budget on the card while the turn runs
    /// (#1841 parity): the settled chrome already carries `ctx`, but
    /// only at settle. Discord streams the same string from
    /// `ProgressEvent::TokenCount` mid-turn, so the live line shows the
    /// budget as it grows. Returns the updated state, or None when the
    /// message has no stored group (aged out of retention).
    pub(crate) async fn set_live_ctx(&self, message_id: u64, ctx: String) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        group.live_ctx = Some(ctx);
        Some(group.clone())
    }

    /// Stamp the post-delivery status (#1841, outcome-honest FR-005): freeze
    /// the clock at now, record how the turn ENDED, and keep the ctx budget
    /// line for the settled chrome. A `None` ctx keeps whatever a previous
    /// settle stamped, so a re-settle never clears the budget. `bg` and
    /// `agents` are the alive background-task / sub-agent counts at settle
    /// (#1144/#1183): a Finished turn with either still alive settles to the
    /// `⏳ Waiting for …` pair instead of `✅ Finished`. Returns the updated
    /// state, or None when the message has no stored group (aged out of
    /// retention).
    pub(crate) async fn settle_tool_group(
        &self,
        message_id: u64,
        outcome: TurnOutcome,
        bg: usize,
        agents: crate::channels::background_work::SubagentCounts,
        ctx: Option<String>,
    ) -> Option<GroupState> {
        let settled = {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            let group = map.get_mut(&message_id)?;
            stamp_settled(group, outcome, bg, agents, ctx);
            group.clone()
        };
        // The settled line is the card's last word, so it is the one that most
        // needs to survive a restart: written through with the map lock already
        // released.
        self.persist_group(message_id, &settled).await;
        Some(settled)
    }

    /// Alive background-task and sub-agent counts for a session at settle
    /// (#1144/#1183): the single read the settle sites share so Discord
    /// cannot drift from Telegram's `bg_indicator_for` + `subagent_counts_for`.
    /// Both registries are optional (no manager wired) and degrade to zero.
    pub(crate) fn waiting_counts(
        agent: &crate::brain::agent::AgentService,
        session_id: uuid::Uuid,
    ) -> (usize, crate::channels::background_work::SubagentCounts) {
        let bg = agent
            .background_manager()
            .map(|bm| bm.running_tasks(session_id).len())
            .unwrap_or(0);
        let agents = crate::channels::telegram::delivery::subagent_counts_for(agent, session_id);
        (bg, agents)
    }

    /// Clone the live or settled group for out-of-loop renderers (#1843):
    /// the flow ticker snapshots under the lock, renders outside it, and
    /// re-snapshots after its edit so the settled line keeps the last word.
    pub(crate) async fn tool_group_snapshot(&self, message_id: u64) -> Option<GroupState> {
        {
            let guard = self.tool_groups.lock().await;
            let (_, map) = &*guard;
            if let Some(hit) = map.get(&message_id) {
                return Some(hit.clone());
            }
        }
        // Miss: the group aged out of retention, or this process just started
        // with an empty map. Try the durable store before answering None, so a
        // card that outlived a restart renders its stored state instead of being
        // read as an orphan (H1). Deliberately a read: re-admitting here would
        // let the flow ticker resurrect a card the cap just dropped, while
        // `toggle_tool_group` re-admits the one card a user actually pressed.
        let snap = self.load_group_snapshot(message_id).await?;
        GroupState::from_snapshot(snap)
    }

    /// Write a card's snapshot through to the durable store (H1). Best effort:
    /// the render has already reached the channel, so a store failure is logged
    /// and dropped rather than failing the caller.
    async fn persist_group(&self, message_id: u64, group: &GroupState) {
        let Ok(json) = serde_json::to_string(&group.to_snapshot()) else {
            tracing::warn!("Discord: could not serialise tool-group card {message_id}");
            return;
        };
        self.persist_card(message_id, TOOL_GROUP_KIND, json).await;
    }

    /// Read a card's durable snapshot back (H1). `None` when no store is wired,
    /// when the card is not tracked, or when the stored form is unreadable: all
    /// three answer the caller the same way.
    async fn load_group_snapshot(&self, message_id: u64) -> Option<GroupSnapshot> {
        let json = self.load_card(message_id, TOOL_GROUP_KIND).await?;
        match serde_json::from_str(&json) {
            Ok(snap) => Some(snap),
            Err(e) => {
                tracing::warn!("Discord: unreadable tool-group card {message_id}: {e}");
                None
            }
        }
    }

    /// Remove the LAST narration line matching `pred` — the final-response
    /// dedup drops the trailing note that mirrors the answer, so the trace
    /// does not double-post it as a clip. Returns the updated state, or None
    /// when nothing matched or no group is stored.
    pub(crate) async fn drop_note_if<F>(&self, message_id: u64, pred: F) -> Option<GroupState>
    where
        F: Fn(&str) -> bool,
    {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(&message_id)?;
        let idx = group.notes.iter().rposition(|n| pred(n))?;
        group.notes.remove(idx);
        Some(group.clone())
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn probe(entries: usize, notes: usize, expanded: bool) -> GroupState {
        GroupState {
            entries: (0..entries)
                .map(|i| GroupEntry {
                    name: format!("tool{i}"),
                    context: format!(" (long context line to reach the cap sooner #{i})"),
                    status: Some(true),
                })
                .collect(),
            notes: (0..notes)
                .map(|i| clip_note(&"n".repeat(NOTE_MAX_CHARS + 40 + i)))
                .collect(),
            expanded,
            started_at: Instant::now(),
            last_activity_at: Instant::now(),
            live_ctx: None,
            settled: Some(SettledStatus::new(
                TurnOutcome::Finished,
                Duration::from_secs(3),
                None,
            )),
        }
    }

    #[test]
    fn huge_expansion_fits_the_cap_and_states_drops() {
        let rendered = render_content(&probe(300, 6, true));
        let chars = rendered.chars().count();
        assert!(chars <= CONTENT_MAX_CHARS, "expansion overshot: {chars}");
        assert!(
            rendered.contains("_") && rendered.contains("omitted to fit"),
            "silent drop: {rendered}"
        );
        assert!(
            rendered.contains("tool299"),
            "the newest entry must survive the clamp"
        );
    }

    #[test]
    fn small_expansion_is_untouched() {
        let rendered = render_content(&probe(3, 0, true));
        assert!(rendered.contains("tool0") && rendered.contains("tool2"));
        assert!(!rendered.contains("omitted to fit"));
    }

    #[test]
    fn collapsed_note_flood_fits_the_cap() {
        let rendered = render_content(&probe(30, 6, false));
        assert!(rendered.chars().count() <= CONTENT_MAX_CHARS);
    }

    #[test]
    fn multibyte_context_never_splits_a_char() {
        let mut g = probe(2, 0, true);
        g.entries[0].context = " (".repeat(3000);
        let rendered = render_content(&g);
        assert!(rendered.chars().count() <= CONTENT_MAX_CHARS);
        assert!(!rendered.ends_with('\u{FFFD}'));
    }

    #[test]
    fn marker_counts_exactly_the_dropped_rows() {
        let rendered = render_content(&probe(250, 0, true));
        // Line 0 is the summary (`✅ **250 tool calls** · …`), which also
        // starts with an entry icon — only the entry rows count as shown.
        let shown = rendered
            .lines()
            .skip(1)
            .filter(|l| l.starts_with(['✅', '❌', '\u{2699}']))
            .count();
        assert!(shown > 0 && shown < 250, "clamp kept {shown} rows");
        assert!(
            rendered.contains(&format!("_{} omitted to fit", 250 - shown)),
            "marker disagrees with visible rows:\n{rendered}"
        );
    }
}
