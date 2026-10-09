//! Agent-driven interactive components: select menus (#382) and modal
//! forms (#383), with lazy TTL expiry (#386).
//!
//! `discord_send` posts a select menu or a form button; the pick or the
//! submitted fields come back here, get routed into the channel's session
//! as an agent turn (with only a compact "[System: ...]" tag persisted),
//! and the reply is delivered to the channel. Pending component state
//! lives in [`super::DiscordState`] with creation timestamps; clicks past
//! the TTL answer "expired" instead of firing stale actions.

use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::services::SessionService;
use serenity::model::application::CommandInteraction;
use serenity::prelude::Context;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::component_spec::FormField;
use super::writes::{self, Class};

/// One pending modal form: what the modal shows when the button is hit.
#[derive(Debug, Clone)]
pub(crate) struct FormSpec {
    pub title: String,
    /// One entry per modal input, max 5 (Discord's modal cap).
    pub fields: Vec<FormField>,
}

/// Resolve (or create) the session for interaction input, mirroring
/// handle_message's keying: DMs by user, channels/threads by channel id.
pub(crate) async fn resolve_interaction_session(
    session_svc: &SessionService,
    is_dm: bool,
    user_id: u64,
    channel_id: u64,
    idle_hours: Option<f64>,
) -> Option<Uuid> {
    use crate::channels::session_resolve;
    let (id_str, legacy_title) = if is_dm {
        (
            format!("discord-dm-{user_id}"),
            format!("Discord: DM {user_id}"),
        )
    } else {
        (
            format!("discord-{channel_id}"),
            format!("Discord: #{channel_id}"),
        )
    };
    let suffix = session_resolve::chat_id_suffix(&id_str);
    let session_title = format!("{legacy_title} {suffix}");
    match session_resolve::resolve_or_create_channel_session(
        session_svc,
        &suffix,
        &legacy_title,
        &session_title,
        idle_hours,
        "Discord",
    )
    .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::error!("Discord interaction: failed to resolve session: {e}");
            None
        }
    }
}

/// Run an interaction-originated agent turn and deliver the reply to the
/// channel. `context_text` goes to the model for this turn only; history
/// persists `display_tag` (the system-note contract shared with reactions).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn route_interaction_turn(
    ctx: &Context,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    is_dm: bool,
    user_id: u64,
    channel_id: u64,
    idle_hours: Option<f64>,
    context_text: String,
    display_tag: String,
) {
    let Some(session_id) =
        resolve_interaction_session(&session_svc, is_dm, user_id, channel_id, idle_hours).await
    else {
        return;
    };
    let response = match agent
        .send_message_with_display(session_id, context_text, Some(display_tag), None)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Discord interaction: agent error for session {session_id}: {e}");
            return;
        }
    };
    let (text_only, _imgs) = crate::utils::extract_img_markers(&response.content);
    let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
    let (text_only, _react) = crate::utils::extract_react_marker(&text_only);
    let trimmed = text_only.trim();
    if trimmed.is_empty() {
        return;
    }
    let channel = serenity::model::id::ChannelId::new(channel_id);
    for chunk in super::handler::split_message(trimmed, 2000) {
        if let Err(e) = writes::say(&ctx.http, channel, chunk, Class::Final).await {
            tracing::warn!("Discord interaction: failed to deliver reply: {e}");
        }
    }
}

