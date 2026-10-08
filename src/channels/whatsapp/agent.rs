//! WhatsApp Agent
//!
//! Single bot instance — handles pairing, reconnection, and message processing.
//! Onboarding subscribes to QR/connected events via WhatsAppState.

use super::WhatsAppState;
use super::handler;
use super::history;
use super::newsletter;
use super::owner_alert;
use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::db::ChannelMessageRepository;
use crate::services::{ServiceContext, SessionService};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::store::Store;
use wacore::types::events::Event;
use whatsapp_rust::TokioRuntime;
use whatsapp_rust::bot::Bot;
use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
use whatsapp_rust_ureq_http_client::UreqHttpClient;

/// WhatsApp agent that forwards messages to the AgentService
pub struct WhatsAppAgent {
    agent_service: Arc<AgentService>,
    session_service: SessionService,
    shared_session_id: Arc<Mutex<Option<Uuid>>>,
    whatsapp_state: Arc<WhatsAppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
}

impl WhatsAppAgent {
    pub fn new(
        agent_service: Arc<AgentService>,
        service_context: ServiceContext,
        shared_session_id: Arc<Mutex<Option<Uuid>>>,
        whatsapp_state: Arc<WhatsAppState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
        channel_msg_repo: ChannelMessageRepository,
    ) -> Self {
        Self {
            agent_service,
            session_service: SessionService::new(service_context),
            shared_session_id,
            whatsapp_state,
            config_rx,
            channel_msg_repo,
        }
    }

    /// Start as a background task. Returns JoinHandle.
    /// Always starts — if no session exists, emits QR events for onboarding.
    /// If already paired, reconnects and handles messages.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Spawn the outbound rate-limit drainer exactly once per
            // process (#1407). The drainer snapshots the rate_limit
            // config here; later config changes reach the inline gate
            // paths immediately (they re-read config per turn) but the
            // drainer keeps this snapshot until restart (v1 tradeoff,
            // documented in rate_limit.rs).
            let rl_cfg = self.config_rx.borrow().channels.whatsapp.rate_limit.clone();
            super::rate_limit::spawn_drainer(self.whatsapp_state.clone(), rl_cfg);

            let db_path = crate::config::opencrabs_home()
                .join("whatsapp")
                .join("session.db");

            // Ensure parent directory exists
            if let Some(parent) = db_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // `with_backend` takes the backend by value now (it wraps it
            // internally); the old blanket `Backend for Arc<T>` impl is gone.
            let backend = match Store::new(db_path.to_string_lossy().as_ref()).await {
                Ok(store) => store,
                Err(e) => {
                    let msg = format!(
                        "Failed to open session store at {}: {}",
                        db_path.display(),
                        e
                    );
                    tracing::error!("WhatsApp: {}", msg);
                    self.whatsapp_state.broadcast_error(&msg);
                    return;
                }
            };

            let cfg = self.config_rx.borrow().clone();
            tracing::info!(
                "WhatsApp agent running (STT={}, TTS={})",
                cfg.voice_config().stt_enabled,
                cfg.voice_config().tts_enabled,
            );

            // Derive owner JID from first allowed phone (for proactive messaging)
            let owner_jid = cfg
                .channels
                .whatsapp
                .allowed_phones
                .first()
                .map(|p| format!("{}@s.whatsapp.net", p.trim_start_matches('+')));

            let agent = self.agent_service.clone();
            let session_svc = self.session_service.clone();
            let shared_session = self.shared_session_id.clone();
            let wa_state = self.whatsapp_state.clone();
            let config_rx = self.config_rx.clone();
            let channel_msg_repo = self.channel_msg_repo.clone();
            let owner_jid_clone = owner_jid.clone();

