//! Tests for the richer select-menu and modal specs (`component_spec`).
//!
//! The milestone 2 clause asked for more than a bare label per option and a
//! bare label per modal input. These pin the added keys, the Discord caps, and
//! the promise that the older, simpler shape still parses to the same
//! component.

use crate::channels::discord::component_spec::{
    FORM_LABEL_MAX, FORM_LENGTH_MAX, FORM_PLACEHOLDER_MAX, FORM_VALUE_MAX, SELECT_DESCRIPTION_MAX,
    SELECT_LABEL_MAX, SELECT_MAX_OPTIONS, SelectOption, parse_form_fields, parse_select_options,
    select_arity,
};
use serde_json::json;

// ── select options ────────────────────────────────────────────────────────────

#[test]
fn bare_string_option_is_the_label() {
    let raw = [json!("Deploy"), json!("Rollback")];
    let opts = parse_select_options(&raw);
    assert_eq!(opts.len(), 2);
    assert_eq!(
        opts[0],
        SelectOption {
            label: "Deploy".to_string(),
            description: None,
            emoji: None,
            default: false,
        }
    );
}

#[test]
fn object_option_carries_description_emoji_and_default() {
    let raw = [json!({
        "label": "Deploy",
        "description": "ship the current main",
        "emoji": "🚀",
        "default": true,
    })];
    let opts = parse_select_options(&raw);
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].label, "Deploy");
    let description = opts[0].description.as_deref();
    assert_eq!(description, Some("ship the current main"));
    assert_eq!(opts[0].emoji.as_deref(), Some("🚀"));
    assert!(opts[0].default);
}

#[test]
fn option_omitting_optional_keys_gets_defaults() {
    let raw = [json!({"label": "Just a label"})];
    let opts = parse_select_options(&raw);
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].description, None);
    assert_eq!(opts[0].emoji, None);
    assert!(!opts[0].default);
}

#[test]
fn select_options_are_capped_at_twenty_five() {
    let raw: Vec<_> = (0..30).map(|i| json!(format!("opt {i}"))).collect();
    assert_eq!(parse_select_options(&raw).len(), SELECT_MAX_OPTIONS);
}

#[test]
fn select_label_and_description_are_truncated() {
    let long = "x".repeat(SELECT_LABEL_MAX + 40);
    let desc = "y".repeat(SELECT_DESCRIPTION_MAX + 40);
    let raw = [json!({"label": long, "description": desc})];
    let opts = parse_select_options(&raw);
    assert_eq!(opts[0].label.chars().count(), SELECT_LABEL_MAX);
    assert_eq!(
        opts[0].description.as_ref().unwrap().chars().count(),
        SELECT_DESCRIPTION_MAX
    );
}

#[test]
fn entries_without_a_label_are_dropped() {
    let raw = [
        json!({"description": "no label here"}),
        json!(7),
        json!({"label": "kept"}),
    ];
    let opts = parse_select_options(&raw);
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].label, "kept");
}

// ── select arity ──────────────────────────────────────────────────────────────

#[test]
fn single_choice_by_default() {
    assert_eq!(select_arity(false, 3, None, None), (None, None));
}

#[test]
fn multi_select_opens_the_ceiling() {
    assert_eq!(select_arity(true, 3, None, None), (None, Some(3)));
}

#[test]
fn explicit_max_values_beats_the_sugar() {
    assert_eq!(select_arity(true, 3, None, Some(2)), (None, Some(2)));
    assert_eq!(select_arity(false, 3, None, Some(2)), (None, Some(2)));
}

#[test]
fn arity_is_clamped_to_the_option_count_and_the_platform_ceiling() {
    // 40 options is already beyond Discord's cap, so the ceiling is 25.
    assert_eq!(select_arity(false, 40, None, Some(30)), (None, Some(25)));
    assert_eq!(select_arity(false, 3, None, Some(9)), (None, Some(3)));
    // An empty option list still leaves a legal, non-zero ceiling.
    assert_eq!(select_arity(true, 0, None, None), (None, Some(1)));
}

