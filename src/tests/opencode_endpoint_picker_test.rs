//! OpenCode built-in provider: the Go or Zen plan picker (#2040). Each plan
//! serves its own model catalog and chat endpoint, so the picker decides both.

use crate::tui::onboarding::opencode_models_url;
use crate::tui::provider_selector::ProviderSelectorState;

#[test]
fn go_is_the_default_plan_and_its_catalog_is_the_go_models_endpoint() {
    assert_eq!(
        opencode_models_url(None),
        "https://opencode.ai/zen/go/v1/models"
    );
    assert_eq!(
        opencode_models_url(Some("go")),
        "https://opencode.ai/zen/go/v1/models"
    );
}

#[test]
fn zen_selects_the_zen_catalog() {
    assert_eq!(
        opencode_models_url(Some("zen")),
        "https://opencode.ai/zen/v1/models"
    );
}

#[test]
fn the_plan_writes_its_name_and_chat_endpoint_together() {
    assert_eq!(
        ProviderSelectorState::opencode_endpoint(0),
        ("go", "https://opencode.ai/zen/go/v1/chat/completions")
    );
    assert_eq!(
        ProviderSelectorState::opencode_endpoint(1),
        ("zen", "https://opencode.ai/zen/v1/chat/completions")
    );
}

#[test]
fn opencode_asks_for_its_plan_before_the_key() {
    use crate::tui::onboarding::{AuthField, OnboardingStep, OnboardingWizard, PROVIDERS};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c| KeyEvent::new(c, KeyModifiers::empty());

    let mut w = OnboardingWizard::new();
    w.step = OnboardingStep::ProviderAuth;
    w.ps.selected_provider = PROVIDERS
        .iter()
        .position(|p| p.id == "opencode")
        .expect("built-in OpenCode provider");
    w.auth_field = AuthField::Provider;

    w.handle_key(key(KeyCode::Enter));
    assert_eq!(w.auth_field, AuthField::OpenCodeEndpointType);
    assert_eq!(w.ps.opencode_endpoint_type, 0, "Go is the default plan");

    w.handle_key(key(KeyCode::Down));
    assert_eq!(w.ps.opencode_endpoint_type, 1, "the toggle selects Zen");

    w.handle_key(key(KeyCode::Enter));
    assert_eq!(
        w.auth_field,
        AuthField::ApiKey,
        "the key comes after the plan"
    );
}
