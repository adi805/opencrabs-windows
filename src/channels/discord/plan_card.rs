//! Discord plan card (FR-008, #1880): ONE message per session showing the
//! active plan's checklist, edited in place, carrying Approve/Discard buttons.
//!
//! Mirrors the Telegram card (`telegram/plan_card.rs`) in BEHAVIOUR, not in
//! code: same plan source (`utils::plan_files`), same approval entry point
//! (`utils::plan_mode::try_approve`), same durable store
//! (`db::repository::PlanCardRepository`). No third state mechanism — the
//! whole point of AC-019 is that a Discord card and a Telegram card for the
//! same session read and write the SAME rows.
//!
//! Why this exists: the plan board is the single most-visible surface in
//! Telegram and Discord had nothing at all (`grep -rln plan
//! src/channels/discord/` was empty before this module). The owner's original
//! complaint was exactly this gap.

use serenity::builder::{CreateActionRow, CreateButton};
use serenity::model::application::ButtonStyle;

use crate::tui::plan::{PlanDocument, PlanStatus, status_mark};

/// Callback data. The `plan:` prefix is deliberate and mirrors Telegram: it
/// can never collide with the tool-approval `approve:` family, so a plan
/// button is never mistaken for a permission prompt.
pub(crate) const PLAN_APPROVE: &str = "plan:ok";
pub(crate) const PLAN_DISCARD: &str = "plan:no";

/// Which buttons the card carries for the plan's current state.
///
/// Mirrors `telegram::flow_chrome::PlanKb` variant-for-variant so the two
/// surfaces cannot drift into offering different actions for the same state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanKb {
    /// Editing: the plan is waiting on the user, so Approve + Discard.
    ApproveDiscard,
    /// Active: the checklist is executing, so only Discard applies.
    DiscardOnly,
}

impl PlanKb {
    /// The keyboard a loaded plan warrants.
    ///
    /// Keyed on `status` only. `pending_approval` is the authoritative
    /// "waiting on the user" marker on the JSON sidecar, and `PlanStatus`
    /// is derived from it, so reading the status here keeps this a pure
    /// function of the loaded document — no second source of truth.
    pub(crate) fn of(plan: &PlanDocument) -> Self {
        match plan.status {
            PlanStatus::Editing => PlanKb::ApproveDiscard,
            PlanStatus::Active => PlanKb::DiscardOnly,
        }
    }
}

/// Discord markdown body for a plan: title, checklist rows, acceptance
/// criteria count.
///
/// Returns an empty string for a plan with nothing renderable, so callers
/// can treat empty as "no card" without a second predicate (mirrors the
/// Telegram renderer, which returns `Option`).
///
/// Discord has no collapsible markup and a 2000-char message cap, so the
/// Telegram card's expandable prose sections have no equivalent here. Prose
/// is deliberately omitted rather than inlined: a design plan's body can run
/// to thousands of characters, and truncating it mid-sentence reads worse
/// than the checklist alone. The `.md` stays available via the plan file.
pub(crate) fn render_plan_card(plan: &PlanDocument) -> String {
    let mut out = String::new();

    let title = plan.title.trim();
    if !title.is_empty() {
        out.push_str("📋 **");
        out.push_str(title);
        out.push_str("**\n");
    }

    for task in &plan.tasks {
        let mark = status_mark(&task.status);
        out.push_str(&format!("{mark} **{}. {}**", task.order, task.title));
        if !task.acceptance_criteria.is_empty() {
            out.push_str(&format!(
                " _({} acceptance criteria)_",
                task.acceptance_criteria.len()
            ));
        }
        out.push('\n');
    }

    out.trim_end().to_string()
}

/// Buttons for this state. Both plan states warrant an action, so unlike
/// Telegram's `PlanKb` there is no buttonless variant: a rendered card
/// always carries at least Discard.
pub(crate) fn render_components(kb: PlanKb) -> Vec<CreateActionRow> {
    match kb {
        PlanKb::ApproveDiscard => vec![CreateActionRow::Buttons(vec![
            CreateButton::new(PLAN_APPROVE)
                .label("✅ Approve plan")
                .style(ButtonStyle::Success),
            CreateButton::new(PLAN_DISCARD)
                .label("🗑 Discard")
                .style(ButtonStyle::Danger),
        ])],
        PlanKb::DiscardOnly => vec![CreateActionRow::Buttons(vec![
            CreateButton::new(PLAN_DISCARD)
                .label("🗑 Discard plan")
                .style(ButtonStyle::Danger),
        ])],
    }
}

