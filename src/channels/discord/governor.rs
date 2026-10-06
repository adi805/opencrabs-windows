//! Discord write-budget governor (PRD FR-003, #1888 follow-up).
//!
//! Discord rate-limits writes per channel. Before this module every Discord
//! write went straight to serenity, so a busy turn — a progress card refreshed
//! on every tick, tool-group churn, chunked answers — could burn the per-channel
//! budget and then keep hammering 429s with no pacing and no backoff at all.
//!
//! This is the Discord twin of [`crate::channels::telegram::governor`]: a
//! proactive token bucket per (channel, class) layered in front of every write,
//! plus a reactive cooldown that grows on consecutive 429s. It is deliberately
//! transport-free — it never touches serenity — so it can be unit-tested
//! against a virtual clock with zero real sleeps.
//!
//! Classes ([`WriteClass`]):
//! - `Create` — a brand new message (`.say`, `send_message`). Droppable.
//! - `Edit` — an in-place update of an existing message. Droppable: cosmetic
//!   refreshes self-heal on the next tick.
//! - `Final` — a settle render or the user-visible answer. NEVER dropped: the
//!   caller waits instead, bounded by `MAX_TOTAL_HOLD`.
//! - `Interactive` — the direct consequence of a user tap. NEVER dropped.
//!
//! Fail-open rules, both deliberate: a disabled governor and a non-positive
//! refill rate admit immediately rather than wedge a write path. A governor
//! that can hang the bot is worse than one that occasionally overshoots.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;

/// Absolute ceiling on how long ANY caller may be held by the governor.
/// Past this even a `Final` write fails open and attempts the request, so a
/// pathological config cannot wedge the channel forever.
const MAX_TOTAL_HOLD: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Virtual-clock seam
// ---------------------------------------------------------------------------

/// Virtual-clock offset used ONLY by `cfg(test)` builds ([`gate_now`]). Tests
/// advance it by hand to simulate refill spacing and cooldown expiry — mocked
/// passage of time, zero real sleeps anywhere. Production never reads it.
#[cfg(test)]
static CLOCK_OFFSET_MS: AtomicU64 = AtomicU64::new(0);

/// Wall-clock seam for ALL gate math. Production reads the real clock
/// unchanged; test builds add the monotonic offset so time is scripted.
#[cfg(not(test))]
pub(crate) fn gate_now() -> Instant {
    Instant::now()
}

/// [`gate_now`] — test build.
#[cfg(test)]
pub(crate) fn gate_now() -> Instant {
    let off = CLOCK_OFFSET_MS.load(Ordering::Relaxed);
    Instant::now()
        .checked_add(Duration::from_millis(off))
        .expect("virtual clock offset overflow")
}

/// Sleep for `d` — a real sleep in production, a clock jump in tests.
async fn sleep_for(d: Duration) {
    if d.is_zero() {
        return;
    }
    #[cfg(test)]
    test_support::advance(d.as_millis() as u64);
    #[cfg(test)]
    tokio::task::yield_now().await;
    #[cfg(not(test))]
    tokio::time::sleep(d).await;
}

// ---------------------------------------------------------------------------
// Classes and verdicts
// ---------------------------------------------------------------------------

/// What kind of write is asking for permission. Determines which bucket is
/// charged and whether the caller may be dropped under pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteClass {
    /// A brand new message. Cosmetic churn lives here, so it is droppable:
    /// the next event re-renders the full state and self-heals the gap.
    Create,
    /// An in-place update of an existing message. Droppable, same reasoning:
    /// every refresh re-renders FULL current state, so a dropped edit's
    /// content rides the next admitted one.
    Edit,
    /// A settle render, a content-bearing message, or the final answer.
    /// Never dropped: the caller waits for a token instead.
    Final,
}

impl WriteClass {
    /// Whether this class may be refused outright when the budget is spent.
    /// A dropped cosmetic write costs nothing: the next tick re-renders the
    /// FULL current state, so the dropped content rides the next admitted one.
    fn is_droppable(self) -> bool {
        matches!(self, WriteClass::Create | WriteClass::Edit)
    }
}

/// Outcome of a gate evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Go ahead and perform the write.
    Admit,
    /// Budget spent and this class is droppable: skip this write.
    Drop,
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Governor knobs, read fresh from config on every evaluation so a config
/// reload takes effect without a restart.
struct Limits {
    enabled: bool,
    create_capacity: u32,
    create_refill_per_sec: f64,
    edit_capacity: u32,
    edit_refill_per_sec: f64,
    max_hold: Duration,
    cooldown_base: Duration,
    cooldown_max: Duration,
}

impl Limits {
    fn from_config() -> Self {
        let c = &Config::current().channels.discord.rate_limiter;
        Self {
            enabled: c.enabled,
            create_capacity: c.create_burst,
            create_refill_per_sec: f64::from(c.creates_per_5s) / 5.0,
            edit_capacity: c.edit_burst,
            edit_refill_per_sec: f64::from(c.edits_per_5s) / 5.0,
            max_hold: Duration::from_secs(c.max_hold_secs),
            cooldown_base: Duration::from_millis(c.cooldown_base_millis),
            cooldown_max: Duration::from_millis(c.cooldown_max_millis),
        }
    }
}

