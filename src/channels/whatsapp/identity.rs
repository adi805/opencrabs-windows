//! Which of a sender's two addresses to key on (#1533).
//!
//! WhatsApp addresses a message either by phone number (PN, `@s.whatsapp.net`)
//! or by an opaque privacy id (LID, `@lid`), and the parser hands back the
//! OTHER one in `sender_alt`. From `wacore/src/messages.rs:539`:
//!
//! ```text
//! let sender_alt = if participant.server.is_pn_family() {
//!     attrs.optional_jid("participant_lid")   // sender is PN, alt is the LID
//! } else if participant.server.is_lid_family() {
//!     attrs.optional_jid("participant_pn")    // sender is LID, alt is the PN
//! }
//! ```
//!
//! So `sender_alt` is whichever identity the sender is NOT. That makes
//! "prefer `sender_alt`" a trap: it reads as "prefer the phone number" and is
//! only that for a LID-addressed sender. For a PN-addressed one it prefers the
//! LID, which is the opposite of the intent, and it looks correct in any test
//! where the sender happens to arrive as a LID.
//!
//! Anything used as a MAP KEY has to pick one of the pair and pick the same one
//! every time, or a contact whose addressing mode changes between two stanzas
//! gets two entries: two sessions, an approval that can never be resolved, a
//! photo album that never batches. The rule below keys on the server family
//! rather than on which field the value arrived in.

use wacore::store::traits::ProtocolStore;
use wacore_binary::jid::Jid;

/// The sender's phone-number identity when the pair has one, else the sender.
///
/// A PN is preferred because it is the identity everything else in the channel
/// is written in: `allowed_phones`, `bot_owner`, the blocklist, and the session
/// label a human reads. A LID is only ever a fallback for a sender who has no
/// PN twin in the stanza, where the choice is that LID or nothing.
///
/// Both families are asked rather than one being assumed as the negation of the
/// other: `is_pn_family` covers `Pn | Hosted` and `is_lid_family` covers
/// `Lid | HostedLid`, so a two-branch `if/else` on either one alone sorts the
/// hosted variants into the wrong half. Servers in neither family (group,
/// broadcast, newsletter) are not identity pairs at all and keep their own user
/// part.
///
/// Takes the user part only, never the device: `Jid::user` already excludes the
/// `:34` linked-device suffix, so the same person on phone and desktop keys the
/// same way.
pub(crate) fn canonical_user(sender: &Jid, sender_alt: Option<&Jid>) -> String {
    if sender.server.is_pn_family() {
        return sender.user.to_string();
    }
    if sender.server.is_lid_family()
        && let Some(alt) = sender_alt
        && alt.server.is_pn_family()
        && !alt.user.is_empty()
    {
        return alt.user.to_string();
    }
    sender.user.to_string()
}

/// The sender's phone number, asked of the Signal store when the stanza did not
/// carry it (#2183).
///
/// [`canonical_user`] above can only use what the message arrived with. WhatsApp
/// is not obliged to include the PN twin on a LID-addressed stanza, and when it
/// does not, every downstream reader sees digits that are not a phone number:
/// the owner gate compares an opaque id against `bot_owner`/`allowed_phones` and
/// answers "not the owner", so the owner's own DM is handled as a stranger, and
/// the session is filed under a second key until the next boot's reconciliation
/// folds it back (#2178).
///
/// The pair is not a guess. The Signal store learns it and `wa_lid_pn_mapping`
/// is authoritative, which is the same source `lid_migration` uses to reconcile
/// session keys. This asks it once, before anything keys on the identity.
///
/// Returns `None` in three cases, all of which mean "keep today's behaviour":
/// the sender already has a PN, the sender is not a LID at all (group,
/// broadcast, newsletter are not identity pairs), or the mapping is not known
/// yet. Availability is never invented: an unmapped LID stays unmapped until the
/// store learns the pair.
pub(crate) async fn resolve_pn(
    store: Option<&super::store::Store>,
    sender: &Jid,
    sender_alt: Option<&Jid>,
) -> Option<String> {
    // A PN sender needs no resolution, and a stanza that already carries the
    // twin is `canonical_user`'s job.
    if !sender.server.is_lid_family() {
        return None;
    }
    if let Some(alt) = sender_alt
        && alt.server.is_pn_family()
        && !alt.user.is_empty()
    {
        return None;
    }
    let store = store?;
    match store.get_lid_mapping(&sender.user).await {
        Ok(Some(entry)) if !entry.phone_number.is_empty() => Some(entry.phone_number),
        Ok(_) => {
            // The unhappy path that used to be silent: an unmapped LID means
            // this turn is handled as a non-owner. One line here is what turns
            // the next occurrence into a diagnosis instead of a feeling.
            tracing::debug!(
                target: "whatsapp",
                lid = %sender.user,
                "no LID→PN mapping for this sender; the turn is handled as a non-owner"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                target: "whatsapp",
                lid = %sender.user,
                "LID→PN lookup failed, falling back to the raw sender id: {e}"
            );
            None
        }
    }
}