/// The gate, the acknowledgement and the dispatch shared by every interaction
/// that asks the agent to do work: a catalog command picked from the `/` menu
/// (#1850) and the two right-click context menus (FR-006 / AC-009).
///
/// ONE copy for every interaction entry point, deliberately. The rule is the
/// OC-02 deny-by-default gate, and the message path (`handler.rs`) already
/// carries a copy of it inline; a third one, written for context menus, is the
/// copy that gets forgotten the next time the rule moves. The caller's job is
/// only to say what text the request is; where the request came from is not
/// something the gate should have to know.
///
/// `invocation` is the request as text, exactly as if the user had typed it.
/// A catalog command rebuilds `/<name> <args>`; a context menu renders its
/// target. Either way the agent sees one shape and there is one implementation
/// of every command.
///
/// The ack is deferred and non-ephemeral on purpose: `route_followup_turn` edits
/// that very message in place with the turn's answer (FR-002 / AC-004), and an
/// ephemeral defer can only ever be edited into another ephemeral message.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_invoked_request(
    ctx: &Context,
    command: &CommandInteraction,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    discord_state: Arc<super::DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    invocation: String,
) {
    let user = command.user.id.get();
    let user_name = command.user.name.clone();
    let is_dm = command.guild_id.is_none();
    let channel_id = command.channel_id.get();

    // OC-02: Discord shows the command list to every member of the guild, so an
    // interaction is an entry point like any other message and gets the same
    // deny-by-default gate `handle_message` applies. Roles count in guilds only,
    // mirroring that path. A context menu carries a member too, so this is the
    // same check for both.
    let cfg = config_rx.borrow().clone();
    let dc = &cfg.channels.discord;
    let role_ids: Vec<u64> = if is_dm {
        Vec::new()
    } else {
        command
            .member
            .as_ref()
            .map(|m| m.roles.iter().map(|r| r.get()).collect())
            .unwrap_or_default()
    };
    let owner = crate::config::owner::is_owner(&dc.allowed_users, &dc.bot_owner, &user.to_string());
    let in_allowlist = dc
        .allowed_users
        .iter()
        .filter_map(|s| s.parse::<i64>().ok())
        .any(|u| u == user as i64);
    let admitted = super::commands::identity_admitted(
        dc.allowed_users.is_empty() && dc.allowed_roles.is_empty() && dc.bot_owner.is_empty(),
        owner,
        in_allowlist,
        !is_dm && super::commands::holds_allowed_role(&dc.allowed_roles, &role_ids),
    );

    // Channel scope, with the parent fallback: a thread or forum post carries
    // its own id, so allow-listing a forum admits its posts.
    let channel_str = channel_id.to_string();
    let mut channel_ok =
        dc.allowed_channels.is_empty() || dc.allowed_channels.iter().any(|c| c == &channel_str);
    if !channel_ok && !is_dm {
        channel_ok = match command.channel_id.to_channel(&ctx.http).await {
            Ok(serenity::model::channel::Channel::Guild(gc)) => gc.parent_id.is_some_and(|p| {
                dc.allowed_channels
                    .iter()
                    .any(|c| c == &p.get().to_string())
            }),
            _ => false,
        };
    }
    // `respond_to` filters unsolicited messages; an invoked command is
    // solicited by definition, so only `dm_only` applies, and it means the
    // operator told this bot not to speak in guild channels.
    let inside_dm_policy = is_dm || !matches!(dc.respond_to, crate::config::RespondTo::DmOnly);

    if !admitted || !channel_ok || !inside_dm_policy {
        tracing::warn!(
            "Discord: refused {:?} from user {} (allowed={}, channel={}, dm_only={})",
            command.data.name,
            user,
            admitted,
            channel_ok,
            inside_dm_policy
        );
        let _ = command
            .create_response(
                &ctx.http,
                serenity::builder::CreateInteractionResponse::Message(
                    serenity::builder::CreateInteractionResponseMessage::new()
                        .content("This bot is not enabled for you or this channel.")
                        .ephemeral(true),
                ),
            )
            .await;
        return;
    }

    let idle = dc.session_idle_hours;
    // History keeps the invocation the way a typed message would:
    // `Sender: /cmd args` in a guild, bare in the owner's DM, the same rule
    // `handler.rs` uses. `context_text` is the invocation itself, which is what
    // the model sees when you type it.
    let history_line = if owner && is_dm {
        invocation.clone()
    } else {
        format!("{user_name}: {invocation}")
    };
    // A slash command has no originating message, so `Acknowledge` (Discord's
    // DEFERRED_UPDATE_MESSAGE, kind 6) is not a valid reply to it: there is
    // nothing to update, the handshake fails, and the user gets the red "This
    // interaction didn't respond" banner while the turn quietly carries on
    // (#27, upstream #1888). `Defer` (kind 5) opens Discord's native loading
    // state inside the 3-second window AND keeps the interaction alive, so the
    // turn's answer can replace this very message instead of landing as an
    // orphaned second reply (FR-002, AC-004).
    //
    // The ack result is BOUND, never discarded: a refused acknowledgement used
    // to vanish into a discard binding, which hid exactly the failure this
    // branch exists to fix.
    let ack = command
        .create_response(
            &ctx.http,
            serenity::builder::CreateInteractionResponse::Defer(
                serenity::builder::CreateInteractionResponseMessage::new(),
            ),
        )
        .await;
    if let Err(e) = &ack {
        tracing::warn!(
            "Discord: deferred ack for {:?} refused: {e}",
            command.data.name
        );
    }
    // FR-002: hand the turn the token so its answer can replace the deferred
    // message instead of arriving as a second reply.
    let interaction_token = Some(command.token.clone());
    let ctx2 = ctx.clone();
    tokio::spawn(async move {
        route_followup_turn(
            &ctx2,
            agent,
            session_svc,
            discord_state,
            interaction_token,
            is_dm,
            user,
            channel_id,
            idle,
            invocation,
            history_line,
        )
        .await;
    });
}