// ---------------------------------------------------------------------------
// Token bucket
// ---------------------------------------------------------------------------

/// A classic token bucket. `take` is the consume path; the caller decides what
/// to do with the reported wait.
#[derive(Debug)]
pub(crate) struct Bucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            tokens: f64::from(capacity),
            capacity: f64::from(capacity),
            refill_per_sec,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.last_refill = now;
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * self.refill_per_sec).min(self.capacity);
    }

    /// Consume one token, or report how long until one is available.
    fn try_take(&mut self, now: Instant) -> Result<(), Duration> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64(
                (1.0 - self.tokens) / self.refill_per_sec,
            ))
        }
    }
}

/// Initialize or reshape a lazily-built bucket slot. A config change that moves
/// capacity or rate rebuilds the bucket fresh (burst restored) rather than
/// silently keeping the old shape for the life of the process.
fn ensure_bucket(
    slot: &mut Option<Bucket>,
    capacity: u32,
    refill_per_sec: f64,
    now: Instant,
) -> &mut Bucket {
    let stale = match slot {
        Some(b) => {
            b.capacity != f64::from(capacity)
                || (b.refill_per_sec - refill_per_sec).abs() > f64::EPSILON
        }
        None => true,
    };
    if stale {
        let mut b = Bucket::new(capacity, refill_per_sec);
        // Anchor the refill clock to the SAME seam the gate reads. `Bucket::new`
        // uses the real clock; under the test virtual clock that would credit a
        // spurious burst on the first take.
        b.last_refill = now;
        *slot = Some(b);
    }
    slot.as_mut().expect("bucket was just built")
}

// ---------------------------------------------------------------------------
// Per-target state
// ---------------------------------------------------------------------------

/// Everything the governor tracks for one channel.
#[derive(Debug)]
struct Target {
    create: Option<Bucket>,
    edit: Option<Bucket>,
    /// Set by [`record_429`]; writes wait this out before touching a bucket.
    cooldown_until: Option<Instant>,
    /// Consecutive 429s, driving the exponential cooldown. Reset on admit.
    consecutive_429: u32,
    creates: u64,
    edits: u64,
    dropped: u64,
    count_429: u64,
    throttled_ms: u64,
}

impl Target {
    fn new(_now: Instant) -> Self {
        Self {
            create: None,
            edit: None,
            cooldown_until: None,
            consecutive_429: 0,
            creates: 0,
            edits: 0,
            dropped: 0,
            count_429: 0,
            throttled_ms: 0,
        }
    }
}

fn targets() -> &'static Mutex<HashMap<u64, Target>> {
    static TARGETS: OnceLock<Mutex<HashMap<u64, Target>>> = OnceLock::new();
    TARGETS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read-only view of one channel's counters, for telemetry and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Snapshot {
    pub creates: u64,
    pub edits: u64,
    pub dropped: u64,
    pub count_429: u64,
    pub throttled_ms: u64,
}

/// Counters for `target`, or `None` if it has never been gated.
pub(crate) fn snapshot(target: u64) -> Option<Snapshot> {
    let map = targets().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&target).map(|t| Snapshot {
        creates: t.creates,
        edits: t.edits,
        dropped: t.dropped,
        count_429: t.count_429,
        throttled_ms: t.throttled_ms,
    })
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Ask permission to write to `target`. Waits out any active cooldown and any
/// spent bucket, and reports [`Admission::Drop`] only for a droppable class
/// whose wait would exceed the configured hold budget.
pub(crate) async fn admit(target: u64, class: WriteClass) -> Admission {
    let lim = Limits::from_config();
    if !lim.enabled {
        return Admission::Admit;
    }
    // A non-positive refill rate can never mint a token: admit rather than
    // divide by zero in `Bucket::try_take`, where `(1 - tokens) / 0.0` is
    // `inf` and `Duration::from_secs_f64(inf)` panics. The check is PER CLASS,
    // because the bucket is charged per class: a zero create budget must not
    // wedge edits, and a zero edit budget must not wedge creates. Fail open,
    // never wedge.
    let class_refill = match class {
        WriteClass::Create => lim.create_refill_per_sec,
        _ => lim.edit_refill_per_sec,
    };
    if class_refill <= 0.0 {
        return Admission::Admit;
    }

    let mut held = Duration::ZERO;

    // Phase A — reactive cooldown left by a previous 429.
    loop {
        let now = gate_now();
        let wait = {
            let mut map = targets().lock().unwrap_or_else(|e| e.into_inner());
            let t = map.entry(target).or_insert_with(|| Target::new(now));
            match t.cooldown_until {
                Some(until) if until > now => until.saturating_duration_since(now),
                _ => Duration::ZERO,
            }
        };
        if wait.is_zero() {
            break;
        }
        if class.is_droppable() && held + wait > lim.max_hold {
            note_drop(target);
            return Admission::Drop;
        }
        sleep_for(wait).await;
        held += wait;
    }

    // Phase B — proactive token bucket.
    loop {
        let now = gate_now();
        let verdict = {
            let mut map = targets().lock().unwrap_or_else(|e| e.into_inner());
            let t = map.entry(target).or_insert_with(|| Target::new(now));
            let (slot, cap, rate) = match class {
                WriteClass::Create => (
                    &mut t.create,
                    lim.create_capacity,
                    lim.create_refill_per_sec,
                ),
                _ => (&mut t.edit, lim.edit_capacity, lim.edit_refill_per_sec),
            };
            ensure_bucket(slot, cap, rate, now).try_take(now)
        };
        match verdict {
            Ok(()) => break,
            Err(wait) => {
                if held + wait > lim.max_hold {
                    if class.is_droppable() {
                        note_drop(target);
                        return Admission::Drop;
                    }
                    // Non-droppable: bounded by MAX_TOTAL_HOLD, then fail open
                    // so a pathological config cannot park a final write.
                    if held + wait > MAX_TOTAL_HOLD {
                        break;
                    }
                }
                sleep_for(wait).await;
                held += wait;
            }
        }
    }

    let mut map = targets().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = map.get_mut(&target) {
        match class {
            WriteClass::Create => t.creates += 1,
            _ => t.edits += 1,
        }
        t.throttled_ms = t.throttled_ms.saturating_add(held.as_millis() as u64);
        // A write that got through clears the backoff ladder.
        t.consecutive_429 = 0;
        t.cooldown_until = None;
    }
    Admission::Admit
}

