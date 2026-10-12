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
use whatsapp_rust::features::MexRequest;
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

/// Intern a MEX op name or doc id for the process lifetime.
///
/// `MexRequest::new` takes `&'static str` because every generated call site
/// names a compile-time constant, and a runtime-supplied op has to be leaked to
/// satisfy that. Caching the strings keeps the leak bounded by the number of
/// distinct ops a caller actually uses: repeating an op reuses its entry
/// instead of growing the heap on every call.
fn intern_mex_str(value: &str) -> &'static str {
    static INTERNED: std::sync::OnceLock<Mutex<HashMap<String, &'static str>>> =
        std::sync::OnceLock::new();
    let table = INTERNED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = table.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(value) {
        return existing;
    }
    let leaked: &'static str = Box::leak(value.to_string().into_boxed_str());
    guard.insert(value.to_string(), leaked);
    leaked
}

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

    /// This device's LID, phone JID and push name, as the session reports them.
    ///
    /// Read straight off the device snapshot rather than inferred: a bot whose
    /// LID is unset looks the same as one whose LID is wrong from the outside,
    /// and only the raw values tell them apart.
    pub async fn identity_summary(&self) -> Option<String> {
        let client = self.client().await?;
        Some(format!(
            "account identity:\n- lid: {}\n- pn: {}\n- push name: {}",
            match client.lid() {
                Some(j) => j.to_string(),
                None => "not set (pairing incomplete?)".to_string(),
            },
            match client.pn() {
                Some(j) => j.to_string(),
                None => "not set (pairing incomplete?)".to_string(),
            },
            client.push_name()
        ))
    }

    /// The identity tags the tracing layer uses on spans and errors.
    ///
    /// Reported alongside the raw identity because these are what a span
    /// carries: a redacted PN here against a full PN above means the redaction
    /// path is doing something the caller did not expect.
    pub async fn identity_tags_summary(&self) -> Option<String> {
        let client = self.client().await?;
        Some(format!("span identity tags: {:?}", client.identity_tags()))
    }

    /// In-process collection footprint for this session.
    pub async fn memory_report_summary(&self) -> Option<String> {
        let client = self.client().await?;
        Some(format!("{:#?}", client.memory_report().await))
    }

    /// The same footprint plus storage, transport and HTTP components.
    pub async fn resource_report_summary(&self) -> Option<String> {
        let client = self.client().await?;
        Some(format!("{:#?}", client.resource_report().await))
    }

    /// Whether history sync is currently being skipped.
    ///
    /// Distinct from the setter's return value: the setter reports whether the
    /// call was accepted, this reports the state the client is actually in.
    pub async fn skip_history_sync_enabled(&self) -> Option<bool> {
        Some(self.client().await?.skip_history_sync_enabled())
    }

    /// The pre-key count the client targets for its next upload batch.
    pub async fn wanted_pre_key_count(&self) -> Option<usize> {
        Some(self.client().await?.wanted_pre_key_count())
    }

    /// Run a MEX GraphQL query by op name and document id.
    ///
    /// Both halves are required and passed through as given: the persisted
    /// query is identified by the pair, and a wrong id fails at the server with
    /// nothing to point at, so the caller names the operation rather than this
    /// wrapper guessing one.
    pub async fn mex_query(
        &self,
        name: &str,
        doc_id: &str,
        variables: serde_json::Value,
    ) -> Option<std::result::Result<String, String>> {
        let client = self.client().await?;
        let request = MexRequest::new(intern_mex_str(name), intern_mex_str(doc_id), variables);
        Some(match client.mex().query(request).await {
            Ok(response) => Ok(format!("{response:#?}")),
            Err(e) => Err(e.to_string()),
        })
    }

    /// Run a MEX GraphQL mutation. See [`Self::mex_query`].
    pub async fn mex_mutate(
        &self,
        name: &str,
        doc_id: &str,
        variables: serde_json::Value,
    ) -> Option<std::result::Result<String, String>> {
        let client = self.client().await?;
        let request = MexRequest::new(intern_mex_str(name), intern_mex_str(doc_id), variables);
        Some(match client.mex().mutate(request).await {
            Ok(response) => Ok(format!("{response:#?}")),
            Err(e) => Err(e.to_string()),
        })
    }

    /// The account's current reachout-timelock state.
    ///
    /// A timelock restricts who the account may message first; reading it is
    /// how a caller finds out that an outreach failure was a policy limit
    /// rather than a bad number.
    pub async fn reachout_timelock_summary(&self) -> Option<std::result::Result<String, String>> {
        let client = self.client().await?;
        Some(match client.mex().fetch_reachout_timelock().await {
            Ok(lock) => Ok(format!(
                "reachout timelock:\n- active: {}\n- enforcement type: {}\n- ends: {}",
                match lock.is_active {
                    Some(b) => b.to_string(),
                    None => "not reported".to_string(),
                },
                lock.enforcement_type.as_deref().unwrap_or("not reported"),
                lock.time_enforcement_ends
                    .as_deref()
                    .unwrap_or("not reported")
            )),
            Err(e) => Err(e.to_string()),
        })
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

    /// Whether the transport is connected right now.
    ///
    /// Deliberately NOT the same question as [`Self::is_logged_in`]: a client
    /// can hold the socket open and still not have completed the login
    /// handshake. Reading one as the other reports a broken session as
    /// healthy, which is the failure this pair exists to separate.
    pub async fn client_is_connected(&self) -> Option<bool> {
        Some(self.client().await?.is_connected())
    }

    /// Whether the login handshake has completed for this device.
    pub async fn is_logged_in(&self) -> Option<bool> {
        Some(self.client().await?.is_logged_in())
    }

    /// Wait until the session is fully ready, or until the timeout expires.
    ///
    /// Stricter than [`Self::client_is_connected`] on purpose: the library's readiness
    /// wait means connected AND logged in AND critical sync done. Work started
    /// against a merely-connected session gets an IQ that cannot be answered
    /// yet, so the caller wants this one, not the flag.
    pub async fn wait_for_connected(&self, timeout: Duration) -> Option<Result<(), String>> {
        let client = self.client().await?;
        Some(
            client
                .wait_for_connected(timeout)
                .await
                .map_err(|e| e.to_string()),
        )
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
