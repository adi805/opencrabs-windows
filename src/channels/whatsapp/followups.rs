//! Optional follow-up suggestions from `suggest_options` (#600).
//!
//! Suggestions render as a native-flow card when `interactive_buttons` is on
//! and the set fits the button cap (#1411), as a single-choice poll when the
//! set is larger than that cap (#1616), and as a numbered text list otherwise.
//! A bare numeric reply selects one (card taps are rewritten to the number)
//! and a poll vote selects by label; the set is consumed on a valid selection,
//! and it is cleared by the handler on any other message.

use std::sync::Arc;

use uuid::Uuid;
use wacore_binary::jid::Jid;
use whatsapp_rust::client::Client;

use super::WhatsAppState;

impl WhatsAppState {
    /// Stash this session's optional follow-up suggestions (#600).
    pub async fn set_pending_followups(&self, session_id: Uuid, options: Vec<String>) {
        self.pending_followups
            .lock()
            .await
            .insert(session_id, options);
    }

    /// If this session has pending suggestions and `reply` parses as a 1-based
    /// option number in range, consume the whole set and return the chosen
    /// suggestion. Returns None otherwise (leaving the set for the caller to
    /// clear on a non-selecting message).
    pub async fn take_followup_by_reply(&self, session_id: Uuid, reply: &str) -> Option<String> {
        let parsed: usize = reply.trim().parse().ok()?;
        if parsed == 0 {
            return None;
        }
        let mut map = self.pending_followups.lock().await;
        let options = map.get(&session_id)?;
        let chosen = options.get(parsed - 1).cloned();
        if chosen.is_some() {
            map.remove(&session_id);
        }
        chosen
    }

    /// If this session has pending suggestions and `label` is one of them,
    /// consume the whole set and return the matching suggestion.
    ///
    /// This is the poll path (#1616): a vote names its option by label, never
    /// by number, so the numeric selector above can never match one. Matching
    /// is trimmed and case-insensitive because the label makes the round trip
    /// through WhatsApp's poll proto.
    pub async fn take_followup_by_label(&self, session_id: Uuid, label: &str) -> Option<String> {
        let needle = label.trim().to_lowercase();
        if needle.is_empty() {
            return None;
        }
        let mut map = self.pending_followups.lock().await;
        let options = map.get(&session_id)?;
        let chosen = options
            .iter()
            .find(|o| o.trim().to_lowercase() == needle)
            .cloned();
        if chosen.is_some() {
            map.remove(&session_id);
        }
        chosen
    }

    /// Drop this session's pending follow-up suggestions (non-selecting message).
    pub async fn clear_pending_followups(&self, session_id: Uuid) {
        self.pending_followups.lock().await.remove(&session_id);
    }
}

/// Show a turn's suggestions to the chat: a native-flow card, a single-choice
/// poll, or the numbered text list, as the set and `interactive` allow.
///
/// Shared by the live turn and the background-completion block, so a resumed
/// turn that halts on `suggest_options` offers the same choices (#2030).
pub(crate) async fn send_suggestions(
    client: Arc<Client>,
    jid: Jid,
    wa: Arc<WhatsAppState>,
    session_id: Uuid,
    raw_options: Vec<String>,
    interactive: bool,
) {
    let numbered: String = raw_options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}. {}", i + 1, o))
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!(
        "\u{1f4a1} Suggested next:\n\n{numbered}\n\nReply with a number, or type your own."
    );
    let buttons: Vec<super::interactive::Button> = raw_options
        .iter()
        .enumerate()
        .map(|(i, o)| super::interactive::Button::new(format!("wa_suggest_{}", i + 1), o.clone()))
        .collect();
    use super::interactive::SuggestionSurface;
    // Over the button cap, a poll is the only one-tap surface
    // WhatsApp offers. A vote is matched back by label, so the
    // pending set has to be stashed before the send either way.
    let surface = super::interactive::suggestion_surface(raw_options.len(), interactive);
    wa.set_pending_followups(session_id, raw_options.clone())
        .await;

    if surface == SuggestionSurface::Poll {
        match client
            .polls()
            .create(
                jid.clone(),
                "\u{1f4a1} Suggested next: tap one, or reply with the number.",
                &raw_options,
                1,
            )
            .await
        {
            Ok((result, _secret)) => {
                // Votes name options by hash, so the poll's own
                // option list is the only way to read one back.
                wa.polls.remember(result.message_id, raw_options).await;
                return;
            }
            Err(e) => {
                // Not fatal: the numbered list below still
                // selects by number, so the turn degrades to
                // the text path rather than losing the
                // suggestions entirely.
                tracing::warn!(
                    error = %e,
                    "WhatsApp: suggestion poll failed, falling back to the numbered list"
                );
            }
        }
    }

    let msg = if surface == SuggestionSurface::Card {
        super::interactive::build(
            &body,
            Some("Tap an option, or reply with the number."),
            &buttons,
        )
    } else {
        waproto::whatsapp::Message {
            conversation: Some(body),
            ..Default::default()
        }
    };
    if let Err(e) = client.send_message(jid, msg).await {
        tracing::warn!(error = %e, "failed to send WhatsApp message");
    }
}
