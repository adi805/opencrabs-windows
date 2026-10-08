//! Long-answer paging (FR-009).
//!
//! Discord caps a message at 2000 characters. Posting a 6000-character answer
//! as three consecutive messages buries the channel and gives the reader no
//! way to navigate; the PRD asks for a summary first with the rest behind a
//! button (AC-020).
//!
//! The turn's already-chunked body is stored against the id of the message
//! that carries page 0, and a single pager row is attached to it. That row
//! opens the first HIDDEN page (index 1), and every answer carries the row for
//! the page after it, so the remainder stays reachable one press at a time.
//! Each press answers **ephemerally** with the requested page: the pager is a
//! read affordance, so it must not add more messages to the channel it exists
//! to keep clean.
//!
//! ## Component budget (AC-021)
//!
//! One Action Row, one Button. Discord allows 5 rows of 5 buttons per message
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

/// Label for the button that reveals page `page` (1-based in prose).
fn button_label(page: usize, total: usize) -> String {
    format!("Page {} of {}", page + 1, total)
}

/// The single Action Row attached to page 0 of a long answer.
///
/// Exactly one row with one button — the AC-021 shape. The button is
/// `Secondary` because paging is navigation, not a destructive or primary
/// action.
pub(crate) fn pager_row(message_id: u64, page: usize, total: usize) -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{PAGER_PREFIX}{message_id}:{page}"))
            .label(button_label(page, total))
            .style(ButtonStyle::Secondary),
    ])
}

/// The row that keeps the page after `page` reachable, or `None` once the last
/// page has been shown.
///
/// Page 0 posts in-channel, so every later page is visible only through a
/// press. A reply that carried no row would leave the pages after it stored but
/// unreachable, which is what FR-009's "the remainder sits behind a button"
/// forbids. The caller attaches this to the ephemeral answer for `page`, which
/// walks the reader through the remainder one press at a time.
pub(crate) fn next_page_row(message_id: u64, page: usize, total: usize) -> Option<CreateActionRow> {
    if page + 1 < total {
        Some(pager_row(message_id, page + 1, total))
    } else {
        None
    }
}

/// Body shown when a pager press arrives.
///
/// Every page is prefixed with its position so an ephemeral reply pasted out
/// of context still says where it came from. Out-of-range pages return `None`
/// so the caller can answer "aged out" instead of showing an empty bubble.
pub(crate) fn page_body(pages: &[String], page: usize) -> Option<String> {
    let body = pages.get(page)?;
    if pages.len() == 1 {
        return Some(body.clone());
    }
    Some(format!(
        "**Page {} of {}**\n\n{body}",
        page + 1,
        pages.len()
    ))
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

    /// `CreateButton`'s inner field is private, so read the wire shape it
    /// actually serializes to — the same bytes Discord receives.
    fn button_json(row: &CreateActionRow) -> serde_json::Value {
        let CreateActionRow::Buttons(buttons) = row else {
            panic!("pager must be a button row");
        };
        assert_eq!(buttons.len(), 1, "AC-021: one button, not a wall of them");
        serde_json::to_value(&buttons[0]).expect("button serializes")
    }

    #[test]
    fn pager_is_one_row_with_one_button() {
        // `button_json` asserts the row shape; this pins the row count.
        let row = pager_row(42, 0, 3);
        assert!(matches!(row, CreateActionRow::Buttons(_)));
        let _ = button_json(&row);
    }

    #[test]
    fn custom_id_round_trips_message_and_page() {
        let json = button_json(&pager_row(7, 2, 4));
        let id = json["custom_id"].as_str().expect("custom_id on the wire");
        assert_eq!(id, "longanswer:7:2");
        let rest = id.strip_prefix(PAGER_PREFIX).expect("prefix");
        let (mid, page) = rest.split_once(':').expect("two fields");
        assert_eq!(mid, "7");
        assert_eq!(page, "2");
    }

    #[test]
    fn button_label_names_the_page_and_total() {
        let json = button_json(&pager_row(7, 1, 3));
        assert_eq!(json["label"].as_str(), Some("Page 2 of 3"));
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

    #[test]
    fn multi_page_body_carries_its_position() {
        let pages = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            page_body(&pages, 1),
            Some("**Page 2 of 2**\n\nb".to_string())
        );
    }

    /// The `custom_id` of the row's single button, read off the wire shape.
    fn row_custom_id(row: &CreateActionRow) -> String {
        button_json(row)["custom_id"]
            .as_str()
            .expect("custom_id on the wire")
            .to_string()
    }

    /// The page index a pager row opens.
    fn row_page(row: &CreateActionRow) -> usize {
        row_custom_id(row)
            .strip_prefix(PAGER_PREFIX)
            .expect("pager prefix")
            .split_once(':')
            .expect("two fields")
            .1
            .parse()
            .expect("page parses")
    }

    /// FR-009: the row attached to page 0 opens the first HIDDEN page, and every
    /// answer carries the row for the page after it. Without both halves the
    /// pages after the first press are stored but unreachable, and the button
    /// just re-shows the summary the reader already has in-channel.
    #[test]
    fn the_pager_chain_reaches_every_hidden_page() {
        let total = 5;
        let mut row = pager_row(7, 1, total);
        let mut visited = Vec::new();
        loop {
            let page = row_page(&row);
            visited.push(page);
            match next_page_row(7, page, total) {
                Some(next) => row = next,
                None => break,
            }
        }
        assert_eq!(
            visited,
            vec![1, 2, 3, 4],
            "every page after the in-channel one is reachable"
        );
    }

    /// The chain terminates: the last page carries no row, so a press cannot
    /// open a page that was never stored.
    #[test]
    fn the_last_page_carries_no_next_row() {
        assert!(next_page_row(7, 1, 2).is_none());
        assert!(next_page_row(7, 0, 1).is_none());
        assert!(next_page_row(7, 1, 3).is_some());
    }
}
