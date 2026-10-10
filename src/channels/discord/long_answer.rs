//! Long-answer paging (FR-009).
//!
//! Discord caps a message at 2000 characters. Posting a 6000-character answer
//! as three consecutive messages buries the channel and gives the reader no
//! way to navigate; the PRD asks for a summary first with the rest behind a
//! button (AC-020).
//!
//! The turn's already-chunked body is stored against the id of the message
//! that carries page 0, and a pager row is attached to it: `◀` and `▶`, one
//! per direction. Each button names its own target page, so the same row rides
//! page 0 and every page after it and a press simply renders the page the
//! button names — forward or back, no chain to walk.
//! Each press answers **ephemerally** with the requested page: the pager is a
//! read affordance, so it must not add more messages to the channel it exists
//! to keep clean.
//!
//! ## Component budget (AC-021)
//!
//! One Action Row, two Buttons. Discord allows 5 rows of 5 buttons per message
//! and 25 components in total, so the pager is nowhere near the cap by
//! construction — [`pager_row`] is the only builder and it emits exactly one
//! row. A test pins that shape rather than trusting the caller.

use std::collections::HashMap;

use serenity::builder::{CreateActionRow, CreateButton};
use serenity::model::application::ButtonStyle;

/// Insertion-ordered store of paged answers: the ids of the messages carrying
/// page 0, in insertion order, plus each answer's pages keyed by that id.
/// Named rather than inlined so the [`DiscordState`] field stays readable;
/// bounded by [`DiscordState::LONG_ANSWER_CAP`].
pub(super) type LongAnswerStore = (Vec<u64>, HashMap<u64, Vec<String>>);

/// `custom_id` prefix for a pager press: `longanswer:<message_id>:<page>`.
pub(crate) const PAGER_PREFIX: &str = "longanswer:";

/// Discord's per-message character ceiling.
pub(crate) const PAGE_CHARS: usize = 2000;

/// Headroom held back from [`PAGE_CHARS`] before an answer is split, so the
/// position footer [`page_body`] appends cannot push a page over Discord's
/// ceiling. A body that filled the split budget exactly plus
/// `\n\n-# Page 99 of 99` (18 bytes) is 2018 characters, and the API refuses
/// the whole response rather than trimming it, so the press would fail
/// silently. Subtracting this at split time is the only fix that never drops
/// the reader's text; clamping in `page_body` would.
pub(crate) const FOOTER_RESERVE: usize = 24;

/// FR-007 (AC-010): whether a finished answer should be delivered in a thread
/// instead of the channel.
///
/// Takes the answer length and the configured threshold, and deliberately NOT
/// the pager's page count. Gating this on "not paged" is the bug the function
/// exists to prevent: the pager claims every answer past [`PAGE_CHARS`], which
/// is exactly the set of answers a thread is for, so a `!paged` guard left the
/// thread reachable only in the narrow band between the threshold and the page
/// ceiling. The pager is the FALLBACK for a refused thread, not a precondition
/// for trying. `0` disables the feature.
pub(crate) fn wants_thread(auto_thread_min_chars: usize, answer_chars: usize) -> bool {
    auto_thread_min_chars > 0 && answer_chars >= auto_thread_min_chars
}

/// Label for one pager arrow. The two directions are the whole label: the
/// position moves to the body footer so the buttons stay narrow enough to sit
/// side by side in one row.
fn arrow_label(back: bool) -> &'static str {
    if back { "◀" } else { "▶" }
}