/// Run a tapped follow-up suggestion as an agent turn through the SAME
/// tool-loop display path typed messages use (#1852, the Discord twin of
/// #1847). The tap used to ride the bare `send_message_with_display`
/// single-completion path: zero tools, zero progress events, zero approvals,
/// so the channel sat silent from the `▶️` echo until a plain reply landed.
///
/// Here the flow-group shell and its 🕒 ticker are born BEFORE dispatch, tool
/// events edit the bubble in place, intermediate text dedups against the
/// final answer, approvals route to the Discord buttons, and chained
/// `SuggestedOptions` render again. History persists `display_tag` (the
/// tapper's name) exactly like the bare path did.
///
/// The other [`route_interaction_turn`] callers (modal form, select menu)
/// deliberately stay on the bare single-call contract: they are synthetic
/// steering prompts, not user-intent turns.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn route_followup_turn(
    ctx: &Context,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    discord_state: Arc<super::DiscordState>,
    // Interaction token for the deferred ack, when this turn was started by a
    // slash command. `Some` lets the final answer REPLACE the deferred message
    // instead of arriving as an orphaned second reply (FR-002, AC-004); `None`
    // keeps the legacy channel-say delivery.
    interaction_token: Option<String>,
    is_dm: bool,
    user_id: u64,
    channel_id: u64,
    idle_hours: Option<f64>,
    context_text: String,
    display_tag: String,
) {
    let Some(session_id) =
        resolve_interaction_session(&session_svc, is_dm, user_id, channel_id, idle_hours).await
    else {
        return;
    };
    let channel = serenity::model::id::ChannelId::new(channel_id);
    let http = ctx.http.clone();

    // Approvals and chained suggestions resolve their delivery target through
    // the session→channel registration; mirror handle_message and register
    // before dispatch.
    discord_state
        .register_session_channel(session_id, channel_id)
        .await;

    let cancel_token = tokio_util::sync::CancellationToken::new();
    discord_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    let trace_narration = crate::config::Config::current()
        .channels
        .discord
        .trace_narration;

    // Per-turn dedup state (the #456/#459/#943/#951 class, ported from
    // handle_message): tool_loop emits the last iteration's text BOTH as
    // IntermediateText AND as response.content — posting both duplicates
    // the answer.
    type SentIntermediate = String;
    let sent_intermediates: Arc<Mutex<Vec<SentIntermediate>>> = Arc::new(Mutex::new(Vec::new()));
    let intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let turn_group_mid: Arc<Mutex<Option<serenity::model::id::MessageId>>> =
        Arc::new(Mutex::new(None));

    // Progress callback: handle_message's tool-loop display arms, scoped to
    // the tap turn.
    let progress_cb: crate::brain::agent::ProgressCallback = {
        use crate::brain::agent::ProgressEvent;
        use serenity::builder::EditMessage;

        use super::tool_group::{GroupEntry, GroupState};

        let tools: Arc<Mutex<Vec<GroupEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let group_msg_id = turn_group_mid.clone();
        let group_state_cb = discord_state.clone();
        let http = http.clone();
        let sent = sent_intermediates.clone();
        let handles_cb = intermediate_handles.clone();

        Arc::new(move |session_id, event| {
            let tools = tools.clone();
            let http = http.clone();

            match event {
                ProgressEvent::ToolStarted {
                    tool_name,
                    tool_input,
                } => {
                    let ctx_hint = crate::utils::tool_context_hint(&tool_name, &tool_input);
                    let gmid = group_msg_id.clone();
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let entries = {
                            let mut t = tools.lock().await;
                            t.push(GroupEntry {
                                name: tool_name,
                                context: ctx_hint,
                                status: None,
                            });
                            t.clone()
                        };
                        let mut mid_guard = gmid.lock().await;
                        match *mid_guard {
                            Some(mid) => {
                                let group = dstate
                                    .upsert_tool_group(
                                        mid.get(),
                                        GroupState {
                                            last_activity_at: std::time::Instant::now(),
                                            entries,
                                            notes: Vec::new(),
                                            expanded: false,
                                            started_at: std::time::Instant::now(),
                                            settled: None,
                                        },
                                    )
                                    .await;
                                let edit = EditMessage::new()
                                    .content(super::tool_group::render_content(&group))
                                    .components(super::tool_group::render_components(
                                        &group,
                                        mid.get(),
                                    ));
                                if let Err(e) =
                                    writes::edit(&http, channel, mid, edit, Class::Edit).await
                                {
                                    tracing::warn!(
                                        "Discord: follow-up tap tool group edit failed (append): {e}"
                                    );
                                }
                            }
                            None => {
                                let group = GroupState {
                                    last_activity_at: std::time::Instant::now(),
                                    entries,
                                    notes: Vec::new(),
                                    expanded: false,
                                    started_at: std::time::Instant::now(),
                                    settled: None,
                                };
                                let content = super::tool_group::render_content(&group);
                                match writes::say(&http, channel, &content, Class::Final).await {
                                    Ok(Some(sent_msg)) => {
                                        let comps = super::tool_group::render_components(
                                            &group,
                                            sent_msg.id.get(),
                                        );
                                        if !comps.is_empty()
                                            && let Err(e) = writes::edit(
                                                &http,
                                                channel,
                                                sent_msg.id,
                                                EditMessage::new().components(comps),
                                                Class::Final,
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                "Discord: follow-up tap tool group component fixup failed: {e}"
                                            );
                                        }
                                        dstate.upsert_tool_group(sent_msg.id.get(), group).await;
                                        *mid_guard = Some(sent_msg.id);
                                    }
                                    Ok(None) => tracing::error!(
                                        "Discord: follow-up tap tool group post refused despite Final class"
                                    ),
                                    Err(e) => tracing::warn!(
                                        "Discord: follow-up tap failed to post tool group message: {e}"
                                    ),
                                }
                            }
                        }
                    });
                }
                ProgressEvent::ToolCompleted {
                    tool_name, success, ..
                } => {
                    let gmid = group_msg_id.clone();
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let entries = {
                            let mut t = tools.lock().await;
                            if let Some(entry) = t
                                .iter_mut()
                                .rev()
                                .find(|e| e.name == tool_name && e.status.is_none())
                            {
                                entry.status = Some(success);
                            }
                            t.clone()
                        };
                        if let Some(mid) = *gmid.lock().await {
                            let group = dstate
                                .upsert_tool_group(
                                    mid.get(),
                                    GroupState {
                                        last_activity_at: std::time::Instant::now(),
                                        entries,
                                        notes: Vec::new(),
                                        expanded: false,
                                        started_at: std::time::Instant::now(),
                                        settled: None,
                                    },
                                )
                                .await;
                            let edit = EditMessage::new()
                                .content(super::tool_group::render_content(&group))
                                .components(super::tool_group::render_components(
                                    &group,
                                    mid.get(),
                                ));
                            if let Err(e) =
                                writes::edit(&http, channel, mid, edit, Class::Edit).await
                            {
                                tracing::warn!(
                                    "Discord: follow-up tap tool group edit failed (status): {e}"
                                );
                            }
                        }
                    });
                }
                ProgressEvent::SelfHealingAlert { message } => {
                    tokio::spawn(async move {
                        let text =
                            format!("🔧 {}", crate::utils::sanitize::normalize_dashes(&message));
                        if let Err(e) = writes::say(&http, channel, &text, Class::Final).await {
                            tracing::warn!(error = %e, "Discord: follow-up tap self-heal post failed");
                        }
                    });
                }
                ProgressEvent::IntermediateText { text, .. } => {
                    // Same sanitation the typed path applies, so the dedup
                    // keys below normalize identically for intermediate and
                    // final copies.
                    let clean = crate::utils::sanitize::strip_llm_artifacts(&text);
                    let clean = crate::utils::sanitize::redact_secrets(&clean);
                    let (clean, _) = crate::utils::extract_img_markers(&clean);
                    let (clean, _) = crate::utils::extract_vid_markers(&clean);
                    let clean = super::table_convert::tables_to_discord(&clean);
                    if clean.trim().is_empty() {
                        return;
                    }
                    if trace_narration {
                        let gmid = group_msg_id.clone();
                        let dstate = group_state_cb.clone();
                        let http = http.clone();
                        let handles = handles_cb.clone();
                        let note = super::tool_group::clip_note(&clean);
                        let handle = tokio::spawn(async move {
                            let Some(mid) = *gmid.lock().await else {
                                return;
                            };
                            let Some(group) = dstate.append_note(mid.get(), note).await else {
                                return;
                            };
                            let edit = EditMessage::new()
                                .content(super::tool_group::render_content(&group))
                                .components(super::tool_group::render_components(
                                    &group,
                                    mid.get(),
                                ));
                            if let Err(e) =
                                writes::edit(&http, channel, mid, edit, Class::Edit).await
                            {
                                tracing::debug!(
                                    "Discord: follow-up tap trace note edit failed: {e}"
                                );
                            }
                        });
                        if let Ok(mut g) = handles.lock() {
                            g.push(handle);
                        }
                        return;
                    }
                    let sent = sent.clone();
                    let handles = handles_cb.clone();
                    let http = http.clone();
                    let handle = tokio::spawn(async move {
                        {
                            let mut prev = sent.lock().await;
                            if prev.iter().any(|b| b == &clean) {
                                return;
                            }
                            prev.push(clean.clone());
                        }
                        for chunk in super::handler::split_message(&clean, 2000) {
                            if let Err(e) = writes::say(&http, channel, &chunk, Class::Final).await
                            {
                                tracing::debug!(
                                    "Discord: follow-up tap intermediate send failed: {e}"
                                )
                            }
                        }
                    });
                    if let Ok(mut g) = handles.lock() {
                        g.push(handle);
                    }
                }
                ProgressEvent::RetryAttempt {
                    attempt,
                    max,
                    reason,
                } => {
                    let http = http.clone();
                    tokio::spawn(async move {
                        let text = format!("⏳ Retry {}/{} — {}", attempt, max, reason);
                        if let Err(e) = writes::say(&http, channel, &text, Class::Final).await {
                            tracing::warn!(error = %e, "Discord: follow-up tap retry post failed");
                        }
                    });
                }
                ProgressEvent::ProviderSwitched {
                    to_name, to_model, ..
                } => {
                    let http = http.clone();
                    tokio::spawn(async move {
                        let text = format!("🔄 Now using {}/{}", to_name, to_model);
                        if let Err(e) = writes::say(&http, channel, &text, Class::Final).await {
                            tracing::warn!(error = %e, "Discord: follow-up tap provider switch post failed");
                        }
                    });
                }
                ProgressEvent::SuggestedOptions(options) => {
                    let http = http.clone();
                    let state = group_state_cb.clone();
                    let raw_options: Vec<String> =
                        options.into_iter().map(|item| item.label).collect();
                    tokio::spawn(async move {
                        super::suggest_options::render_suggestions(
                            &http,
                            &state,
                            session_id,
                            raw_options,
                        )
                        .await;
                    });
                }
                _ => {}
            }
        })
    };

    // Turn-start group shell (#1845's pattern): post the bubble NOW, before
    // the turn dispatches, so a tapped suggestion has a live 🕒 clock from
    // second zero — the whole of #1852. On a post failure the mid stays
    // None and bubble creation falls back to the first tool event.
    let turn_shell = super::tool_group::GroupState {
        last_activity_at: std::time::Instant::now(),
        entries: Vec::new(),
        notes: Vec::new(),
        expanded: false,
        started_at: std::time::Instant::now(),
        settled: None,
    };
    match writes::say(
        &http,
        channel,
        &super::tool_group::render_content(&turn_shell),
        Class::Final,
    )
    .await
    {
        Ok(Some(sent_msg)) => {
            discord_state
                .upsert_tool_group(sent_msg.id.get(), turn_shell)
                .await;
            *turn_group_mid.lock().await = Some(sent_msg.id);
        }
        Ok(None) => {
            tracing::error!("Discord: follow-up tap turn-start shell refused despite Final class")
        }
        Err(e) => {
            tracing::warn!("Discord: follow-up tap turn-start shell post failed: {e}")
        }
    }

    // Flow ticker (#1843's twin): re-render the clock every 4 s so the tap
    // turn's bubble never freezes between tool events.
    super::handler::spawn_flow_ticker(
        http.clone(),
        channel,
        turn_group_mid.clone(),
        discord_state.clone(),
    );

    let approval_cb = super::handler::make_approval_callback(discord_state.clone());
    let chat_id_str = channel_id.to_string();
    let result = agent
        .send_message_with_tools_and_display(
            session_id,
            context_text,
            Some(display_tag),
            None,
            Some(cancel_token),
            Some(approval_cb),
            Some(progress_cb),
            "discord",
            Some(&chat_id_str),
            None,
        )
        .await;

    discord_state.remove_cancel_token(session_id).await;

    match result {
        Ok(response) => {
            // Await in-flight intermediate posts before the dedup read
            // (spawn-then-push race, the #459/#951 class).
            let pending = {
                let mut g = intermediate_handles.lock().expect("poisoned");
                std::mem::take(&mut *g)
            };
            for h in pending {
                if let Err(e) = h.await {
                    tracing::warn!("Discord: follow-up tap intermediate task panicked: {e}");
                }
            }
            // Same sanitation tail the typed path runs before delivery. The
            // bare tap path dropped react and media markers; dropping them
            // here too keeps the tap contract unchanged (#1852 is about the
            // live status, not attachments).
            let (response_content, _react) = crate::utils::extract_react_marker(&response.content);
            let (text_only, _imgs) = crate::utils::extract_img_markers(&response_content);
            let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
            let text_only = crate::utils::sanitize::redact_secrets(&text_only);
            let text_only = super::table_convert::tables_to_discord(&text_only);

            // Settled status chrome (#1841's twin): freeze the clock and
            // stamp the ctx budget into the bubble's settled line.
            let ctx_max = agent.context_limit_for_session(session_id);
            let ctx_line = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );

            let skip_final_post = {
                let posted = sent_intermediates.lock().await;
                if text_only.trim().is_empty() {
                    // Empty-final guard: the real answer already went out
                    // as intermediates; never post a bare shell.
                    true
                } else {
                    let final_key = super::handler::norm_key(&text_only);
                    posted
                        .iter()
                        .any(|b| super::handler::norm_key(b) == final_key)
                }
            };

            // Trace mode: drop the mirror note the tool loop folded in as
            // the trailing intermediate (the full answer posts below).
            let answer_head = text_only
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_lowercase();
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .drop_note_if(mid.get(), |n| answer_head.starts_with(&n.to_lowercase()))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = writes::edit(&http, channel, mid, edit, Class::Edit).await {
                    tracing::debug!("Discord: follow-up tap trace mirror-note drop failed: {e}");
                }
            }

            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .settle_tool_group(
                        mid.get(),
                        super::tool_group::TurnOutcome::Finished,
                        if ctx_line.is_empty() {
                            None
                        } else {
                            Some(ctx_line.clone())
                        },
                    )
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = writes::edit(&http, channel, mid, edit, Class::Edit).await {
                    tracing::debug!("Discord: follow-up tap settled stamp failed: {e}");
                }
            }

            if !skip_final_post {
                let chunks = super::handler::split_message(&text_only, 2000);
                // FR-002 (AC-004): when the turn was started by a slash
                // command, the deferred ack IS the answer's home — edit it in
                // place so the invocation and the result are one message
                // instead of two. The token is good for 15 minutes; past that
                // the edit fails and we fall back to a plain message tagged as
                // a continuation (AC-005) rather than dropping the answer.
                let mut delivered_via_token = false;
                for (idx, chunk) in chunks.iter().enumerate() {
                    if idx == 0
                        && let Some(token) = interaction_token.as_deref()
                    {
                        let edit = serenity::builder::EditInteractionResponse::new()
                            .content(chunk.clone());
                        match http
                            .edit_original_interaction_response(token, &edit, Vec::new())
                            .await
                        {
                            Ok(_) => {
                                delivered_via_token = true;
                                continue;
                            }
                            Err(e) => tracing::warn!(
                                "Discord: deferred ack edit failed (token expired?), falling back to a plain message: {e}"
                            ),
                        }
                    }
                    // A continuation marker covers both the overflow chunks of
                    // a token delivery and the first chunk of a fallback.
                    let payload =
                        if idx > 0 || (interaction_token.is_some() && !delivered_via_token) {
                            format!("\u{2026}{chunk}")
                        } else {
                            chunk.clone()
                        };
                    if let Err(e) = writes::say(&http, channel, &payload, Class::Final).await {
                        tracing::error!("Discord: follow-up tap reply delivery failed: {e}");
                    }
                }
            }
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("Discord: follow-up tap turn cancelled for session {session_id}");
            super::handler::settle_outcome(
                &http,
                channel,
                &discord_state,
                &turn_group_mid,
                super::tool_group::TurnOutcome::Cancelled,
                None,
            )
            .await;
        }
        Err(e) => {
            tracing::error!("Discord: follow-up tap agent error: {e}");
            super::handler::settle_outcome(
                &http,
                channel,
                &discord_state,
                &turn_group_mid,
                super::handler::classify_outcome(&e),
                None,
            )
            .await;
            let error_msg = format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
            if let Err(e) = writes::say(&http, channel, error_msg, Class::Final).await {
                tracing::warn!("Discord: follow-up tap error post failed: {e}");
            }
        }
    }
    // Plan board (FR-008, #1880): re-stick this session's plan card after the
    // turn so it follows the conversation instead of staying buried at the
    // position the chatter arrived after. Same tail as the message path.
    super::plan_card::restick_plan_card_after_turn(&http, channel, &discord_state, session_id).await;
}