fn note_drop(target: u64) {
    let mut map = targets().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = map.get_mut(&target) {
        t.dropped += 1;
    }
}

// ---------------------------------------------------------------------------
// Reactive 429 handling
// ---------------------------------------------------------------------------

/// True when the error text looks like a Discord rate-limit rejection.
pub(crate) fn is_rate_limited(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("429") || lower.contains("rate limited") || lower.contains("ratelimit")
}

/// Pull `retry_after` (seconds, fractional) out of a Discord 429 payload.
///
/// Discord reports the window in the JSON body as `"retry_after": 1.234`, and
/// serenity surfaces the body inside its error string. A missing or malformed
/// value returns `None` and the caller falls back to the exponential ladder.
pub(crate) fn parse_retry_after(error: &str) -> Option<Duration> {
    let idx = error.find("retry_after")?;
    let rest = &error[idx..];
    let colon = rest.find(':')?;
    let tail = rest[colon + 1..].trim_start();
    let num: String = tail
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let secs: f64 = num.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Record a 429 for `target`. The cooldown is the larger of Discord's own
/// `retry_after` and an exponential ladder, capped at `cooldown_max`, so a
/// burst of 429s backs off further each time instead of hammering.
pub(crate) fn record_429(target: u64, retry_after: Option<Duration>) {
    let lim = Limits::from_config();
    let now = gate_now();
    let mut map = targets().lock().unwrap_or_else(|e| e.into_inner());
    let t = map.entry(target).or_insert_with(|| Target::new(now));
    t.count_429 = t.count_429.saturating_add(1);
    t.consecutive_429 = t.consecutive_429.saturating_add(1);
    let shift = t.consecutive_429.saturating_sub(1).min(16);
    let exp = lim
        .cooldown_base
        .saturating_mul(1u32 << shift)
        .min(lim.cooldown_max);
    let wait = match retry_after {
        Some(d) => d.max(exp).min(lim.cooldown_max),
        None => exp,
    };
    t.cooldown_until = Some(now + wait);
}

/// Whether a cooldown is currently active for `target`.
///
/// Test-only: the gate itself reads `cooldown_until` directly, so this exists
/// purely as an observation seam for the suite. Production learns the same
/// thing from [`cooldown_remaining`], which the write path logs.
#[cfg(test)]
pub(crate) fn is_cooldown_active(target: u64) -> bool {
    let now = gate_now();
    let map = targets().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&target)
        .and_then(|t| t.cooldown_until)
        .is_some_and(|until| until > now)
}

/// Current cooldown remaining for `target`, if any.
pub(crate) fn cooldown_remaining(target: u64) -> Option<Duration> {
    let now = gate_now();
    let map = targets().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&target)
        .and_then(|t| t.cooldown_until)
        .filter(|until| *until > now)
        .map(|until| until.saturating_duration_since(now))
}

// ---------------------------------------------------------------------------
// Test support (cfg(test))
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Serialize every test that touches the shared registry or virtual clock.
    pub(crate) async fn registry_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        GUARD.lock().await
    }

    /// Wipe all target state and pin the virtual clock at `offset_ms`.
    pub(crate) fn reset(offset_ms: u64) {
        targets().lock().unwrap_or_else(|e| e.into_inner()).clear();
        CLOCK_OFFSET_MS.store(offset_ms, Ordering::Relaxed);
    }

    /// Advance the virtual clock. Bucket refills and cooldown expiry observe
    /// the jump on their next `gate_now` evaluation — no real sleeping.
    pub(crate) fn advance(ms: u64) {
        CLOCK_OFFSET_MS.fetch_add(ms, Ordering::Relaxed);
    }
}