/// The Action Row that pages a long answer: one row, two arrows (AC-021).
///
/// Each button carries its OWN target page in its `custom_id`
/// (`longanswer:<message_id>:<target>`), so the click handler needs no
/// "which arrow was this" branch — it renders whatever page the id names. That
/// is also why the row is a pure function of `(message_id, page, total)`: the
/// same call draws the row on page 0 and on every later page, and only the
/// disabled ends differ.
///
/// An arrow that would leave the range is `disabled` rather than absent. A row
/// that dropped the dead direction would reflow between pages, and the reader
/// loses the one affordance that says "there is nothing further this way".
/// `Secondary` because paging is navigation, never a primary or destructive
/// action.
pub(crate) fn pager_row(message_id: u64, page: usize, total: usize) -> CreateActionRow {
    let last = total.saturating_sub(1);
    let button = |target: usize, back: bool, enabled: bool| {
        CreateButton::new(format!("{PAGER_PREFIX}{message_id}:{target}"))
            .label(arrow_label(back))
            .style(ButtonStyle::Secondary)
            .disabled(!enabled)
    };
    CreateActionRow::Buttons(vec![
        button(page.saturating_sub(1), true, page > 0),
        button((page + 1).min(last), false, page + 1 < total),
    ])
}

/// Body shown when a pager press arrives.
///
/// Every page carries its position as a FOOTER so an ephemeral reply pasted out
/// of context still says where it came from. The body leads: the reader already
/// pressed an arrow to get here, so a header would repeat what the buttons just
/// said and push the answer down. `-#` renders dim and small, which keeps the
/// footer from competing with the text it belongs to. Out-of-range pages return
/// `None` so the caller can answer "aged out" instead of showing an empty
/// bubble.
pub(crate) fn page_body(pages: &[String], page: usize) -> Option<String> {
    let body = pages.get(page)?;
    if pages.len() == 1 {
        return Some(body.clone());
    }
    let position = format!("-# Page {} of {}", page + 1, pages.len());
    Some(format!("{body}\n\n{position}"))
}

use super::DiscordState;

impl DiscordState {
    /// How many paged answers stay retrievable. Same lazy-aging contract as
    /// the tool groups: older entries stop being openable rather than being
    /// swept by a timer.
    pub(crate) const LONG_ANSWER_CAP: usize = 12;

    /// Retain the pages of a long answer against the message that shows page 0.
    pub(crate) async fn store_long_answer(&self, message_id: u64, pages: Vec<String>) {
        let mut guard = self.long_answers.lock().await;
        let (order, map) = &mut *guard;
        if !map.contains_key(&message_id) {
            order.push(message_id);
        }
        map.insert(message_id, pages);
        while order.len() > Self::LONG_ANSWER_CAP {
            let oldest = order.remove(0);
            map.remove(&oldest);
        }
    }

