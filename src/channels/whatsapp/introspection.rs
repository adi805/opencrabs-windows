//! Runtime introspection and tuning for the live client.
//!
//! Every knob here is a method on the WhatsApp client that a rebuild used to be
//! the only way to reach. Wired through [`WhatsAppState`] they are callable
//! against the running session: the counters answer "is this session healthy",
//! the waiters answer "did the stanza I expected actually go out", and the
//! setters retune pacing without a reconnect.
//!
//! Each wrapper answers `None` when there is no connected client rather than
//! failing: asking a disconnected bot for its counters is a normal answer, not
//! a fault.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wacore_binary::jid::Jid;
use whatsapp_rust::{NodeFilter, RawNodeLease, RetryAdmission, StatsSnapshot};

use super::WhatsAppState;

/// How long a node waiter blocks before it reports that nothing arrived.
///
/// A waiter holds a slot in the client's waiter list until it is dropped, and a
/// caller cannot tell "no node yet" from "no node ever" without a bound, so
/// every wait is capped.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on the per-chat buckets a [`RetryBucket`] remembers.
///
/// A bucket that has fully refilled carries no state worth keeping, so a table
/// past this size drops the ones already back at full rather than growing with
/// every chat the bot has ever seen.
const MAX_TRACKED_CHATS: usize = 4096;

impl WhatsAppState {
    /// Start forwarding raw decoded stanzas to this process, and hold the lease
    /// for as long as the session lives.
    ///
    /// `Event::RawNode` stays enabled only while at least one lease is held, so
    /// the lease is stored rather than dropped: letting it fall out of scope
    /// here would switch forwarding off again the moment this call returned.
    ///
    /// Returns whether a lease was already held, so the caller can say that the
    /// call was a no-op instead of reporting a fresh acquisition.
    pub async fn acquire_raw_node_forwarding(&self) -> Option<bool> {
        let client = self.client().await?;
        let lease: RawNodeLease = client.acquire_raw_node_forwarding();
        let mut slot = self
            .raw_node_lease
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let already_held = slot.is_some();
        *slot = Some(lease);
        Some(already_held)
    }

    /// Drop the raw-stanza lease, which stops forwarding once no other holder
    /// remains. Returns whether there was a lease to drop.
    pub fn release_raw_node_forwarding(&self) -> bool {
        let mut slot = self
            .raw_node_lease
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        slot.take().is_some()
    }

    /// Cumulative wire I/O and activity counters for the live session.
    pub async fn client_stats(&self) -> Option<StatsSnapshot> {
        Some(self.client().await?.stats())
    }

    /// Wait for the next inbound node whose tag matches, optionally narrowed by
    /// one attribute.
    ///
    /// The node is rendered with `Debug` rather than reduced to a summary: this
    /// is a diagnostic surface, and a summary that dropped the field the caller
    /// was looking for would send them back to a rebuild, which is the thing
    /// this module exists to avoid.
    pub async fn wait_for_node(
        &self,
        tag: &str,
        attr: Option<(&str, &str)>,
        timeout: Duration,
    ) -> Option<String> {
        let client = self.client().await?;
        let rx = client.wait_for_node(build_filter(tag, attr));
        Some(describe_wait(
            timeout,
            tokio::time::timeout(timeout, rx).await,
            "arrived",
        ))
    }

    /// Wait for the next outbound node whose tag matches, captured before it is
    /// encrypted and sent.
    pub async fn wait_for_sent_node(
        &self,
        tag: &str,
        attr: Option<(&str, &str)>,
        timeout: Duration,
    ) -> Option<String> {
        let client = self.client().await?;
        let rx = client.wait_for_sent_node(build_filter(tag, attr));
        Some(describe_wait(
            timeout,
            tokio::time::timeout(timeout, rx).await,
            "sent",
        ))
    }

    /// Retune the per-chat resend bucket without a reconnect. `burst` of `0`
    /// disables the limiter.
    pub async fn set_resend_rate_limit(&self, burst: u32, refill_per_min: u32) -> bool {
        match self.client().await {
            Some(client) => {
                client.set_resend_rate_limit(burst, refill_per_min);
                true
            }
            None => false,
        }
    }

