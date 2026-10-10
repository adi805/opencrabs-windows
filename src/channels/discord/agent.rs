//! Discord Agent
//!
//! Agent struct and startup logic. Mirrors the Telegram/WhatsApp agent pattern.

use super::DiscordState;
use super::handler;
use super::writes::{self, Class};
use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::db::ChannelMessageRepository;
use crate::services::{ServiceContext, SessionService};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use serenity::async_trait;
use serenity::model::application::{CommandType, Interaction};
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::guild::Member;
use serenity::prelude::*;

/// Discord bot that forwards messages to the AgentService
pub struct DiscordAgent {
    agent_service: Arc<AgentService>,
    session_service: SessionService,
    /// Kept alongside the service handles: plan Discard clears the session
    /// goal through `GoalManager`, which needs the pool (FR-008).
    service_context: ServiceContext,
    shared_session_id: Arc<Mutex<Option<Uuid>>>,
    discord_state: Arc<DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
}

impl DiscordAgent {
    pub fn new(
        agent_service: Arc<AgentService>,
        service_context: ServiceContext,
        shared_session_id: Arc<Mutex<Option<Uuid>>>,
        discord_state: Arc<DiscordState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
        channel_msg_repo: ChannelMessageRepository,
    ) -> Self {
        Self {
            agent_service,
            session_service: SessionService::new(service_context.clone()),
            service_context,
            shared_session_id,
            discord_state,
            config_rx,
            channel_msg_repo,
        }
    }

    /// Start the bot as a background task. Returns a JoinHandle.
    pub fn start(self, token: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Validate token format - Discord tokens are typically ~70 chars
            if token.is_empty() || token.len() < 50 {
                tracing::debug!("Discord bot token not configured or invalid, skipping bot start");
                return;
            }

            let cfg = self.config_rx.borrow().clone();
            tracing::info!(
                "Starting Discord bot with {} allowed user(s), STT={}, TTS={}",
                cfg.channels.discord.allowed_users.len(),
                cfg.voice_config().stt_enabled,
                cfg.voice_config().tts_enabled,
            );

            let extra_sessions: Arc<Mutex<HashMap<u64, (Uuid, std::time::Instant)>>> =
                Arc::new(Mutex::new(HashMap::new()));

            let agent = self.agent_service;
            let session_svc = self.session_service;
            let service_context = self.service_context;
            let shared_session = self.shared_session_id;
            let discord_state = self.discord_state;
            let config_rx = self.config_rx;
            let channel_msg_repo = self.channel_msg_repo;

            // Reactions are their own bits, not part of GUILD_MESSAGES: without
            // them `reaction_add` below compiles, passes its tests and never
            // fires. `discord_intent_coherence_test` now pins the pairing.
            //
            // The base set is never refused. MESSAGE_CONTENT is privileged
            // too, but Discord answers a request for it without the toggle by
            // sending empty content rather than refusing the IDENTIFY, so
            // GUILD_MEMBERS is the only bit that can fail the handshake.
            let base_intents = GatewayIntents::GUILD_MESSAGES
                | GatewayIntents::DIRECT_MESSAGES
                | GatewayIntents::MESSAGE_CONTENT
                | GatewayIntents::GUILD_MESSAGE_REACTIONS
                | GatewayIntents::DIRECT_MESSAGE_REACTIONS;

            // GUILD_MEMBERS (FR-004) is PRIVILEGED, and it carries one feature:
            // the welcome message. Asking for it while the application toggle
            // is off makes Discord refuse the IDENTIFY outright. That refusal
            // must cost the FEATURE, not the channel, so the retry loop below
            // drops the bit and reconnects: messages, reactions and slash
            // commands keep working on a bot whose owner never flipped the
            // Portal toggle. See `member_events::refused_identify`.
            let mut members_enabled = true;
            let intents_for = |members_enabled: bool| {
                if members_enabled {
                    base_intents | GatewayIntents::GUILD_MEMBERS
                } else {
                    base_intents
                }
            };

            let make_handler = || Handler {
                agent: agent.clone(),
                session_svc: session_svc.clone(),
                service_context: service_context.clone(),
                extra_sessions: extra_sessions.clone(),
                shared_session: shared_session.clone(),
                discord_state: discord_state.clone(),
                config_rx: config_rx.clone(),
                channel_msg_repo: channel_msg_repo.clone(),
            };

            let mut client = match Client::builder(&token, intents_for(members_enabled))
                .event_handler(make_handler())
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Discord: failed to create client: {}", e);
                    return;
                }
            };

            // Retry loop: if the gateway connection drops (network hiccup, Discord
            // server restart, etc.), wait and reconnect instead of dying silently.
            loop {
                tracing::info!("Discord: starting gateway connection");
                if let Err(e) = client.start().await {
                    // NFR-001: a refused IDENTIFY is not transient, and
                    // reconnecting cannot flip a Developer Portal toggle. It
                    // is also not fatal: the refusal names a privileged bit,
                    // and only GUILD_MEMBERS is requested, so dropping that
                    // bit keeps every other surface alive. Stop only when the
                    // refusal survives the drop: then the base set itself is
                    // being rejected and no Portal toggle can fix that.
                    if super::member_events::refused_identify(&e) {
                        if !members_enabled {
                            tracing::error!(
                                "{} Gateway error: {}",
                                super::member_events::MISSING_TOGGLE_HINT,
                                e
                            );
                            return;
                        }
                        tracing::warn!(
                            "{} Gateway error: {}",
                            super::member_events::DEGRADE_HINT,
                            e
                        );
                        members_enabled = false;
                    } else {
                        tracing::error!("Discord: client error: {} — reconnecting in 5s", e);
                    }
                } else {
                    tracing::warn!("Discord: client exited unexpectedly — reconnecting in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                client = match Client::builder(&token, intents_for(members_enabled))
                    .event_handler(make_handler())
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Discord: failed to rebuild client: {}", e);
                        return;
                    }
                };
            }
        })
    }
}

