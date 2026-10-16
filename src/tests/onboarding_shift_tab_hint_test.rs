//! #2043: Shift+Tab goes back a field on the provider step, so the step's
//! footer and `/help` must say so.

use crate::tui::app::help_catalog::{HelpSection, load};
use crate::tui::onboarding::{AuthField, OnboardingStep, OnboardingWizard, PROVIDERS};
use crate::tui::onboarding_render::render_onboarding;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn screen_text(wizard: &OnboardingWizard, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_onboarding(f, wizard)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn provider_step_footer_names_shift_tab() {
    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::ProviderAuth;
    w.ps.selected_provider = PROVIDERS
        .iter()
        .position(|p| p.id == "openai")
        .expect("openai");
    w.auth_field = AuthField::ApiKey;
    w.resume_notice = None;
    let text = screen_text(&w, 120, 40);
    assert!(
        text.contains("Shift+Tab"),
        "footer lacks Shift+Tab:\n{text}"
    );
    assert!(text.contains("Back"), "footer lacks Back:\n{text}");
}

#[test]
fn help_lists_the_field_keys_including_shift_tab() {
    let rows = load();
    let keys: Vec<&str> = rows
        .iter()
        .filter(|r| r.section == HelpSection::Keys)
        .map(|r| r.name.as_str())
        .collect();
    for k in ["Tab", "Shift+Tab", "Enter", "Esc"] {
        assert!(keys.contains(&k), "help misses {k}: {keys:?}");
    }
    let back = rows
        .iter()
        .find(|r| r.name == "Shift+Tab")
        .expect("Shift+Tab row");
    assert!(
        back.description.contains("previous field"),
        "{}",
        back.description
    );
}
