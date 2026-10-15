//! #2060: `/models` opens on the session's provider and model, and the config
//! fallback only ever picks an enabled provider.

use crate::config::{Config, ProviderConfig};
use crate::tui::onboarding::OnboardingWizard;
use crate::tui::provider_selector::{CUSTOM_INSTANCES_START, index_of_provider};
use std::collections::HashMap;

fn enabled() -> ProviderConfig {
    ProviderConfig {
        enabled: true,
        ..Default::default()
    }
}

fn custom(base: &str, enabled: bool) -> ProviderConfig {
    ProviderConfig {
        enabled,
        base_url: Some(base.to_string()),
        ..Default::default()
    }
}

#[test]
fn an_enabled_builtin_opencrabs_provider_is_the_one_opened() {
    let mut config = Config::default();
    config.providers.opencrabs = Some(enabled());
    let wizard = OnboardingWizard::from_config(&config);
    assert_eq!(
        wizard.ps.selected_provider,
        index_of_provider("opencrabs").expect("opencrabs is a provider")
    );
}

#[test]
fn a_disabled_custom_provider_is_never_selected_by_fallback() {
    let mut config = Config::default();
    let mut customs = HashMap::new();
    customs.insert(
        "baidu-ernie".to_string(),
        custom("https://qianfan.example/v2", false),
    );
    config.providers.custom = Some(customs.into_iter().collect());
    let wizard = OnboardingWizard::from_config(&config);
    assert!(
        wizard.ps.selected_provider < CUSTOM_INSTANCES_START,
        "a disabled custom provider must not be opened; got index {}",
        wizard.ps.selected_provider
    );
}

#[test]
fn the_session_custom_provider_is_selected_by_name() {
    let mut customs = HashMap::new();
    customs.insert("alpha".to_string(), custom("https://a.example/v1", true));
    customs.insert("beta".to_string(), custom("https://b.example/v1", true));
    let mut config = Config::default();
    config.providers.custom = Some(customs.into_iter().collect());

    let mut wizard = OnboardingWizard::from_config(&config);
    let found = wizard.select_session_pair("custom:beta", "gpt-x", &config);
    assert!(found);
    let pos = config
        .providers
        .custom
        .as_ref()
        .unwrap()
        .keys()
        .position(|k| k == "beta")
        .unwrap();
    assert_eq!(wizard.ps.selected_provider, CUSTOM_INSTANCES_START + pos);
    assert_eq!(wizard.ps.custom_name, "beta");
    assert_eq!(wizard.ps.base_url, "https://b.example/v1");
    assert_eq!(wizard.ps.custom_model, "gpt-x");
}

#[test]
fn the_session_builtin_model_is_the_one_selected_even_when_unlisted() {
    let config = Config::default();
    let mut wizard = OnboardingWizard::from_config(&config);
    let found = wizard.select_session_pair("zai", "glm-session-only", &config);
    assert!(found);
    assert_eq!(
        wizard.ps.selected_provider,
        index_of_provider("zai").unwrap()
    );
    let all = wizard.ps.all_model_names();
    assert_eq!(all[wizard.ps.selected_model], "glm-session-only");
}

#[test]
fn an_unknown_session_provider_leaves_the_dialog_alone() {
    let config = Config::default();
    let mut wizard = OnboardingWizard::from_config(&config);
    let before = wizard.ps.selected_provider;
    let found = wizard.select_session_pair("custom:gone", "m", &config);
    assert!(!found);
    assert_eq!(wizard.ps.selected_provider, before);
}
