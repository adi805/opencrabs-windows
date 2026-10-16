//! `render_schedule` must survive a prompt whose 300-byte cut lands inside a
//! multibyte character.
//!
//! The cut used to be a raw byte slice (`&item.prompt[..300]`), which panics
//! with "byte index 300 is not a char boundary" when the boundary falls inside
//! a multibyte character. That is the #1919 / #2051 class; this was its third
//! instance in the tree. Cron prompts are written and edited from chat, so an
//! arrow or an emoji at the wrong offset is enough to take the TUI down when
//! the detail popup opens.

use crate::brain::mission_control::{McScheduleItem, McScheduleKind};
use crate::tui::render::mission_control::detail_popup::render_schedule;
use chrono::Utc;

fn item_with_prompt(prompt: String) -> McScheduleItem {
    McScheduleItem {
        id: "job-1".into(),
        label: "nightly".into(),
        schedule: "0 3 * * *".into(),
        kind: McScheduleKind::Cron,
        awaiting_user: false,
        prompt,
        deliver_to: None,
        last_run_at: None,
        next_run_at: None,
        created_at: Utc::now(),
        enabled: true,
        last_run_status: None,
        last_run_cost: None,
        last_run_duration_secs: None,
        profile_name: None,
    }
}

fn render_to_text(item: &McScheduleItem) -> String {
    render_schedule(item)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_multibyte_character_straddling_the_cut_does_not_panic() {
    // "→" is 3 bytes. Pad so byte 300 lands INSIDE it, which is exactly what
    // the old slice did.
    let mut prompt = "a".repeat(299);
    prompt.push('→');
    prompt.push_str(&"b".repeat(50));
    assert!(prompt.len() > 300);
    assert!(
        !prompt.is_char_boundary(300),
        "fixture must straddle the boundary to mean anything"
    );

    let text = render_to_text(&item_with_prompt(prompt));
    assert!(text.contains("..."), "the preview still marks truncation");
    // `truncate_str` walks BACK to the previous boundary, so a character the
    // cut landed inside is dropped rather than letting the preview overshoot
    // its byte budget by up to 3. The near-miss test below pins the other side
    // of that choice.
    assert!(
        !text.contains('→'),
        "the straddled character is dropped, not kept"
    );
    assert!(
        !text.contains(&"a".repeat(300)),
        "the preview never spends more than the 300-byte budget"
    );
}

#[test]
fn a_multibyte_character_ending_exactly_at_the_cut_survives() {
    // The near miss to the test above: "→" occupies bytes 297..300, so byte 300
    // IS a boundary and the whole character fits inside the budget.
    let mut prompt = "a".repeat(297);
    prompt.push('→');
    prompt.push_str(&"b".repeat(50));
    assert!(
        prompt.is_char_boundary(300),
        "fixture must end on the boundary to mean anything"
    );

    let text = render_to_text(&item_with_prompt(prompt));
    assert!(text.contains("..."), "the preview still marks truncation");
    assert!(
        text.contains('→'),
        "a character that fits inside the budget is kept"
    );
}

#[test]
fn an_emoji_at_the_cut_does_not_panic() {
    // 4-byte character, the widest case, straddling the cut.
    let mut prompt = "x".repeat(298);
    prompt.push('🦀');
    prompt.push_str(&"y".repeat(40));

    let text = render_to_text(&item_with_prompt(prompt));
    assert!(text.contains("..."));
    assert!(
        !text.contains('🦀'),
        "a straddled 4-byte character is dropped, not kept"
    );
    assert!(
        !text.contains(&"x".repeat(300)),
        "the preview never spends more than the 300-byte budget"
    );
}

#[test]
fn an_emoji_ending_exactly_at_the_cut_survives() {
    // Bytes 296..300: the emoji fits inside the budget exactly.
    let mut prompt = "x".repeat(296);
    prompt.push('🦀');
    prompt.push_str(&"y".repeat(40));
    assert!(prompt.is_char_boundary(300));

    let text = render_to_text(&item_with_prompt(prompt));
    assert!(
        text.contains('🦀'),
        "a 4-byte character that fits inside the budget is kept"
    );
}

#[test]
fn a_short_prompt_is_shown_whole() {
    let text = render_to_text(&item_with_prompt("short prompt".into()));
    assert!(text.contains("short prompt"));
    assert!(!text.contains("..."), "no ellipsis under the cut");
}

#[test]
fn a_long_ascii_prompt_is_still_truncated() {
    let text = render_to_text(&item_with_prompt("z".repeat(500)));
    assert!(text.contains("..."));
    assert!(!text.contains(&"z".repeat(301)), "cut at 300 bytes");
}
