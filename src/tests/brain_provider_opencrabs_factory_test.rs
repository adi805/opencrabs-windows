use crate::brain::provider::factory::create_provider_by_name;
use crate::config::types::io::{KeysFile, merge_provider_keys};
use crate::config::{Config, ProviderConfig, ProviderConfigs};

fn opencrabs_config(api_key: Option<&str>) -> Config {
    let providers = ProviderConfigs {
        opencrabs: Some(ProviderConfig {
            enabled: true,
            api_key: api_key.map(str::to_string),
            ..Default::default()
        }),
        ..Default::default()
    };
    Config {
        providers,
        ..Default::default()
    }
}

#[tokio::test]
async fn by_name_opencrabs_with_install_token() {
    let config = opencrabs_config(Some("tok-install-123"));
    let provider = create_provider_by_name(&config, "opencrabs")
        .await
        .expect("opencrabs should create with an install token");
    assert_eq!(provider.name(), "opencrabs");
}

#[tokio::test]
async fn by_name_opencrabs_without_token_is_not_created() {
    let config = opencrabs_config(None);
    let result = create_provider_by_name(&config, "opencrabs").await;
    assert!(
        result.is_err(),
        "without an install token the provider must not be created"
    );
}

#[test]
fn keys_merge_copies_the_install_token_into_the_provider() {
    let keys = ProviderConfigs {
        opencrabs: Some(ProviderConfig {
            api_key: Some("tok-install-123".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let merged = merge_provider_keys(ProviderConfigs::default(), keys);
    let entry = merged.opencrabs.expect("opencrabs entry created");
    assert_eq!(entry.api_key.as_deref(), Some("tok-install-123"));
}

#[test]
fn keys_file_with_install_id_still_parses() {
    // The store keeps install_id beside api_key. It is not a ProviderConfig
    // field, so it must be ignored rather than break keys.toml parsing.
    let raw = r#"
[providers.opencrabs]
install_id = "abcd1234abcd1234abcd1234abcd1234"
api_key = "tok-install-123"
"#;
    let file: KeysFile = toml::from_str(raw).expect("keys.toml with install_id parses");
    let section = file.providers.opencrabs.expect("section present");
    assert_eq!(section.api_key.as_deref(), Some("tok-install-123"));
}