/// Serenity event handler — routes messages to the agent
struct Handler {
    agent: Arc<AgentService>,
    session_svc: SessionService,
    /// Pool handle for the plan card's Discard (FR-008): `plan_mode::discard`
    /// clears the session goal through `GoalManager`, which needs the pool.
    service_context: ServiceContext,
    extra_sessions: Arc<Mutex<HashMap<u64, (Uuid, std::time::Instant)>>>,
    shared_session: Arc<Mutex<Option<Uuid>>>,
    discord_state: Arc<DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
}

#[async_trait]
impl EventHandler for Handler {
    async fn reaction_add(&self, ctx: Context, reaction: serenity::model::channel::Reaction) {
        let agent = self.agent.clone();
        let session_svc = self.session_svc.clone();
        let discord_state = self.discord_state.clone();
        let config_rx = self.config_rx.clone();
        tokio::spawn(async move {
            super::reactions::handle_reaction_add(
                &ctx,
                &reaction,
                agent,
                session_svc,
                discord_state,
                config_rx,
            )
            .await;
        });
    }

    /// FR-004: greet a member who just joined.
    ///
    /// Delegated the same way `reaction_add` is: the handler is the wiring and
    /// the module holds the behaviour, so the greeting can await an HTTP round
    /// trip without blocking the gateway's event loop.
    async fn guild_member_addition(&self, ctx: Context, new_member: Member) {
        let config_rx = self.config_rx.clone();
        tokio::spawn(async move {
            super::member_events::handle_member_addition(&ctx, &new_member, config_rx).await;
        });
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(
            "Discord: connected as {} (id={})",
            ready.user.name,
            ready.user.id
        );
        self.discord_state
            .set_connected(ctx.http.clone(), None)
            .await;
        self.discord_state
            .set_bot_user_id(ready.user.id.get())
            .await;

        // FR-003: the bot's own activity line. Discord keeps a presence until
        // something changes it, so a process that died mid-turn would reconnect
        // still advertising work it is not doing. `ready` is the only hook that
        // runs on every connect, which is what makes it the place to reconcile
        // rather than a place to announce. No intent is involved: this is the
        // bot's own status (gateway opcode 3), not other members' presence, so
        // `GUILD_PRESENCES` stays unrequested.
        ctx.set_activity(super::presence::steady());

        // Application commands (#1850): project `commands.toml` onto Discord's
        // slash-command list so the catalog the TUI completes and Telegram
        // menus is the same one this channel autocompletes. `ready` is the only
        // hook holding an HTTP handle, and the retry loop above rebuilds the
        // client (not the handler) after a gateway drop, so a reconnect
        // re-plans and the key comparison decides whether anything is actually
        // sent. Registration is global (FR-002), which is what makes the
        // commands reachable in a DM: a guild-scoped set is not served there.
        // The guild list is still collected, because the same sync clears the
        // guild-scoped set this feature used to write, and a guild joined while
        // the process was down appears in `ready.guilds` on the next connect,
        // moving the key and getting its stale set cleared without a config
        // write. A guild joined while we are connected is covered the same way,
        // on the next reconnect: the watcher below only knows the guild list
        // this `ready` reported.
        let guilds: Vec<serenity::model::id::GuildId> =
            ready.guilds.iter().map(|guild| guild.id).collect();
        let http = ctx.http.clone();
        let state = self.discord_state.clone();
        let mut config_rx = self.config_rx.clone();

        // First `ready` of the process: sync unconditionally, because the stored
        // key is `None`. A reconnect hands back that stored key instead, so a
        // gateway that flaps on a short retry loop does not re-PUT the whole
        // command tree on each pass; it costs one `commands.toml` read and
        // nothing else, unless the catalog or the guild set actually moved.
        let stored = *state.commands_sig.lock().await;
        let key = super::commands::sync_commands(&http, &guilds, stored).await;
        *state.commands_sig.lock().await = key;
        let start_watcher = !*state.commands_watcher_started.lock().await;
        if start_watcher {
            *state.commands_watcher_started.lock().await = true;
            tokio::spawn(async move {
                // The ConfigWatcher re-publishes on any config write, which
                // covers `commands.toml` (see the same contract documented in
                // `telegram::menu_refresh`). Each publish re-plans and compares
                // the key, so an unrelated config edit costs a file read and no
                // API call. Guild membership is folded into that key, so a
                // publish after the bot joined a server clears that guild's
                // stale scoped set rather than only the ones listed here.
                loop {
                    if config_rx.changed().await.is_err() {
                        break;
                    }
                    let last = *state.commands_sig.lock().await;
                    let next = super::commands::sync_commands(&http, &guilds, last).await;
                    *state.commands_sig.lock().await = next;
                }
            });
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Skip bot messages
        if msg.author.bot {
            return;
        }

        handler::handle_message(
            &ctx,
            &msg,
            self.agent.clone(),
            self.session_svc.clone(),
            self.shared_session.clone(),
            self.discord_state.clone(),
            self.config_rx.clone(),
            self.channel_msg_repo.clone(),
        )
        .await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        // FR-005 (AC-007, AC-008): suggestions while the user is still typing.
        // Answered first and in place: an autocomplete request has the same
        // three-second window as any other interaction and no fallback, so it
        // must not queue behind the command path's spawn. The decision lives in
        // `autocomplete.rs`; this is only the wiring. An unenumerable option
        // answers with an empty list, never an error, so there is no branch
        // here for the negative case.
        if let Interaction::Autocomplete(command) = &interaction {
            let cfg = self.config_rx.borrow().clone();
            super::autocomplete::answer(&ctx.http, command, &cfg, &self.session_svc).await;
            return;
        }

        // Every command interaction that asks the agent to do work lands here:
        // a catalog command picked from the `/` menu (#1850) and the two
        // right-click context menus (FR-006 / AC-009). The gate, the deferred
        // ack and the dispatch are shared with the context menus
        // (`interactions::handle_invoked_request`); only the request text
        // differs, and that is what this arm builds.
        if let Interaction::Command(command) = &interaction {
            let request = match command.data.kind {
                // #1850: rebuild the invocation as the text the user would have
                // typed, so the agent receives `/check args`, exactly what a
                // hand-typed message produces. Parity is structural: one command
                // implementation, and the arguments survive inside the text.
                //
                // Deliberately NOT the bare `route_interaction_turn`. Its own
                // contract is a single completion with no tool loop (see
                // `interactions.rs`), so `/check` picked from the menu would come
                // back saying it cannot run cargo. The two synthetic branches
                // that stay on it are synthetic: a modal fill and a select pick
                // are steering prompts, an invoked command is a request to do
                // work.
                CommandType::ChatInput => Some(super::commands::invocation(command)),
                // FR-006: the request is the right-clicked target.
                CommandType::Message | CommandType::User => {
                    super::context_menu::invocation(command)
                }
                // Nothing else is registered, so this is a type the client sent
                // that we never asked for. Refused rather than guessed at.
                _ => None,
            };
            // `None` is a target the client named but did not resolve, which is
            // how Discord reports a message deleted between the right-click and
            // the interaction landing. Answering in place beats letting the
            // interaction time out into the red "didn't respond" banner, and
            // running a turn on a message nobody can see is worse than both.
            let Some(request) = request else {
                tracing::warn!(
                    "Discord: {:?} interaction with no readable target, refusing",
                    command.data.name
                );
                let _ = command
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Message(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .content("Nothing to ask about: the target could not be read.")
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            // Per-channel `/respond_to` and `/cowork` need this channel's id,
            // the owner verdict and the thread's parent. The admission gate
            // itself stays in `interactions::handle_invoked_request`, where the
            // fork enforces it; here we resolve only what the scope reply needs
            // (#2014). The parent lookup runs for a guild channel only.
            let cfg = self.config_rx.borrow().clone();
            let dc = &cfg.channels.discord;
            let is_dm = command.guild_id.is_none();
            let channel_str = command.channel_id.get().to_string();
            let owner = crate::config::owner::is_owner(
                &dc.allowed_users,
                &dc.bot_owner,
                &command.user.id.get().to_string(),
            );
            let parent = if is_dm {
                None
            } else {
                super::commands::parent_channel_id(&ctx.http, command.channel_id).await
            };

            // `/respond_to` from the menu is answered here, not routed to the
            // model as a prompt. It never writes Telegram's section (#2013).
            let cowork_name = if !is_dm && super::cowork::is_cowork_command(&invocation) {
                super::cowork::channel_name(&ctx.http, command.channel_id).await
            } else {
                None
            };
            let scope_reply = super::cowork::cowork_discord_channel(
                &invocation,
                owner,
                !is_dm,
                &channel_str,
                cowork_name.as_deref(),
                super::cowork::write_channel_open,
            )
            .or_else(|| {
                crate::channels::respond_to_scope::respond_to_discord_channel(
                    &invocation,
                    owner,
                    &channel_str,
                    &dc.respond_to_for(&channel_str, parent.as_deref()),
                    super::commands::write_channel_respond_to,
                )
            });
            if let Some(reply) = scope_reply {
                if let Err(e) = command
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Message(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .content(reply)
                                .ephemeral(true),
                        ),
                    )
                    .await
                {
                    tracing::warn!("Discord: /{} reply refused: {e}", command.data.name);
                }
                return;
            }

            let idle = dc.session_idle_hours;
            // History keeps the invocation the way a typed message would:
            // `Sender: /cmd args` in a guild, bare in the owner's DM, the same
            // rule `handler.rs` uses. `context_text` is the invocation itself,
            // which is what the model sees when you type it.
            let history_line = if owner && is_dm {
                invocation.clone()
            } else {
                format!("{user_name}: {invocation}")
            };
            super::interactions::handle_invoked_request(
                &ctx,
                command,
                self.agent.clone(),
                self.session_svc.clone(),
                self.discord_state.clone(),
                self.config_rx.clone(),
                request,
            )
            .await;
            return;
        }

        // Modal submissions (#383): route the filled fields back as a turn.
        if let Interaction::Modal(modal) = &interaction {
            let custom_id = modal.data.custom_id.clone();
            if let Some(form_id) = custom_id.strip_prefix("formsub:") {
                let Some(spec) = self.discord_state.take_form(form_id).await else {
                    let _ack = modal
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Acknowledge,
                        )
                        .await;
                    return;
                };
                // Collect input values in field order.
                use serenity::model::application::ActionRowComponent;
                let mut values: Vec<String> = Vec::new();
                for row in &modal.data.components {
                    for comp in &row.components {
                        if let ActionRowComponent::InputText(input) = comp {
                            values.push(input.value.clone().unwrap_or_default());
                        }
                    }
                }
                let filled: Vec<String> = spec
                    .fields
                    .iter()
                    .zip(values.iter())
                    .map(|(field, v)| format!("{}: {v}", field.label))
                    .collect();
                let user = modal.user.id.get();
                let user_name = modal
                    .user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| modal.user.name.clone());
                let is_dm = modal.guild_id.is_none();
                let channel_id = modal.channel_id.get();
                let _ack = modal
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Acknowledge,
                    )
                    .await;
                let agent = self.agent.clone();
                let session_svc = self.session_svc.clone();
                let idle = self.config_rx.borrow().channels.discord.session_idle_hours;
                let title = spec.title.clone();
                let ctx2 = ctx.clone();
                tokio::spawn(async move {
                    super::interactions::route_interaction_turn(
                        &ctx2,
                        agent,
                        session_svc,
                        is_dm,
                        user,
                        channel_id,
                        idle,
                        format!(
                            "[{user_name} submitted the \"{title}\" form]\n{}",
                            filled.join("\n")
                        ),
                        format!("[System: {user_name} submitted the \"{title}\" form]"),
                    )
                    .await;
                });
                return;
            }
        }