    /// Turn history-sync skipping on or off at runtime. While on, the client
    /// acknowledges history-sync notifications without downloading them.
    pub async fn set_skip_history_sync(&self, enabled: bool) -> bool {
        match self.client().await {
            Some(client) => {
                client.set_skip_history_sync(enabled);
                true
            }
            None => false,
        }
    }

    /// Set how many one-time pre-keys are generated per upload batch. Takes
    /// effect on the next pre-key upload.
    pub async fn set_wanted_pre_key_count(&self, count: usize) -> bool {
        match self.client().await {
            Some(client) => {
                client.set_wanted_pre_key_count(count);
                true
            }
            None => false,
        }
    }

    /// Register the retry-admission policy.
    ///
    /// The client accepts one policy per session and ignores a second
    /// registration, so a `false` here means a policy was already in place and
    /// this one was dropped. The caller has to say so rather than report
    /// success, because the operator would otherwise believe a new limit was
    /// live when the old one still is.
    pub async fn set_retry_admission(&self, policy: Arc<dyn RetryAdmission>) -> Option<bool> {
        Some(self.client().await?.set_retry_admission(policy))
    }
}

/// Build the node filter, narrowing by one attribute only when one was given.
///
/// A filter with no attribute matches every node carrying the tag, which is
/// what a caller wants when they are asking "did anything of this shape go out
/// at all".
fn build_filter(tag: &str, attr: Option<(&str, &str)>) -> NodeFilter {
    let filter = NodeFilter::tag(tag);
    match attr {
        Some((key, value)) => filter.attr(key, value),
        None => filter,
    }
}

/// Render the outcome of a bounded wait.
///
/// Three outcomes are kept distinct on purpose: a node arrived, the waiter was
/// dropped (the client shut down under it), or the bound expired. Collapsing
/// the last two into "nothing" would hide a session that is going away behind
/// the same answer as a session that is simply quiet.
fn describe_wait<T: std::fmt::Debug>(
    timeout: Duration,
    outcome: Result<Result<T, futures::channel::oneshot::Canceled>, tokio::time::error::Elapsed>,
    verb: &str,
) -> String {
    match outcome {
        Ok(Ok(node)) => format!("{node:?}"),
        Ok(Err(_)) => "the client dropped the waiter before a matching node was".to_string(),
        Err(_) => format!(
            "no matching node was {verb} within {}ms",
            timeout.as_millis()
        ),
    }
}

/// A per-chat token-bucket [`RetryAdmission`] policy.
///
/// In a large group, a cohort of members whose pairwise sessions never
/// establish can send a retry for every message, driving the repair path at
/// storm rate. This bounds each chat's share of that work: once a chat is over
/// budget its retries are dropped before any repair runs. The requester
/// re-requests on its own timer and the bucket refills, so genuine recovery
/// stays possible.
///
/// Keyed per chat on purpose, so one broken cohort cannot spend another chat's
/// budget. The decision is a single mutex-guarded map operation with no I/O,
/// which is what the receive path requires of it.
pub struct RetryBucket {
    burst: u32,
    refill_per_min: u32,
    chats: Mutex<HashMap<String, Bucket>>,
}

struct Bucket {
    tokens: f64,
    last_ms: u64,
}

impl RetryBucket {
    /// `burst` is the instantaneous allowance per chat; `0` admits everything,
    /// the same off switch the resend limiter spells with `0`.
    pub fn new(burst: u32, refill_per_min: u32) -> Self {
        Self {
            burst,
            refill_per_min,
            chats: Mutex::new(HashMap::new()),
        }
    }
}

/// Milliseconds since the epoch, or `0` if the clock is before it.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl RetryAdmission for RetryBucket {
    fn admit(&self, chat: &Jid, _requester: &Jid, _retry_count: u8) -> bool {
        if self.burst == 0 {
            return true;
        }
        let now = now_ms();
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        if chats.len() > MAX_TRACKED_CHATS {
            let ceiling = f64::from(self.burst);
            chats.retain(|_, bucket| bucket.tokens < ceiling);
        }
        let bucket = chats.entry(chat.to_string()).or_insert(Bucket {
            tokens: f64::from(self.burst),
            last_ms: now,
        });
        let elapsed_minutes = now.saturating_sub(bucket.last_ms) as f64 / 60_000.0;
        let ceiling = f64::from(self.burst);
        bucket.tokens =
            (bucket.tokens + elapsed_minutes * f64::from(self.refill_per_min)).min(ceiling);
        bucket.last_ms = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
