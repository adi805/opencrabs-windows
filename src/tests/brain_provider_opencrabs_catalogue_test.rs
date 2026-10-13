use crate::config::Config;

/// The `/models` picker reads its catalogue from the embedded example, so the
/// built-in provider's model has to be listed there and default to disabled.
#[test]
fn example_config_lists_the_built_in_model_disabled_by_default() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert!(!section.enabled, "built-in provider ships disabled");
    assert_eq!(
        section.default_model.as_deref(),
        Some("deepseek-v4.1-flash")
    );
    assert_eq!(section.models, vec!["deepseek-v4.1-flash".to_string()]);
    assert_eq!(section.context_window, Some(200_000));
}

/// Image generation ships on day 0 through the built-in provider, on its own
/// model id (served only on the proxy's image routes, never as chat).
#[test]
fn example_config_sets_the_built_in_image_model() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert_eq!(
        section.generation_model.as_deref(),
        Some("gemini-3.1-flash-image")
    );
    assert!(
        !section.models.iter().any(|m| m == "gemini-3.1-flash-image"),
        "image model must not appear in the chat model list"
    );
}