#[test]
fn min_values_is_narrowed_to_max_values() {
    assert_eq!(select_arity(false, 5, Some(4), Some(2)), (Some(2), Some(2)));
}

#[test]
fn a_floor_above_one_widens_the_ceiling() {
    // Discord reads an absent max_values as 1, so a bare min_values above 1
    // would be refused; the ceiling has to rise to meet the floor.
    assert_eq!(select_arity(false, 5, Some(3), None), (Some(3), Some(3)));
    // An explicit ceiling still wins over the widening.
    assert_eq!(select_arity(false, 5, Some(3), Some(4)), (Some(3), Some(4)));
}

// ── form fields ───────────────────────────────────────────────────────────────

#[test]
fn legacy_field_shape_still_parses() {
    let raw = [json!({"label": "Name", "multiline": false})];
    let fields = parse_form_fields(&raw);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].label, "Name");
    assert!(!fields[0].multiline);
    // serenity's own default, so an input that never mentions it still demands
    // an answer.
    assert!(fields[0].required);
    assert_eq!(fields[0].placeholder, None);
    assert_eq!(fields[0].min_length, None);
    assert_eq!(fields[0].max_length, None);
    assert_eq!(fields[0].value, None);
}

#[test]
fn richer_field_keys_are_carried_through() {
    let raw = [json!({
        "label": "Bio",
        "multiline": true,
        "placeholder": "two lines please",
        "required": false,
        "min_length": 3,
        "max_length": 200,
        "value": "prefilled",
    })];
    let fields = parse_form_fields(&raw);
    assert_eq!(fields.len(), 1);
    let f = &fields[0];
    assert!(f.multiline);
    assert!(!f.required);
    assert_eq!(f.placeholder.as_deref(), Some("two lines please"));
    assert_eq!(f.min_length, Some(3));
    assert_eq!(f.max_length, Some(200));
    assert_eq!(f.value.as_deref(), Some("prefilled"));
}

#[test]
fn form_fields_are_capped_at_five() {
    let raw: Vec<_> = (0..7)
        .map(|i| json!({"label": format!("field {i}")}))
        .collect();
    assert_eq!(parse_form_fields(&raw).len(), 5);
}

#[test]
fn field_without_a_label_is_dropped() {
    let raw = [json!({"placeholder": "orphan"}), json!({"label": "kept"})];
    let fields = parse_form_fields(&raw);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].label, "kept");
}

#[test]
fn inverted_length_window_is_narrowed_instead_of_sent() {
    // Discord answers a 400 for min > max, which costs the turn.
    let raw = [json!({"label": "x", "min_length": 10, "max_length": 4})];
    let fields = parse_form_fields(&raw);
    assert_eq!(fields[0].min_length, Some(4));
    assert_eq!(fields[0].max_length, Some(4));
}

#[test]
fn length_window_and_text_are_clamped_to_platform_caps() {
    let raw = [json!({
        "label": "x",
        "min_length": 99999,
        "max_length": 99999,
        "placeholder": "p".repeat(FORM_PLACEHOLDER_MAX + 10),
        "value": "v".repeat(FORM_VALUE_MAX + 10),
    })];
    let fields = parse_form_fields(&raw);
    assert_eq!(fields[0].max_length, Some(FORM_LENGTH_MAX));
    assert_eq!(fields[0].min_length, Some(FORM_LENGTH_MAX));
    assert_eq!(
        fields[0].placeholder.as_ref().unwrap().chars().count(),
        FORM_PLACEHOLDER_MAX
    );
    assert_eq!(
        fields[0].value.as_ref().unwrap().chars().count(),
        FORM_VALUE_MAX
    );
}

#[test]
fn form_label_is_truncated_to_the_modal_cap() {
    let raw = [json!({"label": "L".repeat(FORM_LABEL_MAX + 20)})];
    let fields = parse_form_fields(&raw);
    assert_eq!(fields[0].label.chars().count(), FORM_LABEL_MAX);
}