        if let Some(comp) = interaction.message_component() {
            let custom_id = comp.data.custom_id.as_str();
            tracing::info!("Discord callback received: custom_id={}", custom_id);

            // Optional follow-up suggestion tapped (#598): inject the chosen
            // suggestion as the user's next message (a fresh turn). Options were
            // stashed under `followup:<id>:<idx>` via the TTL-bounded select map.
            if let Some(rest) = custom_id.strip_prefix(super::suggest_options::FOLLOWUP_PREFIX) {
                let ttl = self.config_rx.borrow().channels.discord.component_ttl_hours;
                let picked: Option<String> = if let Some((id, idx_str)) = rest.rsplit_once(':') {
                    match (
                        self.discord_state.take_select(id, ttl).await,
                        idx_str.parse::<usize>(),
                    ) {
                        (Some(opts), Ok(idx)) => opts.get(idx).cloned(),
                        _ => None,
                    }
                } else {
                    None
                };
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let Some(choice) = picked else {
                    let _e = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content("⌛ These suggestions expired.")
                                    .components(Vec::new()),
                            ),
                        )
                        .await;
                    return;
                };
                let user = comp.user.id.get();
                let is_dm = comp.guild_id.is_none();
                let channel_id = comp.channel_id.get();
                let display_tag = comp
                    .user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| comp.user.name.clone());
                // Ack by disabling the buttons and echoing the pick.
                let _e = comp
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::UpdateMessage(
                            CreateInteractionResponseMessage::new()
                                .content(format!("\u{25b6}\u{fe0f} {choice}"))
                                .components(Vec::new()),
                        ),
                    )
                    .await;
                let agent = self.agent.clone();
                let session_svc = self.session_svc.clone();
                let discord_state = self.discord_state.clone();
                let idle = self.config_rx.borrow().channels.discord.session_idle_hours;
                let ctx2 = ctx.clone();
                tokio::spawn(async move {
                    // #1852: tapped suggestions ride the tool-loop display
                    // path (live status, tools, approvals, chained buttons)
                    // instead of the bare single-call interaction route.
                    // No interaction token: the tap already resolved its
                    // interaction with `UpdateMessage` (FR-002 is slash-only).
                    super::interactions::route_followup_turn(
                        &ctx2,
                        agent,
                        session_svc,
                        discord_state,
                        None,
                        is_dm,
                        user,
                        channel_id,
                        idle,
                        choice,
                        display_tag,
                    )
                    .await;
                });
                return;
            }

            // Select menu pick (#382), with lazy TTL (#386).
            if let Some(sel_id) = custom_id.strip_prefix("sel:") {
                let ttl = self.config_rx.borrow().channels.discord.component_ttl_hours;
                let options = self.discord_state.take_select(sel_id, ttl).await;
                use serenity::model::application::ComponentInteractionDataKind;
                // A multi-select menu reports every pick, so collect them all
                // instead of reading only the first (milestone 2).
                let picked: Option<Vec<String>> = match (&comp.data.kind, options) {
                    (ComponentInteractionDataKind::StringSelect { values }, Some(opts)) => Some(
                        values
                            .iter()
                            .filter_map(|v| v.parse::<usize>().ok())
                            .filter_map(|i| opts.get(i).cloned())
                            .collect(),
                    ),
                    _ => None,
                };
                let Some(choice) = picked.filter(|c| !c.is_empty()) else {
                    // Expired or unknown: say so and strip the dead menu.
                    use serenity::builder::{
                        CreateInteractionResponse, CreateInteractionResponseMessage,
                    };
                    let _e = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content("⌛ This menu expired.")
                                    .components(Vec::new()),
                            ),
                        )
                        .await;
                    return;
                };
                let choice = choice.join(", ");
                let user = comp.user.id.get();
                let user_name = comp
                    .user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| comp.user.name.clone());
                let is_dm = comp.guild_id.is_none();
                let channel_id = comp.channel_id.get();
                // Ack by disabling the menu and showing the pick.
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let _e = comp
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::UpdateMessage(
                            CreateInteractionResponseMessage::new()
                                .content(format!("✅ {user_name} picked: {choice}"))
                                .components(Vec::new()),
                        ),
                    )
                    .await;
                let agent = self.agent.clone();
                let session_svc = self.session_svc.clone();
                let idle = self.config_rx.borrow().channels.discord.session_idle_hours;
                let ctx2 = ctx.clone();
                tokio::spawn(async move {
                    super::interactions::route_interaction_turn(
                        &ctx2,
                        agent,
                        session_svc,
                        is_dm,
                        user,
                        channel_id,
                        idle,
                        format!(
                            "[{user_name} picked \"{choice}\" from your select menu — \
                             continue accordingly]"
                        ),
                        format!("[System: {user_name} picked \"{choice}\"]"),
                    )
                    .await;
                });
                return;
            }

            // Form button (#383): open the modal, with lazy TTL (#386).
            if let Some(form_id) = custom_id.strip_prefix("form:") {
                let ttl = self.config_rx.borrow().channels.discord.component_ttl_hours;
                let Some(spec) = self.discord_state.get_form(form_id, ttl).await else {
                    use serenity::builder::{
                        CreateInteractionResponse, CreateInteractionResponseMessage,
                    };
                    let _e = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .content("⌛ This form expired.")
                                    .components(Vec::new()),
                            ),
                        )
                        .await;
                    return;
                };
                use serenity::builder::{
                    CreateActionRow, CreateInputText, CreateInteractionResponse, CreateModal,
                };
                use serenity::model::application::InputTextStyle;
                let rows: Vec<CreateActionRow> = spec
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| {
                        let style = if field.multiline {
                            InputTextStyle::Paragraph
                        } else {
                            InputTextStyle::Short
                        };
                        let mut input =
                            CreateInputText::new(style, field.label.clone(), format!("field:{i}"));
                        if let Some(placeholder) = &field.placeholder {
                            input = input.placeholder(placeholder.clone());
                        }
                        if !field.required {
                            input = input.required(false);
                        }
                        if let Some(min) = field.min_length {
                            input = input.min_length(min);
                        }
                        if let Some(max) = field.max_length {
                            input = input.max_length(max);
                        }
                        if let Some(value) = &field.value {
                            input = input.value(value.clone());
                        }
                        CreateActionRow::InputText(input)
                    })
                    .collect();
                let modal = CreateModal::new(format!("formsub:{form_id}"), spec.title.clone())
                    .components(rows);
                if let Err(e) = comp
                    .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
                    .await
                {
                    tracing::warn!("Discord: failed to open modal: {e}");
                }
                return;
            }

            // Provider picker callback → show models for that provider
            if let Some(rest) = custom_id.strip_prefix(super::long_answer::PAGER_PREFIX) {
                // FR-009: reveal one page of a long answer WITHOUT adding a
                // message to the channel — ephemeral, and it carries its own
                // position so a pasted-out-of-context page still says where it
                // came from. An aged-out pager answers plainly instead of
                // silently doing nothing.
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let mut parts = rest.splitn(2, ':');
                let mid = parts.next().and_then(|s| s.parse::<u64>().ok());
                let page = parts.next().and_then(|s| s.parse::<usize>().ok());
                let pages = match mid {
                    Some(mid) => self.discord_state.long_answer_pages(mid).await,
                    None => None,
                };
                let body = pages.as_deref().and_then(|pages| {
                    page.and_then(|page| super::long_answer::page_body(pages, page))
                });
                let content = body.unwrap_or_else(|| {
                    "That pager aged out. Ask me again and I will repost the answer.".to_string()
                });
                // Carry the pager forward: every page is reachable only by
                // press, so the ephemeral answer re-draws the SAME row for the
                // page it is showing. `◀` walks back, `▶` walks on, and the
                // ends come back disabled instead of the row vanishing
                // (FR-009).
                let mut msg = CreateInteractionResponseMessage::new()
                    .content(content)
                    .ephemeral(true);
                if let (Some(mid), Some(pages), Some(page)) = (mid, pages.as_ref(), page)
                    && pages.len() > 1
                {
                    let row = super::long_answer::pager_row(mid, page, pages.len());
                    msg = msg.components(vec![row]);
                }
                let resp = CreateInteractionResponse::Message(msg);
                if let Err(e) = comp.create_response(&ctx.http, resp).await {
                    tracing::warn!("Discord: long-answer page response failed: {e}");
                }
                return;
            }

            // Tool-group Expand/Collapse toggle (#380): flip stored state
            // and update THIS message via the interaction response.
            if let Some(mid_str) = custom_id.strip_prefix("toolgroup:") {
                // Every path must resolve the interaction (#1949): a bare
                // `Acknowledge` or a skipped response leaves Discord
                // spinning its "didn't respond in time" toast.
                // `render_content` clamps to the 2000-char wire cap, but
                // if the API still refuses, answer with an ephemeral
                // fallback instead of leaving the click unresolved.
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let resp = match mid_str.parse::<u64>() {
                    Ok(mid) => match self.discord_state.toggle_tool_group(mid).await {
                        // The card was created through `writes`, so it is an
                        // embed; this response must redraw the SAME shape or the
                        // first Expand press flips the card back to plain text
                        // (#170). `auto_embed_update` is the choke point for the
                        // raw interaction responses the create/edit helpers
                        // cannot see.
                        Some(group) => CreateInteractionResponse::UpdateMessage(
                            super::embed::auto_embed_update(
                                CreateInteractionResponseMessage::new()
                                    .content(super::tool_group::render_content(&group))
                                    .components(super::tool_group::render_components(&group, mid)),
                            ),
                        ),
                        None => {
                            tracing::debug!("Discord: tool group {mid} aged out — toggle ignored");
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .ephemeral(true)
                                    .content("This status bubble is too old to expand."),
                            )
                        }
                    },
                    Err(_) => CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("This status bubble is too old to expand."),
                    ),
                };
                if let Err(e) = comp.create_response(&ctx.http, resp).await {
                    tracing::warn!("Discord: tool group toggle response failed: {e}");
                    let fallback = CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("Couldn't redraw the status bubble — try again shortly."),
                    );
                    if let Err(e2) = comp.create_response(&ctx.http, fallback).await {
                        tracing::warn!("Discord: tool group toggle fallback response failed: {e2}");
                    }
                }
                return;
            }

            // Plan card Approve/Discard (`plan:` prefix, deliberately distinct
            // from the tool-approval `approve:{id}` family). Owner-only for the
            // same reason Telegram is: the keyboard sits in a channel any
            // allowlisted member can see, so the tapper is re-checked here.
            if super::plan_card::is_plan_callback(custom_id) {
                use serenity::builder::{
                    CreateInteractionResponse, CreateInteractionResponseMessage,
                };
                let cfg = self.config_rx.borrow().clone();
                let caller = comp.user.id.get().to_string();
                let is_owner = crate::config::owner::is_owner(
                    &cfg.channels.discord.allowed_users,
                    &cfg.channels.discord.bot_owner,
                    &caller,
                );
                if !is_owner {
                    tracing::warn!(
                        "Discord: non-owner {} tapped '{}' — refused (OC-01)",
                        caller,
                        custom_id
                    );
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("🔒 Owner only")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    return;
                }

                let channel_id = comp.channel_id;
                let Some(session_id) = self
                    .discord_state
                    .session_owner_by_channel(channel_id.get())
                    .await
                else {
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("No session for this channel.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    return;
                };

                if custom_id == super::plan_card::PLAN_DISCARD {
                    // Discard cancels the running turn first, exactly like the
                    // Telegram arm: the plan is going away, so letting the turn
                    // keep executing would write results against a dead plan.
                    let cancelled = self.discord_state.cancel_session(session_id).await;
                    let mut reply =
                        crate::utils::plan_mode::discard(session_id, &self.service_context).await;
                    if cancelled {
                        reply = format!("⏹️ Cancelled the running turn. {reply}");
                    }
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Plan discarded")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    super::plan_card::remove_plan_card(
                        &ctx.http,
                        channel_id,
                        &self.discord_state,
                        session_id,
                    )
                    .await;
                    if let Err(e) = writes::say(&ctx.http, channel_id, &reply, Class::Final).await {
                        tracing::warn!("Discord: plan discard note failed: {e}");
                    }
                    return;
                }

                // plan:ok — Approve, or the empty-tasks seed retry.
                match crate::utils::plan_mode::try_approve(
                    session_id,
                    crate::tui::plan::ApprovalSource::User,
                )
                .await
                {
                    crate::utils::plan_mode::ApproveOutcome::Refused(msg) => {
                        let _ = comp
                            .create_response(
                                &ctx.http,
                                CreateInteractionResponse::Message(
                                    CreateInteractionResponseMessage::new()
                                        .content(msg)
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                    }
                    crate::utils::plan_mode::ApproveOutcome::SeedTurn { prompt } => {
                        let _ = comp
                            .create_response(
                                &ctx.http,
                                CreateInteractionResponse::Message(
                                    CreateInteractionResponseMessage::new()
                                        .content("✅ Plan approved — starting now…")
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                        // Visible seed turn, spawned so the callback answers
                        // inside Discord's 3s window. Runs through the same
                        // resume path a background task uses, so the result
                        // lands in the channel like any other turn.
                        let agent = self.agent.clone();
                        let http = ctx.http.clone();
                        let target = channel_id.get().to_string();
                        tokio::spawn(async move {
                            // One let-chain rather than a nested `if let`:
                            // clippy's `collapsible_if` fires on the nested
                            // form under `-D warnings`, and edition 2024
                            // allows the chained form.
                            if let Some(content) = crate::channels::bg_resume::run_resume_turn(
                                agent, session_id, prompt, "discord", &target,
                            )
                            .await
                                && let Err(e) =
                                    writes::say(&http, channel_id, &content, Class::Final).await
                            {
                                tracing::warn!("Discord: plan approval turn delivery failed: {e}");
                            }
                        });
                    }
                }
                return;
            }

            if let Some(provider_name) = custom_id.strip_prefix("provider:") {
                let resp = crate::channels::commands::models_for_provider(provider_name).await;

                // Agent-handled providers (OpenRouter 300+ models, custom)
                if resp.agent_handled {
                    let session_id = *self.shared_session.lock().await;
                    let display = crate::channels::commands::provider_display_name(provider_name);
                    let config = crate::config::Config::current();
                    if let Ok(new_provider) =
                        crate::brain::provider::factory::create_provider_by_name(
                            &config,
                            provider_name,
                        )
                        .await
                    {
                        match session_id {
                            Some(sid) => self.agent.swap_provider_for_session(
                                sid,
                                new_provider.clone(),
                                new_provider.default_model().to_string(),
                            ),
                            None => self.agent.swap_provider(new_provider),
                        }
                    }
                    if !resp.current_model.is_empty() {
                        let _ = crate::channels::commands::switch_model(
                            &self.agent,
                            &resp.current_model,
                            session_id,
                            Some(provider_name),
                        )
                        .await;
                    }
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Acknowledge,
                        )
                        .await;
                    if let Some(sid) = session_id {
                        let prompt = if resp.current_model.is_empty() {
                            format!(
                                "[System: User selected {} provider but no default model is set. \
                                 Ask them which model they want. Use config_manager tool to read \
                                 providers section, then set the default_model. Keep current provider \
                                 until a model is chosen.]",
                                display
                            )
                        } else {
                            format!(
                                "[System: User switched to {} provider with model {}. \
                                 Confirm the switch. Ask if they want a different model — \
                                 if so, use config_manager to update providers.{}.default_model \
                                 and confirm.]",
                                display,
                                resp.current_model,
                                if provider_name == "openrouter" {
                                    "openrouter"
                                } else {
                                    provider_name
                                }
                            )
                        };
                        let agent_clone = self.agent.clone();
                        let http = ctx.http.clone();
                        let channel_id = comp.channel_id;
                        tokio::spawn(async move {
                            match agent_clone.send_message(sid, prompt, None).await {
                                Ok(r) => {
                                    if let Err(e) =
                                        writes::say(&http, channel_id, &r.content, Class::Final)
                                            .await
                                    {
                                        tracing::warn!(error = %e, "failed to send Discord agent message");
                                    }
                                }
                                Err(e) => tracing::error!("Agent follow-up failed: {}", e),
                            }
                        });
                    }
                    return;
                }

                if resp.models.is_empty() {
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content("No models available for this provider.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    return;
                }
                use serenity::builder::{
                    CreateActionRow, CreateButton, CreateInteractionResponse,
                    CreateInteractionResponseMessage,
                };
                use serenity::model::application::ButtonStyle;
                let rows: Vec<CreateActionRow> = resp
                    .models
                    .chunks(5)
                    .take(5)
                    .map(|chunk| {
                        CreateActionRow::Buttons(
                            chunk
                                .iter()
                                .map(|m| {
                                    let label = if *m == resp.current_model {
                                        format!("✓ {}", m)
                                    } else {
                                        m.clone()
                                    };
                                    let label = if label.len() > 80 {
                                        let mut end = 79;
                                        while !label.is_char_boundary(end) {
                                            end -= 1;
                                        }
                                        format!("{}…", &label[..end])
                                    } else {
                                        label
                                    };
                                    CreateButton::new(format!("model:{}:{}", resp.provider_name, m))
                                        .label(label)
                                        .style(ButtonStyle::Secondary)
                                })
                                .collect(),
                        )
                    })
                    .collect();
                let _ = comp
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(&resp.text)
                                .components(rows)
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            // Model switch callback (format: model:<provider>:<model>)
            if let Some(rest) = custom_id.strip_prefix("model:") {
                let (provider_name, model_name) = if let Some((p, m)) = rest.split_once(':') {
                    (Some(p), m)
                } else {
                    (None, rest)
                };
                // Resolve session first so the provider swap pins to the
                // right per-session slot instead of leaking via the global.
                let session_id = *self.shared_session.lock().await;
                let mut provider_err: Option<String> = None;
                if let Some(pname) = provider_name {
                    match crate::config::Config::load() {
                        Ok(config) => {
                            match crate::brain::provider::factory::create_provider_by_name(
                                &config, pname,
                            )
                            .await
                            {
                                Ok(new_provider) => match session_id {
                                    Some(sid) => self.agent.swap_provider_for_session(
                                        sid,
                                        new_provider.clone(),
                                        new_provider.default_model().to_string(),
                                    ),
                                    None => self.agent.swap_provider(new_provider),
                                },
                                Err(e) => {
                                    provider_err = Some(format!(
                                        "Failed to create provider '{}': {}",
                                        pname, e
                                    ))
                                }
                            }
                        }
                        Err(e) => provider_err = Some(format!("Failed to load config: {}", e)),
                    }
                }
                let reply = if let Some(err) = provider_err {
                    format!("⚠️ {}", err)
                } else {
                    match crate::channels::commands::switch_model(
                        &self.agent,
                        model_name,
                        session_id,
                        provider_name,
                    )
                    .await
                    {
                        Ok(_) => format!("✅ Model switched to `{}`", model_name),
                        Err(e) => format!("⚠️ {}", e),
                    }
                };
                let _ = comp
                    .create_response(
                        &ctx.http,
                        serenity::builder::CreateInteractionResponse::Message(
                            serenity::builder::CreateInteractionResponseMessage::new()
                                .content(reply)
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            // Session switch callback
            if let Some(session_id_str) = custom_id.strip_prefix("session:") {
                if let Ok(new_id) = session_id_str.parse::<Uuid>() {
                    let cfg = self.config_rx.borrow().clone();
                    let caller_id = comp.user.id.get();
                    let owner_id = cfg
                        .channels
                        .discord
                        .allowed_users
                        .first()
                        .and_then(|s| s.parse::<u64>().ok());
                    let is_owner = cfg.channels.discord.allowed_users.is_empty()
                        || owner_id == Some(caller_id);

                    if is_owner {
                        *self.shared_session.lock().await = Some(new_id);
                    } else {
                        self.extra_sessions
                            .lock()
                            .await
                            .insert(caller_id, (new_id, std::time::Instant::now()));
                    }
                    self.discord_state
                        .register_session_channel(new_id, comp.channel_id.get())
                        .await;
                    let display = match self.session_svc.get_session(new_id).await {
                        Ok(Some(s)) => s.title.unwrap_or_else(|| {
                            session_id_str[..8.min(session_id_str.len())].to_string()
                        }),
                        _ => session_id_str[..8.min(session_id_str.len())].to_string(),
                    };
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content(format!("✅ Switched to session `{}`", display))
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                } else {
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content("Invalid session ID")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                }
                return;
            }

            let (approved, always, yolo, approval_id) =
                if let Some(id) = custom_id.strip_prefix("approve:") {
                    (true, false, false, id.to_string())
                } else if let Some(id) = custom_id.strip_prefix("always:") {
                    (true, true, false, id.to_string())
                } else if let Some(id) = custom_id.strip_prefix("yolo:") {
                    (true, true, true, id.to_string())
                } else if let Some(id) = custom_id.strip_prefix("deny:") {
                    (false, false, false, id.to_string())
                } else {
                    tracing::warn!("Discord: unknown interaction custom_id: {}", custom_id);
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Acknowledge,
                        )
                        .await;
                    return;
                };

            // OC-01: the approval keyboard sits in a channel where any member
            // can press it, so re-check that the tapper is the owner before
            // acting. Without this a non-owner tap runs the pending tool, and a
            // YOLO tap persists auto-always for the whole instance. The session
            // switch branch above already re-checks; the tool-approval buttons
            // did not. Uses the canonical owner resolver, so an empty allowlist
            // is unconfigured (deny), not "everyone is owner".
            {
                let cfg = self.config_rx.borrow().clone();
                let caller = comp.user.id.get().to_string();
                let is_owner = crate::config::owner::is_owner(
                    &cfg.channels.discord.allowed_users,
                    &cfg.channels.discord.bot_owner,
                    &caller,
                );
                if !is_owner {
                    tracing::warn!(
                        "Discord: non-owner {} tapped '{}' — refused (OC-01)",
                        caller,
                        custom_id
                    );
                    let _ = comp
                        .create_response(
                            &ctx.http,
                            serenity::builder::CreateInteractionResponse::Message(
                                serenity::builder::CreateInteractionResponseMessage::new()
                                    .content("⛔ Only the owner can approve tool calls.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    return;
                }
            }

            if yolo {
                crate::utils::persist_auto_always_policy();
            }

            let resolved = self
                .discord_state
                .resolve_pending_approval(&approval_id, approved, always)
                .await;
            tracing::info!(
                "Discord approval resolved: id={}, approved={}, always={}, found_pending={}",
                approval_id,
                approved,
                always,
                resolved
            );
            if !resolved {
                tracing::warn!(
                    "Discord: no pending approval for id={} — may have timed out or already resolved",
                    approval_id
                );
            }

            // Ack the interaction so Discord doesn't show "interaction failed"
            let _ = comp
                .create_response(
                    &ctx.http,
                    serenity::builder::CreateInteractionResponse::Acknowledge,
                )
                .await;
        }
    }
}