/// Whether a custom_id belongs to the plan card.
pub(crate) fn is_plan_callback(custom_id: &str) -> bool {
    custom_id == PLAN_APPROVE || custom_id == PLAN_DISCARD
}

// ── card lifecycle ───────────────────────────────────────────────────────

use serenity::all::{ChannelId, CreateMessage, EditMessage, Http, MessageId};
use uuid::Uuid;

use super::DiscordState;
use super::writes::{self, Class};

/// Reconcile the channel's plan card with the session's live plan.
///
/// Called after a turn and on resume. Three outcomes, in order:
///
/// * no plan (or nothing renderable) → the tracked card is deleted;
/// * a tracked card whose signature is unchanged → no API call at all;
/// * otherwise → edit in place, or post a fresh card when none is tracked.
///
/// Serialised per session (#822): the map read, the edit-or-post decision
/// and the id write all happen under one lock, or two concurrent refreshes
/// both see no card and both post one.
///
/// Writes ride the governor as `Final` — a plan card is the surface the user
/// acts on, so it is never dropped under load.
pub(crate) async fn refresh_plan_card(
    http: &Http,
    channel: ChannelId,
    state: &DiscordState,
    session_id: Uuid,
) {
    let lock = state.plan_card_lock(session_id).await;
    let _guard = lock.lock().await;

    let Some(plan) = crate::utils::plan_files::load_plan(session_id).await else {
        remove_plan_card_locked(http, channel, state, session_id).await;
        return;
    };

    let body = render_plan_card(&plan);
    if body.is_empty() {
        remove_plan_card_locked(http, channel, state, session_id).await;
        return;
    }

    let kb = PlanKb::of(&plan);
    let signature = format!("{body}\u{1}{kb:?}");

    if let Some((cid, mid, last_sig)) = state.plan_card(session_id).await {
        // Same channel and same rendering: nothing to say. Skipping here is
        // what keeps a long turn from editing the card on every iteration.
        if cid == channel.get() && last_sig == signature {
            return;
        }
        let edit = EditMessage::new()
            .content(&body)
            .components(render_components(kb));
        match writes::edit(http, channel, MessageId::new(mid), edit, Class::Final).await {
            // Edited, or queued by the governor (latest-wins). Either way the
            // signature now describes what the chat will show, so an identical
            // refresh stops re-queueing.
            Ok(Some(_)) | Ok(None) => {
                state
                    .set_plan_card(session_id, channel.get(), mid, signature)
                    .await;
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "Discord: plan card edit failed (message {mid}), posting a fresh card: {e}"
                );
                // Fall through: a dead card must not strand the plan.
            }
        }
    }

    let builder = CreateMessage::new()
        .content(&body)
        .components(render_components(kb));
    match writes::send(http, channel, builder, Class::Final).await {
        Ok(Some(msg)) => {
            state
                .set_plan_card(session_id, channel.get(), msg.id.get(), signature)
                .await;
        }
        Ok(None) => {
            tracing::debug!("Discord: plan card post queued by the governor");
        }
        Err(e) => {
            tracing::warn!("Discord: plan card post failed: {e}");
        }
    }
}

/// Delete the tracked card for a session and forget it.
///
/// Takes the lock itself; callers already holding it use
/// [`remove_plan_card_locked`].
pub(crate) async fn remove_plan_card(
    http: &Http,
    channel: ChannelId,
    state: &DiscordState,
    session_id: Uuid,
) {
    let lock = state.plan_card_lock(session_id).await;
    let _guard = lock.lock().await;
    remove_plan_card_locked(http, channel, state, session_id).await;
}

/// [`remove_plan_card`] without taking the lock: the caller already holds it.
async fn remove_plan_card_locked(
    http: &Http,
    channel: ChannelId,
    state: &DiscordState,
    session_id: Uuid,
) {
    let Some((cid, mid, _)) = state.plan_card(session_id).await else {
        return;
    };
    // A card tracked in a different channel belongs to a surface we are not
    // rendering into; deleting it by id here would hit the wrong message.
    if cid != channel.get() {
        state.clear_plan_card(session_id).await;
        return;
    }
    if let Err(e) = http
        .delete_message(channel, MessageId::new(mid), None)
        .await
    {
        tracing::debug!("Discord: plan card delete failed (already gone?): {e}");
    }
    state.clear_plan_card(session_id).await;
}
