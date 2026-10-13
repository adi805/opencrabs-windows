//! Announcement loop guard (#957, extended by #961) — the text layer.
//!
//! Second half of the Luna fix: a per-session ring buffer of outgoing
//! assistant texts. The tool layer catches the near-identical `bash`
//! echoes; this layer catches the reworded announcements that each land
//! as a separate, internally clean turn, so every within-turn guard
//! (#507, phantom intent-loop, #740, #788) misses them. Since #961 the
//! ring also sees intermediate (between-tool-calls) text, catching the
//! DeepSeek v4 flash pattern of re-announcing the same pending action
//! inside a single turn.
//!
//! Detect and surface, don't prevent (the #954 guard philosophy): the
//! first trip still delivers the text plus a system nudge; only a second
//! trip with the nudge ignored escalates to an abort through the existing
//! repetition -> loop-message path (`format_user_error`).

use std::collections::{HashSet, VecDeque};

use super::helpers::normalize_loop_text;

/// Outgoing texts remembered per session (#957, raised 5 -> 8 for #961:
/// cap 5 evicted the early duplicates of the DeepSeek v4 flash zip-send
/// pattern before the ring could ever trip).
pub const TEXT_LOOP_RING_CAP: usize = 8;
/// Near-duplicate hits within the ring that trip the guard (#957).
pub const TEXT_LOOP_TRIP_AT: usize = 3;
/// Jaccard similarity threshold on normalized word sets (#957).
const NEAR_DUPLICATE_JACCARD: f64 = 0.8;
/// Overlap-coefficient threshold on normalized word sets (#961): a short
/// reworded announcement whose words are almost all contained in a
/// longer one still counts as a near-duplicate even when Jaccard is
/// dragged down by the length difference ("Sending the zip:" vs "Sending
/// the zip to this thread now:").
const NEAR_DUPLICATE_OVERLAP: f64 = 0.8;
/// Below this many normalized words, word-set ratios are too coarse —
/// only exact normalized equality counts.
const NEAR_DUPLICATE_MIN_WORDS: usize = 3;

/// Near-duplicate check for loop detection (#957, extended for #961).
///
/// Jaccard similarity >= 0.8 on normalized word sets, plus an
/// overlap-coefficient clause (>= 0.8 of the SMALLER word set shared)
/// for reworded pairs where one text is much shorter than the other.
/// Equality fallback: short texts (< 3 normalized words) only count as
/// duplicates when their normalized forms are identical, because word-set
/// ratios are too coarse below a handful of words.
pub fn near_duplicate(a: &str, b: &str) -> bool {
    let na = normalize_loop_text(a);
    let nb = normalize_loop_text(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    match similarity_scores(&na, &nb) {
        Some((jaccard, overlap)) => {
            jaccard >= NEAR_DUPLICATE_JACCARD || overlap >= NEAR_DUPLICATE_OVERLAP
        }
        None => false,
    }
}

/// The two word-set scores behind [`near_duplicate`], as `(jaccard, overlap)`
/// on normalized text. `None` when either side is too short to compare.
/// Shared with the trip log so the logged scores are the decided ones (#2052).
pub(crate) fn similarity_scores(na: &str, nb: &str) -> Option<(f64, f64)> {
    let wa: HashSet<&str> = na.split(' ').collect();
    let wb: HashSet<&str> = nb.split(' ').collect();
    if wa.len() < NEAR_DUPLICATE_MIN_WORDS || wb.len() < NEAR_DUPLICATE_MIN_WORDS {
        return None;
    }
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    let min_len = wa.len().min(wb.len()) as f64;
    let jaccard = if union > 0.0 { inter / union } else { 0.0 };
    let overlap = if min_len > 0.0 { inter / min_len } else { 0.0 };
    Some((jaccard, overlap))
}

/// Longest excerpt of a text the trip log carries. The full text stays in the
/// session's messages; the log only needs enough to see what repeated.
const TRIP_LOG_EXCERPT_CHARS: usize = 240;

/// Debug-log every ring entry that matched a tripping text, with the scores
/// that decided it (#2052). Debug only: no effect on the guard's decision.
fn log_trip_pairs(text: &str, ring: &VecDeque<String>) {
    let current = normalize_loop_text(text);
    for prior in ring.iter() {
        let prior_norm = normalize_loop_text(prior);
        if !near_duplicate(text, prior) {
            continue;
        }
        let (jaccard, overlap) = if current == prior_norm {
            (1.0, 1.0)
        } else {
            similarity_scores(&current, &prior_norm).unwrap_or((0.0, 0.0))
        };
        tracing::debug!(
            current_words = current.split(' ').count(),
            prior_words = prior_norm.split(' ').count(),
            exact = current == prior_norm,
            jaccard = format!("{jaccard:.2}"),
            overlap = format!("{overlap:.2}"),
            current_text = %excerpt(text),
            prior_text = %excerpt(prior),
            "announcement_loop: near-duplicate pair at trip"
        );
    }
}

fn excerpt(text: &str) -> String {
    let mut out: String = text.chars().take(TRIP_LOG_EXCERPT_CHARS).collect();
    if text.chars().count() > TRIP_LOG_EXCERPT_CHARS {
        out.push('…');
    }
    out
}

/// Outcome of checking an outgoing text against the session ring (#957).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLoopAction {
    /// No loop signal; deliver normally.
    Continue,
    /// First trip: deliver, but surface a system nudge for the next turn.
    Nudge,
    /// Second trip after an ignored nudge: abort through the loop-message
    /// path.
    Abort,
}

/// Per-session ring buffer of outgoing assistant texts (#957).
///
/// Lives on `AgentService` keyed by session id — NOT in the per-turn
/// context, which is reloaded from the DB every turn, so a loop that
/// spans turns needs state that outlives a single context load. Restart
/// re-arms the guard: same accepted semantics as #507's in-loop flags.
#[derive(Debug, Default)]
pub struct OutgoingTextRing {
    ring: VecDeque<String>,
    nudged: bool,
}

impl OutgoingTextRing {
    /// Record an outgoing text, then check it against the ring.
    ///
    /// The text is stored FIRST, so it counts as one hit against itself:
    /// three near-identical outgoing texts within the ring trip the guard.
    pub fn record_and_check(&mut self, text: &str) -> TextLoopAction {
        self.ring.push_back(text.to_string());
        while self.ring.len() > TEXT_LOOP_RING_CAP {
            self.ring.pop_front();
        }
        let hits = self.ring.iter().filter(|t| near_duplicate(text, t)).count();
        if hits >= TEXT_LOOP_TRIP_AT {
            log_trip_pairs(text, &self.ring);
            if self.nudged {
                TextLoopAction::Abort
            } else {
                self.nudged = true;
                TextLoopAction::Nudge
            }
        } else {
            TextLoopAction::Continue
        }
    }

    /// Peek at the most recently recorded text (#961). Lets a later hook
    /// skip re-recording text an earlier hook already judged, so one
    /// outgoing text can never double-count toward the trip threshold.
    pub fn last_recorded(&self) -> Option<&str> {
        self.ring.back().map(String::as_str)
    }
}
