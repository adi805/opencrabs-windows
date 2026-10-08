//! The bot's own presence (FR-003): the activity line Discord shows under the
//! bot's name while a turn runs, cleared again when the last one ends.
//!
//! This is the bot's *own* status, written with gateway opcode 3. It needs no
//! intent and no Developer Portal toggle: `GUILD_PRESENCES` is what a bot must
//! be granted to RECEIVE other members' presence updates, and it is
//! deliberately not requested here. Nothing in this module names
//! `GatewayIntents`, and `discord_presence_test` pins that, because requesting
//! one would turn a working feature into one that needs an owner-side toggle.
//!
//! Presence is one value for the whole bot, not one per channel, so the state is
//! a process-wide count of turns in flight rather than a flag: several channels
//! can run turns concurrently, and a flag would clear the activity when the
//! first of them finished while the rest were still working.
//!
//! Scope is the turn. A detached command keeps the typing dots alive after the
//! turn ends (see `typing`), but it does not hold this activity up: the turn is
//! the only boundary this channel sees on every path, including the error ones.

use std::sync::atomic::{AtomicUsize, Ordering};

use serenity::gateway::{ActivityData, ShardMessenger};

/// What Discord shows while a turn is running.
const WORKING: &str = "working";

/// Turns currently in flight across every channel of this process.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// The activity line for `in_flight` turns, or `None` to clear it.
///
/// Pure, so the mapping is tested without a gateway. `ActivityData` derives no
/// `PartialEq`, so a test that built one could not compare it anyway.
pub(crate) fn activity_text(in_flight: usize) -> Option<&'static str> {
    if in_flight == 0 { None } else { Some(WORKING) }
}

/// The activity to publish for the current count.
pub(crate) fn steady() -> Option<ActivityData> {
    activity_text(IN_FLIGHT.load(Ordering::SeqCst)).map(ActivityData::custom)
}

/// Holds one turn open for as long as it lives, reverting the activity when the
/// last one ends.
///
/// `Drop` rather than an explicit call, for the reason `typing::TypingGuard`
/// gives: the turn has many return paths, including the error ones, and only the
/// destructor covers all of them.
pub(crate) struct WorkingGuard {
    shard: ShardMessenger,
}

impl WorkingGuard {
    /// Count a turn as running and publish the activity.
    pub(crate) fn acquire(shard: ShardMessenger) -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        shard.set_activity(steady());
        Self { shard }
    }
}

impl Drop for WorkingGuard {
    fn drop(&mut self) {
        // The guard is neither `Clone` nor reusable, so this decrements exactly
        // once per `acquire` and cannot underflow into `usize::MAX`, which would
        // leave the bot advertising work for the rest of the process's life.
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        self.shard.set_activity(steady());
    }
}
