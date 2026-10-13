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
