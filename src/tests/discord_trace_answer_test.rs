//! #1942: with `trace_narration` on, a CLI provider's answer arrives only
//! as intermediates (folded into the bubble as clipped notes) and the
//! final content is empty. The final path must deliver the last folded
//! body instead of skipping the post as an empty wrap-up.

use crate::channels::discord::trace_answer::final_text_for_delivery;

#[test]
fn trace_on_empty_final_delivers_last_folded_body() {
    let body = "Done. Render is at out/promo.mp4\n\nStill open:\n- voice pick".to_string();
    assert_eq!(
        final_text_for_delivery(String::new(), true, Some(body.clone())),
        body
    );
}

#[test]
fn trace_on_whitespace_final_counts_as_empty() {
    let body = "the real answer".to_string();
    assert_eq!(
        final_text_for_delivery("  \n\t ".to_string(), true, Some(body.clone())),
        body
    );
}

#[test]
fn trace_on_non_empty_final_wins() {
    assert_eq!(
        final_text_for_delivery("final".to_string(), true, Some("older note".to_string())),
        "final"
    );
}

#[test]
fn trace_off_empty_final_stays_empty() {
    // Intermediates were posted as real messages: the keep-intermediate
    // guard (#943/#951) must still skip a bare final.
    assert!(final_text_for_delivery(String::new(), false, Some("posted".to_string())).is_empty());
}

#[test]
fn trace_on_without_folded_body_stays_empty() {
    assert!(final_text_for_delivery(String::new(), true, None).is_empty());
}
