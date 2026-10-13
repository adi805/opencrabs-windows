use crate::brain::provider::opencrabs_store::{StoredInstall, load, save};
use crate::config::profile::with_home_override;

fn in_temp_home(f: impl FnOnce()) {
    let dir = tempfile::tempdir().expect("tempdir");
    let opencrabs = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&opencrabs).expect("create .opencrabs");
    with_home_override(opencrabs, f);
}

fn keys_text() -> String {
    std::fs::read_to_string(crate::config::keys_path()).expect("keys.toml written")
}

fn install() -> StoredInstall {
    StoredInstall {
        install_id: "abcd1234abcd1234abcd1234abcd1234".to_string(),
        token: "tok-live-123".to_string(),
    }
}

#[test]
fn fresh_home_has_no_stored_install() {
    in_temp_home(|| {
        assert_eq!(load(), None);
    });
}

#[test]
fn save_then_load_round_trips_id_and_token() {
    in_temp_home(|| {
        save(&install()).expect("save");
        assert_eq!(load(), Some(install()));
    });
}

#[test]
fn id_and_token_land_in_the_opencrabs_section() {
    in_temp_home(|| {
        save(&install()).expect("save");
        let text = keys_text();
        assert!(text.contains("[providers.opencrabs]"), "{text}");
        assert!(text.contains("install_id = \"abcd1234abcd1234abcd1234abcd1234\""));
        assert!(text.contains("api_key = \"tok-live-123\""));
    });
}

#[test]
fn save_keeps_other_provider_sections() {
    in_temp_home(|| {
        std::fs::write(
            crate::config::keys_path(),
            "[providers.xiaomi]\napi_key = \"xiaomi-key\"\n",
        )
        .expect("seed keys.toml");
        save(&install()).expect("save");
        let text = keys_text();
        assert!(text.contains("xiaomi-key"), "{text}");
        assert_eq!(load(), Some(install()));
    });
}

#[test]
fn saving_a_new_install_replaces_the_old_one() {
    in_temp_home(|| {
        save(&install()).expect("first save");
        let next = StoredInstall {
            install_id: "ffff0000ffff0000ffff0000ffff0000".to_string(),
            token: "tok-new-456".to_string(),
        };
        save(&next).expect("second save");
        assert_eq!(load(), Some(next));
    });
}

#[test]
fn id_without_token_is_not_a_stored_install() {
    in_temp_home(|| {
        std::fs::write(
            crate::config::keys_path(),
            "[providers.opencrabs]\ninstall_id = \"abcd1234abcd1234abcd1234abcd1234\"\n",
        )
        .expect("seed keys.toml");
        assert_eq!(load(), None);
    });
}

#[test]
fn malformed_stored_id_is_not_a_stored_install() {
    in_temp_home(|| {
        std::fs::write(
            crate::config::keys_path(),
            "[providers.opencrabs]\ninstall_id = \"bad id\"\napi_key = \"tok\"\n",
        )
        .expect("seed keys.toml");
        assert_eq!(load(), None);
    });
}

#[test]
fn unparseable_keys_file_is_not_a_stored_install() {
    in_temp_home(|| {
        std::fs::write(crate::config::keys_path(), "[providers.opencrabs\n").expect("seed");
        assert_eq!(load(), None);
    });
}
