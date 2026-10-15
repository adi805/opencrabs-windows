//! WhatsApp History Search Tool
//!
//! Read-only retrieval over the bounded history store (#1525): rows captured
//! from offline-sync / PDO-recovered frames plus live-captured messages, all
//! in the shared `channel_messages` table. The decision doc grants the agent
//! no write surface — import requests fire from the connect path when the
//! owner opts a chat in via config, never from here.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use crate::channels::whatsapp::history::clamp_search;
use crate::db::repository::channel_message::ChannelMessageRepository;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Read-only search over one WhatsApp chat's stored history.
pub struct WhatsAppHistoryTool {
    repo: ChannelMessageRepository,
}

impl WhatsAppHistoryTool {
    pub fn new(repo: ChannelMessageRepository) -> Self {
        Self { repo }
    }
}

/// Message for a search that matched nothing, naming the ids that DO have rows.
///
/// A bare "no stored history for X" is a dead end. WhatsApp reports some chats
/// by LID (`<digits>@lid`) and the store keeps whichever JID the platform sent,
/// while callers naturally ask for the phone-number form
/// (`<digits>@s.whatsapp.net`). Observed 2026-10-14: an active chat's rows were
/// stored under `210122852823201@lid`, so searching for the `@s.whatsapp.net`
/// form returned empty with nothing to suggest why. Listing the stored ids
/// turns a dead end into a self-correcting answer.
///
/// `known` is `None` when the stored-chat list could not be read, so the message
/// never claims "nothing is stored" on the strength of a failed query.
pub(crate) fn empty_history_hint(
    chat: &str,
    days: i64,
    query: &str,
    known: Option<&[(String, i64)]>,
) -> String {
    let qual = if query.is_empty() {
        String::new()
    } else {
        format!(" matching '{query}'")
    };
    let mut msg = format!("No stored WhatsApp history for {chat} in the last {days} day(s){qual}.");
    match known {
        None => msg.push_str(
            " (The stored-chat list could not be read, so this is not proof \
                              that the chat has no messages.)",
        ),
        Some([]) => msg.push_str(" No WhatsApp chat has any stored messages yet."),
        Some(list) => {
            let ids: Vec<String> = list
                .iter()
                .take(10)
                .map(|(id, count)| format!("{id} ({count})"))
                .collect();
            msg.push_str(&format!(
                " Stored WhatsApp chats (id, message count): {}. WhatsApp reports some chats \
                 by LID (`...@lid`), so pass the id exactly as stored.",
                ids.join(", ")
            ));
        }
    }
    msg
}

#[async_trait]
impl Tool for WhatsAppHistoryTool {
    fn name(&self) -> &str {
        "whatsapp_history"
    }

    fn description(&self) -> &str {
        "Read-only search over a WhatsApp chat's stored history: imported \
         (offline-sync / recovered) and live-captured messages, newest first. \
         Needs the chat's id exactly as stored: a phone JID \
         ('5511999999999@s.whatsapp.net'), a LID ('210122852823201@lid' — \
         WhatsApp reports some chats this way), or a group id ending '@g.us'. \
         An empty result lists the stored chat ids so the right one can be \
         retried. Optional substring query, day window (max 90), and limit \
         (max 100)."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "chat": {
                    "type": "string",
                    "description": "Normalized WhatsApp chat id as stored (the JID string)."
                },
                "query": {
                    "type": "string",
                    "description": "Optional case-insensitive substring filter; omit to list the newest stored messages in the window."
                },
                "days": {
                    "type": "integer",
                    "description": "Look-back window in days, clamped to 1..=90 (default 90)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum rows returned, clamped to 1..=100 (default 50)."
                }
            },
            "required": ["chat"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![]
    }

    fn hints(&self) -> ToolHints {
        ToolHints {
            read_only: true,
            destructive: false,
            ..Default::default()
        }
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let chat = match input.get("chat").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required parameter 'chat'.".to_string(),
                ));
            }
        };
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let days = input.get("days").and_then(|v| v.as_i64()).unwrap_or(90);
        let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
        let (days, limit) = clamp_search(days, limit);
        let now = Utc::now();
        let since = (now - chrono::Duration::days(days)).timestamp();

        let rows = match self
            .repo
            .search_history("whatsapp", &chat, &query, since, limit)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult::error(format!("History search failed: {e}")));
            }
        };

        if rows.is_empty() {
            // Name the ids that DO have rows instead of dead-ending: WhatsApp
            // reports some chats by LID, so the phone-number form of an active
            // chat can legitimately miss (see `empty_history_hint`).
            let known = self
                .repo
                .list_chats(Some("whatsapp"))
                .await
                .ok()
                .map(|chats| {
                    chats
                        .into_iter()
                        .map(|c| (c.channel_chat_id, c.message_count))
                        .collect::<Vec<(String, i64)>>()
                });
            return Ok(ToolResult::success(empty_history_hint(
                &chat,
                days,
                &query,
                known.as_deref(),
            )));
        }

        let lines: Vec<String> = rows
            .iter()
            .map(|(content, sender, ts)| {
                let when = DateTime::from_timestamp(*ts, 0)
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| format!("{ts}"));
                format!("[{when}] {sender}: {content}")
            })
            .collect();
        Ok(ToolResult::success(format!(
            "{} stored message(s) for {chat}, newest first (window {days}d):\n{}",
            rows.len(),
            lines.join("\n")
        )))
    }
}
