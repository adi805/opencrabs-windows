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
    // The built-in endpoint is free and fixed at 200k, so the key is not
    // offered in the example and is not honoured if a user adds it (#2066).
    assert_eq!(section.context_window, None);
}

/// Image generation ships on day 0 through the built-in provider, on its own
/// model id (served only on the image route, never as chat).
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
        Some("gemini-2.5-flash-image")
    );
    assert!(
        !section.models.iter().any(|m| m == "gemini-2.5-flash-image"),
        "image model must not appear in the chat model list"
    );
}

/// Voice is NOT served by the built-in provider: it answers no `/v1/audio/*`
/// route, so the shipped example must leave both audio fields unset. A set
/// field makes the provider's dead audio route the primary candidate ahead of
/// `[providers.stt.*]` / `[providers.tts.*]` (`src/config/types/loader.rs:80`),
/// so a working engine waits behind a failed round trip on every voice note
/// (#2105). Voice input keeps working through that engine either way.
#[test]
fn example_config_leaves_the_built_in_voice_models_unset() {
    let config: Config = toml::from_str(include_str!("../../config.toml.example"))
        .expect("config.toml.example parses");
    let section = config
        .providers
        .opencrabs
        .expect("[providers.opencrabs] present in the example");

    assert!(
        section.stt_model.is_none(),
        "no transcription route is served here yet, got: {:?}",
        section.stt_model
    );
    assert!(
        section.tts_model.is_none(),
        "no synthesis route is served here yet, got: {:?}",
        section.tts_model
    );
}