    /// Pages of a retained long answer, or `None` once it has aged out.
    pub(crate) async fn long_answer_pages(&self, message_id: u64) -> Option<Vec<String>> {
        self.long_answers.lock().await.1.get(&message_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-007 (AC-010): the thread decision is a function of the threshold and
    /// the answer length only, never of the pager's page count.
    #[test]
    fn thread_wants_an_answer_past_the_threshold() {
        assert!(wants_thread(1800, 1800), "at the threshold is enough");
        assert!(
            wants_thread(1800, 5000),
            "an answer past the page ceiling is exactly the one a thread is \
             for, and the old `!paged` guard made it unreachable"
        );
        assert!(
            !wants_thread(1800, 1799),
            "an answer below the threshold stays in place, so the feature \
             cannot fire on an ordinary reply"
        );
        assert!(!wants_thread(0, 10_000), "0 disables the feature");
    }

    /// The wire shape of every button in a pager row. `CreateButton`'s inner
    /// field is private, so this is the same bytes Discord receives.
    fn buttons_json(row: &CreateActionRow) -> Vec<serde_json::Value> {
        let CreateActionRow::Buttons(buttons) = row else {
            panic!("pager must be a button row");
        };
        buttons
            .iter()
            .map(|b| serde_json::to_value(b).expect("button serializes"))
            .collect()
    }

    #[test]
    fn pager_is_one_row_with_two_arrows() {
        let row = pager_row(42, 0, 3);
        assert!(matches!(row, CreateActionRow::Buttons(_)));
        let json = buttons_json(&row);
        assert_eq!(json.len(), 2, "AC-021: two arrows, one per direction");
        assert_eq!(json[0]["label"].as_str(), Some("◀"));
        assert_eq!(json[1]["label"].as_str(), Some("▶"));
    }

    /// FR-009: each arrow carries the page IT opens, so the handler renders
    /// whatever page the id names and never has to work out which arrow was
    /// pressed.
    #[test]
    fn each_arrow_names_its_own_target() {
        let json = buttons_json(&pager_row(7, 2, 4));
        assert_eq!(json[0]["custom_id"].as_str(), Some("longanswer:7:1"));
        assert_eq!(json[1]["custom_id"].as_str(), Some("longanswer:7:3"));
    }

    #[test]
    fn custom_id_round_trips_message_and_target() {
        let json = buttons_json(&pager_row(7, 2, 4));
        let id = json[0]["custom_id"].as_str().expect("custom_id on the wire");
        let rest = id.strip_prefix(PAGER_PREFIX).expect("prefix");
        let (mid, target) = rest.split_once(':').expect("two fields");
        assert_eq!(mid, "7");
        assert_eq!(target, "1");
    }

    /// The ends are `disabled`, not dropped: a row that lost the dead direction
    /// would reflow between pages, and the reader would lose the one affordance
    /// that says there is nothing further that way.
    #[test]
    fn the_ends_are_disabled_not_dropped() {
        let first = buttons_json(&pager_row(7, 0, 3));
        assert_eq!(first[0]["disabled"].as_bool(), Some(true), "◀ at the start");
        assert_eq!(first[1]["disabled"].as_bool(), Some(false));

        let last = buttons_json(&pager_row(7, 2, 3));
        assert_eq!(last[0]["disabled"].as_bool(), Some(false));
        assert_eq!(last[1]["disabled"].as_bool(), Some(true), "▶ at the end");
    }

    /// A middle page leaves both directions open, which is the case the whole
    /// two-arrow design exists for: the reader can go back.
    #[test]
    fn a_middle_page_leaves_both_directions_open() {
        let json = buttons_json(&pager_row(7, 1, 3));
        assert_eq!(json[0]["disabled"].as_bool(), Some(false));
        assert_eq!(json[1]["disabled"].as_bool(), Some(false));
    }

    /// The row is a pure function of `(message_id, page, total)`: the same call
    /// draws it on page 0 and on any later page, so nothing has to be carried
    /// forward between presses.
    #[test]
    fn the_same_row_serves_page_zero_and_later_pages() {
        let first = pager_row(7, 0, 3);
        let again = pager_row(7, 0, 3);
        assert_eq!(buttons_json(&first), buttons_json(&again));
    }

    #[test]
    fn out_of_range_page_is_none_not_empty() {
        let pages = vec!["a".to_string(), "b".to_string()];
        assert_eq!(page_body(&pages, 9), None);
        assert_eq!(page_body(&[], 0), None);
    }

    #[test]
    fn single_page_body_is_not_labelled() {
        let pages = vec!["only".to_string()];
        assert_eq!(page_body(&pages, 0), Some("only".to_string()));
    }

    /// The position rides as a footer, so the answer leads and the dim `-#`
    /// line trails it instead of pushing it down.
    #[test]
    fn multi_page_body_carries_its_position_as_a_footer() {
        let pages = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            page_body(&pages, 1),
            Some("b\n\n-# Page 2 of 2".to_string())
        );
    }

    /// A page that filled the split budget must still fit the wire once its
    /// footer is added, or the press fails on exactly the page that is full.
    /// The split budget holds [`FOOTER_RESERVE`] back for this; the test pins
    /// the invariant from the reader's end so a later change to the footer
    /// text cannot quietly outgrow the headroom.
    #[test]
    fn a_full_page_plus_its_footer_still_fits_the_wire_cap() {
        let pages = vec![
            "x".repeat(PAGE_CHARS - FOOTER_RESERVE),
            "second".to_string(),
        ];
        let body = page_body(&pages, 0).expect("page 0 exists");
        assert!(
            body.len() <= PAGE_CHARS,
            "a full page plus its footer is {} bytes, past Discord's {PAGE_CHARS} \
             ceiling, so the press would be rejected outright",
            body.len()
        );
        assert!(
            body.ends_with("-# Page 1 of 2"),
            "the footer still rides the full page"
        );
    }
}
