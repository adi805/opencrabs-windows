//! Richer select-menu and modal specs (milestone 2 clause).
//!
//! #382 shipped a string select whose options were bare labels, and #383 a
//! modal whose fields were a label plus a multiline flag. Both work, and both
//! leave the shapes Discord actually supports unused: a select option can
//! carry a description, an emoji and a default selection, and the menu itself
//! can take several picks; a modal input can carry a placeholder, a required
//! flag, a length window and a prefilled value.
//!
//! The parsing lives here rather than inside the tool so the arity rules and
//! Discord's caps are one testable unit, and so the tool and the gateway agree
//! on what a spec means. Every added key is optional: an input written against
//! the #382/#383 shape parses to exactly the component it used to build.

use serde_json::Value;

/// Discord caps a string select menu at 25 options.
pub(crate) const SELECT_MAX_OPTIONS: usize = 25;
/// Discord caps a select option label at 100 characters.
pub(crate) const SELECT_LABEL_MAX: usize = 100;
/// Discord caps a select option description at 100 characters.
pub(crate) const SELECT_DESCRIPTION_MAX: usize = 100;
/// Discord caps `min_values` / `max_values` at 25.
pub(crate) const SELECT_MAX_VALUES: u8 = 25;
/// Discord caps a modal at 5 inputs.
pub(crate) const FORM_MAX_FIELDS: usize = 5;
/// Discord caps a modal input label at 45 characters.
pub(crate) const FORM_LABEL_MAX: usize = 45;
/// Discord caps a modal input placeholder at 100 characters.
pub(crate) const FORM_PLACEHOLDER_MAX: usize = 100;
/// Discord caps a modal input value at 4000 characters.
pub(crate) const FORM_VALUE_MAX: usize = 4000;
/// Discord's ceiling for `min_length` / `max_length` on a modal input.
pub(crate) const FORM_LENGTH_MAX: u16 = 1024;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// One option of a string select menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectOption {
    /// What the user reads in the menu.
    pub label: String,
    /// The second line under the label, Discord's own explanation slot.
    pub description: Option<String>,
    /// Unicode emoji drawn beside the label.
    pub emoji: Option<String>,
    /// Pre-select this option when the menu opens.
    pub default: bool,
}

/// Parse the `options` input.
///
/// A bare string is the label, which is the shape #382 shipped and stays
/// valid. An object adds `description`, `emoji` and `default`. Entries past
/// Discord's 25-option cap are dropped, and an object without a label is
/// dropped rather than guessed at.
pub(crate) fn parse_select_options(raw: &[Value]) -> Vec<SelectOption> {
    raw.iter()
        .take(SELECT_MAX_OPTIONS)
        .filter_map(|entry| {
            if let Some(label) = entry.as_str() {
                return Some(SelectOption {
                    label: truncate(label, SELECT_LABEL_MAX),
                    description: None,
                    emoji: None,
                    default: false,
                });
            }
            let label = entry.get("label")?.as_str()?;
            Some(SelectOption {
                label: truncate(label, SELECT_LABEL_MAX),
                description: entry
                    .get("description")
                    .and_then(Value::as_str)
                    .map(|d| truncate(d, SELECT_DESCRIPTION_MAX)),
                emoji: entry
                    .get("emoji")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                default: entry
                    .get("default")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// Resolve the arity of a select menu.
///
/// `multi_select` is sugar for "the user may pick any number of options": it
/// sets `max_values` to the option count and leaves `min_values` at Discord's
/// default of 1. An explicit `max_values` wins over the sugar, and an explicit
/// `min_values` is clamped down to the ceiling so a spec can never ask for
/// more picks than the menu allows.
///
/// A floor above 1 with no explicit ceiling widens the ceiling to meet it,
/// because Discord reads an absent `max_values` as 1 and refuses the menu
/// otherwise.
pub(crate) fn select_arity(
    multi_select: bool,
    option_count: usize,
    min_values: Option<u8>,
    max_values: Option<u8>,
) -> (Option<u8>, Option<u8>) {
    let ceiling = option_count.clamp(1, SELECT_MAX_VALUES as usize) as u8;
    let floor = min_values.map(|m| m.clamp(1, SELECT_MAX_VALUES));
    let max = max_values
        .map(|m| m.clamp(1, SELECT_MAX_VALUES).min(ceiling))
        .or_else(|| multi_select.then_some(ceiling))
        .or(floor.filter(|f| *f > 1));
    let min = floor.map(|m| m.min(max.unwrap_or(SELECT_MAX_VALUES)));
    (min, max)
}

/// One input of a modal form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormField {
    /// The label Discord prints above the input.
    pub label: String,
    /// Paragraph style, for an answer longer than one line.
    pub multiline: bool,
    /// Grey hint text inside the empty input.
    pub placeholder: Option<String>,
    /// Whether Discord refuses an empty submission.
    pub required: bool,
    /// Shortest accepted answer, in characters.
    pub min_length: Option<u16>,
    /// Longest accepted answer, in characters.
    pub max_length: Option<u16>,
    /// Prefilled answer.
    pub value: Option<String>,
}

impl FormField {
    /// The shape #383 shipped: a label and whether the input is multiline.
    pub(crate) fn new(label: impl Into<String>, multiline: bool) -> Self {
        Self {
            label: label.into(),
            multiline,
            placeholder: None,
            required: true,
            min_length: None,
            max_length: None,
            value: None,
        }
    }
}

/// Parse the `fields` input.
///
/// `label` is the only required key, which keeps the #383 shape valid. An
/// entry without a label is dropped. `required` defaults to true, matching
/// serenity's own default, so an input that never mentions it still demands an
/// answer. A length window whose floor sits above its ceiling is narrowed to
/// the ceiling instead of handed to Discord, which answers a 400 and costs the
/// turn.
pub(crate) fn parse_form_fields(raw: &[Value]) -> Vec<FormField> {
    raw.iter()
        .take(FORM_MAX_FIELDS)
        .filter_map(|entry| {
            let label = entry.get("label")?.as_str()?;
            let mut field = FormField::new(
                truncate(label, FORM_LABEL_MAX),
                entry
                    .get("multiline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            field.placeholder = entry
                .get("placeholder")
                .and_then(Value::as_str)
                .map(|p| truncate(p, FORM_PLACEHOLDER_MAX));
            field.required = entry
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            field.min_length = entry
                .get("min_length")
                .and_then(Value::as_u64)
                .map(|n| n.min(u64::from(FORM_LENGTH_MAX)) as u16);
            field.max_length = entry
                .get("max_length")
                .and_then(Value::as_u64)
                .map(|n| n.min(u64::from(FORM_LENGTH_MAX)) as u16);
            if let (Some(min), Some(max)) = (field.min_length, field.max_length)
                && min > max
            {
                field.min_length = Some(max);
            }
            field.value = entry
                .get("value")
                .and_then(Value::as_str)
                .map(|v| truncate(v, FORM_VALUE_MAX));
            Some(field)
        })
        .collect()
}