            let bot_result = Bot::builder()
                .with_backend(backend)
                .with_transport_factory(TokioWebSocketTransportFactory::new())
                .with_http_client(UreqHttpClient::new())
                .with_runtime(TokioRuntime)
                .on_event(move |event, client| {
                    let agent = agent.clone();
                    let session_svc = session_svc.clone();
                    let shared_session = shared_session.clone();
                    let wa_state = wa_state.clone();
                    let owner_jid = owner_jid_clone.clone();
                    let config_rx = config_rx.clone();
                    let channel_msg_repo = channel_msg_repo.clone();
                    async move {
                        match &*event {
                            Event::PairingQrCode(qr) => {
                                let code = &qr.code;
                                tracing::info!(
                                    "WhatsApp: QR code available (scan with your phone)"
                                );
                                wa_state.broadcast_qr(code);
                            }
                            Event::PairSuccess(s) => {
                                // The paired account's JID IS the owner (the
                                // person who scanned). Pin them as the
                                // authoritative owner so a config mismatch can
                                // never lock them out, and ensure they are in
                                // the allow list. Numbers are stored WITHOUT a
                                // leading '+' (matching sender_phone); the
                                // handler's wa_should_respond normalises both
                                // sides, so '+'-prefixed legacy entries still
                                // match.
                                let full = s.id.to_string();
                                let num = full
                                    .split('@')
                                    .next()
                                    .unwrap_or(&full)
                                    .split(':')
                                    .next()
                                    .unwrap_or(&full)
                                    .trim_start_matches('+')
                                    .to_string();
                                if num.is_empty() {
                                    tracing::warn!(
                                        "WhatsApp: pairing successful but could not extract \
                                         owner number from JID '{full}'"
                                    );
                                } else {
                                    tracing::info!("WhatsApp: pairing successful — owner is {num}");
                                    if let Err(e) = Config::write_key(
                                        "channels.whatsapp",
                                        "bot_owner",
                                        &format!("[\"{num}\"]"),
                                    ) {
                                        tracing::warn!(
                                            "WhatsApp: failed to persist bot_owner: {e}"
                                        );
                                    }
                                    // Append to allowed_phones if not already present.
                                    let mut allowed: Vec<String> =
                                        config_rx.borrow().channels.whatsapp.allowed_phones.clone();
                                    let present =
                                        allowed.iter().any(|a| a.trim_start_matches('+') == num);
                                    if !present {
                                        allowed.push(num.clone());
                                        let json = format!(
                                            "[{}]",
                                            allowed
                                                .iter()
                                                .map(|a| format!("\"{a}\""))
                                                .collect::<Vec<_>>()
                                                .join(",")
                                        );
                                        if let Err(e) = Config::write_key(
                                            "channels.whatsapp",
                                            "allowed_phones",
                                            &json,
                                        ) {
                                            tracing::warn!(
                                                "WhatsApp: failed to persist allowed_phones: {e}"
                                            );
                                        }
                                    }
                                    // Make the freshly-paired owner available to
                                    // the Connected handler (which sends the
                                    // confirmation greeting once the socket is
                                    // ready) and to the whatsapp_send tool.
                                    wa_state
                                        .set_owner_jid(format!("{num}@s.whatsapp.net"))
                                        .await;
                                    // Flag this as a fresh pairing so the
                                    // Connected handler knows to fire the
                                    // one-time greeting (not suppress it as a
                                    // routine restart).
                                    wa_state.set_first_pair_pending();
                                }
                            }
                            Event::Connected(_) => {
                                tracing::info!("WhatsApp: connected successfully");
                                // #1525: fire one bounded history request per
                                // opted-in chat, on connect. The phone answers
                                // with encrypted frames that arrive through the
                                // normal event stream; the capture gate in
                                // `handle_message` stores them and keeps them
                                // off the agent. Empty opt-in (the default)
                                // costs nothing here, and the work is spawned
                                // so the event loop never waits on the phone.
                                {
                                    let hist_chats = config_rx
                                        .borrow()
                                        .channels
                                        .whatsapp
                                        .history_import_chats
                                        .clone();
                                    if !hist_chats.is_empty() {
                                        let client = client.clone();
                                        let wa_state = wa_state.clone();
                                        let repo = channel_msg_repo.clone();
                                        tokio::spawn(async move {
                                            let owner = wa_state.owner_jid.lock().await.clone();
                                            for chat in hist_chats {
                                                let oldest =
                                                    match repo.oldest_for_chat("whatsapp", &chat).await
                                                    {
                                                        Ok(o) => o,
                                                        Err(e) => {
                                                            tracing::warn!(
                                                                "whatsapp history: anchor lookup failed for {chat}: {e}"
                                                            );
                                                            continue;
                                                        }
                                                    };
                                                let now = chrono::Utc::now();
                                                let plan = match &oldest {
                                                    Some((pid, sid, ts)) => history::plan_import(
                                                        Some((
                                                            pid.as_str(),
                                                            history::from_me_of(
                                                                sid,
                                                                owner.as_deref(),
                                                            ),
                                                            *ts,
                                                        )),
                                                        now,
                                                    ),
                                                    None => history::plan_import(None, now),
                                                };
                                                let Some(plan) = plan else { continue };
                                                let Ok(jid) =
                                                    chat.parse::<wacore_binary::jid::Jid>()
                                                else {
                                                    tracing::warn!(
                                                        "whatsapp history: unparseable chat id {chat}"
                                                    );
                                                    continue;
                                                };
                                                match client
                                                    .fetch_message_history(
                                                        &jid,
                                                        &plan.oldest_msg_id,
                                                        plan.oldest_from_me,
                                                        plan.oldest_ts_ms,
                                                        plan.count,
                                                    )
                                                    .await
                                                {
                                                    Ok(req) => tracing::info!(
                                                        "whatsapp history: import requested for {chat} (up to {} msgs, request {req})",
                                                        plan.count
                                                    ),
                                                    Err(e) => tracing::warn!(
                                                        "whatsapp history: request failed for {chat}: {e}"
                                                    ),
                                                }
                                            }
                                        });
                                    }
                                }
                                // #1487: refresh the local blocklist mirror so
                                // the inbound guard reflects blocks the owner
                                // made from their phone, not only ones the bot
                                // issued. One query per connection, never per
                                // message. A failure leaves the previous mirror
                                // in place; the server still enforces the block,
                                // so the worst case is the belt-and-braces layer
                                // being stale for this session.
                                // #1488: announce availability once per
                                // connection. Without it the paired account
                                // reads as permanently offline to everyone it
                                // talks to, and WhatsApp withholds presence
                                // updates from a client that never publishes
                                // its own. Non-fatal: a failure costs presence,
                                // not messaging.
                                if let Err(e) = client.presence().set_available().await {
                                    tracing::warn!(
                                        target: "whatsapp",
                                        error = %e,
                                        "could not publish availability; the account will \
                                         appear offline and contact presence may not arrive"
                                    );
                                }
                                match client.blocking().get_blocklist().await {
                                    Ok(entries) => {
                                        let jids: Vec<String> =
                                            entries.iter().map(|e| e.jid.to_string()).collect();
                                        let count = jids.len();
                                        wa_state.blocklist.replace(jids).await;
                                        tracing::debug!(
                                            target: "whatsapp",
                                            count,
                                            "blocklist mirror refreshed"
                                        );
                                    }
                                    Err(e) => tracing::warn!(
                                        target: "whatsapp",
                                        error = %e,
                                        "could not refresh the blocklist mirror; keeping the previous one"
                                    ),
                                }
                                // Prefer the freshly-paired owner (set on
                                // PairSuccess) over the startup-derived one,
                                // which is None on a first-time pairing.
                                let owner = match wa_state.owner_jid().await {
                                    Some(j) => Some(j),
                                    None => owner_jid.clone(),
                                };
                                // Greet only on a fresh pairing (first-time or
                                // re-pair after reset), not on every app restart
                                // or reconnect. The `first_pair_pending` flag is
                                // set by PairSuccess and consumed here: it is
                                // `true` exactly once per pairing, so a plain
                                // restart (where no PairSuccess fires) never
                                // triggers the greeting. The `was_connected`
                                // guard still suppresses keepalive reconnects
                                // within the same session.
                                let was_connected = wa_state.is_connected().await;
                                wa_state.set_connected(client.clone(), owner.clone()).await;
                                // #1529: the newsletter poller, one task per
                                // process session. First connect only: the
                                // event loop survives keepalive reconnects and
                                // a second poller would race the cursor.
                                // Empty opt-in (the default) never spawns.
                                if !was_connected
                                    && let Some(owner_jid) = owner.clone()
                                {
                                    let chans = config_rx
                                        .borrow()
                                        .channels
                                        .whatsapp
                                        .newsletters
                                        .clone();
                                    if !chans.is_empty() {
                                        tokio::spawn(newsletter::run_poller(
                                            wa_state.clone(),
                                            channel_msg_repo.clone(),
                                            owner_jid,
                                            chans,
                                        ));
                                    }
                                }
                                if was_connected {
                                    tracing::debug!(
                                        "WhatsApp: reconnected — suppressing duplicate \
                                         confirmation greeting"
                                    );
                                } else if wa_state.take_first_pair_pending() {
                                    // Fresh pairing: a real agent turn into the
                                    // owner's self-chat. Spawned so the event loop
                                    // is never blocked by a full agent turn.
                                    if let Some(jid) = owner {
                                        let num = jid.split('@').next().unwrap_or(&jid).to_string();
                                        tokio::spawn(handler::send_connection_greeting(
                                            client.clone(),
                                            agent.clone(),
                                            session_svc.clone(),
                                            wa_state.clone(),
                                            num,
                                        ));
                                    } else {
                                        tracing::warn!(
                                            "WhatsApp: connected but no owner number known — \
                                             skipping confirmation greeting"
                                        );
                                    }
                                } else {
                                    tracing::debug!(
                                        "WhatsApp: connected (app restart) — no fresh pair, \
                                         staying silent"
                                    );
                                }
                            }
                            Event::Messages(batch) => {
                                tracing::debug!(
                                    count = batch.messages.len(),
                                    "WhatsApp: Event::Messages received"
                                );
                                // One task per inbound message: the agent turn
                                // is a very large async state machine, and
                                // polling it inline inside the event-loop
                                // future overflows the worker stack (and would
                                // block the loop). Live traffic is one message
                                // per batch; an offline drain delivers several.
                                for inbound in batch.messages.iter() {
                                    tokio::spawn(handler::handle_message(
                                        (*inbound.message).clone(),
                                        (*inbound.info).clone(),
                                        client.clone(),
                                        agent.clone(),
                                        session_svc.clone(),
                                        shared_session.clone(),
                                        wa_state.clone(),
                                        config_rx.clone(),
                                        channel_msg_repo.clone(),
                                    ));
                                }
                            }
                            Event::TemporaryBan(ban) => {
                                // A ban is the one account event that always needs a
                                // human: nothing sends until it expires (#1999).
                                tracing::error!(
                                    code = ban.code.code(),
                                    expire_secs = ban.expire.as_secs(),
                                    has_url = ban.url.is_some(),
                                    stanza = ?ban.raw,
                                    "WhatsApp: account temporarily banned"
                                );
                                if owner_alert::claim_alert(
                                    &format!("ban:{}", ban.code.code()),
                                    Instant::now(),
                                ) {
                                    let text = owner_alert::ban_text(
                                        &ban.code,
                                        ban.expire,
                                        ban.message.as_deref(),
                                        ban.url.as_deref(),
                                    );
                                    owner_alert::alert_owner(&text).await;
                                }
                            }
                            Event::ConnectFailure(failure) => {
                                let text = owner_alert::connect_failure_text(
                                    &failure.reason,
                                    failure.message.as_deref(),
                                );
                                if owner_alert::connect_failure_needs_owner(&failure.reason) {
                                    tracing::error!(
                                        reason = failure.reason.code(),
                                        stanza = ?failure.raw,
                                        "WhatsApp: connection refused"
                                    );
                                    if owner_alert::claim_alert(
                                        &format!("connect:{}", failure.reason.code()),
                                        Instant::now(),
                                    ) {
                                        owner_alert::alert_owner(&text).await;
                                    }
                                } else {
                                    // The client retries these on its own, so alerting
                                    // per attempt would be noise rather than signal.
                                    tracing::warn!(
                                        reason = failure.reason.code(),
                                        "WhatsApp: {text}"
                                    );
                                }
                            }
                            Event::LoggedOut(info) => {
                                // The reason is the payload's whole point: a 403 account
                                // lock and a manual unlink used to log the same line and
                                // they call for opposite responses. `raw` carries the
                                // one-time `appeal_token`, which the server never repeats,
                                // so the stanza is logged before it is dropped.
                                tracing::warn!(
                                    on_connect = info.on_connect,
                                    reason = info.reason.code(),
                                    has_stanza = info.raw.is_some(),
                                    stanza = ?info.raw,
                                    "WhatsApp: logged out"
                                );
                                if owner_alert::claim_alert(
                                    &format!("logout:{}", info.reason.code()),
                                    Instant::now(),
                                ) {
                                    let text = owner_alert::logged_out_text(
                                        info.on_connect,
                                        &info.reason,
                                        info.logout_message
                                            .as_ref()
                                            .and_then(|m| m.header.as_deref()),
                                        info.logout_message
                                            .as_ref()
                                            .and_then(|m| m.subtext.as_deref()),
                                    );
                                    owner_alert::alert_owner(&text).await;
                                }
                            }
                            Event::Disconnected(_) => {
                                tracing::warn!("WhatsApp: disconnected");
                            }
                            Event::Receipt(receipt) => {
                                // A message we sent was accepted/delivered.
                                // `Delivered` is the normal recipient receipt;
                                // `Sender` is what a self-chat send gets back —
                                // the bot is paired AS the owner, so its replies
                                // go to the owner's own devices, which ack with
                                // `sender`, not `delivered`. Either one means the
                                // message landed, so surface the id for the
                                // onboarding connection test.
                                if matches!(
                                    receipt.r#type,
                                    wacore::types::presence::ReceiptType::Delivered
                                        | wacore::types::presence::ReceiptType::Sender
                                ) {
                                    for id in &receipt.message_ids {
                                        wa_state.broadcast_delivered(id);
                                    }
                                }
                            }
                            Event::ClientOutdated(outdated) => {
                                // The server no longer accepts this client build:
                                // nothing reconnects until the pinned crate moves.
                                tracing::error!(
                                    has_stanza = outdated.raw.is_some(),
                                    "WhatsApp: client version no longer accepted by the server"
                                );
                            }
                            Event::StreamReplaced(_) => {
                                // Another session claimed this account's stream. Our
                                // copy still looks connected while receiving nothing.
                                tracing::warn!(
                                    "WhatsApp: stream replaced by another session, so this client \
                                     is no longer receiving messages"
                                );
                            }
                            Event::StreamError(err) => {
                                tracing::warn!(code = %err.code, "WhatsApp: stream error");
                            }
                            Event::UndecryptableMessage(msg) => {
                                // Named instead of an anonymous catch-all dump: a
                                // message we could not decrypt, and where it came from.
                                tracing::warn!(
                                    chat = %msg.info.source.chat,
                                    sender = %msg.info.source.sender,
                                    id = ?msg.info.id,
                                    unavailable = msg.is_unavailable,
                                    reason = ?msg.decrypt_fail_mode,
                                    "WhatsApp: message could not be decrypted"
                                );
                            }
                            Event::IdentityChange(change) => {
                                // The peer reinstalled WhatsApp; sessions and sender
                                // keys were cleared for them.
                                tracing::info!(
                                    user = %change.user,
                                    implicit = change.implicit,
                                    "WhatsApp: peer identity key changed"
                                );
                            }
                            other => {
                                tracing::debug!("WhatsApp: unhandled event: {:?}", other);
                            }
                        }
                    }
                })
                .build()
                .await;

            let bot = match bot_result {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("Failed to build WhatsApp bot: {}", e);
                    tracing::error!("WhatsApp: {}", msg);
                    self.whatsapp_state.broadcast_error(&msg);
                    return;
                }
            };

            // `run()` drives the bot until the connection ends. It returns `()`
            // (no Result, no separate handle: build/credential failures surface
            // before this point). The live client is published to WhatsAppState
            // from the Connected event callback, so we don't need a handle here.
            bot.run().await;
            tracing::info!("WhatsApp: bot run loop exited");
        })
    }
}
