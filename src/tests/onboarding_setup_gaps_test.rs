//! Tests for the setup-gap check that drives the quick-start welcome.
//!
//! The check is pure: USER.md text and the three capability flags go in,
//! the list of gaps comes out. Nothing here reads the live home.

use crate::tui::onboarding::SetupGap;
use crate::tui::onboarding::setup_gaps::{collect_gaps, profile_gaps, render_gaps};

const TEMPLATE_USER_MD: &str = include_str!("../docs/reference/templates/USER.md");

const FILLED_USER_MD: &str = "\
# USER.md - About Your Human

| Field | Value |
|-------|-------|
| **Name** | Adolfo |
| **Location** | Coimbra, Portugal |
| **Timezone** | UTC+1 |
| **Availability** | 10AM - 4AM |

## Professional

| Field | Value |
|-------|-------|
| **Title** | Senior Engineer |
";

#[test]
fn untouched_template_leaves_every_profile_field_missing() {
    assert_eq!(
        profile_gaps(TEMPLATE_USER_MD),
        vec![
            SetupGap::Name,
            SetupGap::Location,
            SetupGap::Timezone,
            SetupGap::Title,
        ]
    );
}

#[test]
fn filled_profile_leaves_no_profile_gaps() {
    assert!(profile_gaps(FILLED_USER_MD).is_empty());
}

#[test]
fn only_the_unfilled_rows_are_reported() {
    let partial = "| **Name** | Adolfo |\n| **Location** | *(city, country)* |\n";
    assert_eq!(
        profile_gaps(partial),
        vec![SetupGap::Location, SetupGap::Timezone, SetupGap::Title]
    );
}

#[test]
fn a_missing_user_md_counts_as_an_empty_profile() {
    assert_eq!(profile_gaps("").len(), 4);
}

#[test]
fn a_blank_value_counts_as_missing() {
    let blank = "| **Name** |  |\n";
    assert!(profile_gaps(blank).contains(&SetupGap::Name));
}

#[test]
fn profile_gaps_come_first_then_the_capabilities() {
    assert_eq!(
        collect_gaps("", false, false, false),
        vec![
            SetupGap::Name,
            SetupGap::Location,
            SetupGap::Timezone,
            SetupGap::Title,
            SetupGap::SpeechToText,
            SetupGap::TextToSpeech,
            SetupGap::ImageGeneration,
        ]
    );
}

#[test]
fn configured_capabilities_are_not_asked_about() {
    assert_eq!(
        collect_gaps(FILLED_USER_MD, true, true, false),
        vec![SetupGap::ImageGeneration]
    );
}

#[test]
fn everything_set_gives_no_gaps() {
    assert!(collect_gaps(FILLED_USER_MD, true, true, true).is_empty());
}

#[test]
fn render_names_each_gap_and_handles_none() {
    assert_eq!(
        render_gaps(&[SetupGap::SpeechToText, SetupGap::TextToSpeech]),
        "speech to text, text to speech"
    );
    assert_eq!(render_gaps(&[]), "nothing, everything is set");
}

#[test]
fn gap_labels_have_no_em_dash() {
    for gap in collect_gaps("", false, false, false) {
        assert!(!gap.label().contains('\u{2014}'));
    }
}
