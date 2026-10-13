use crate::brain::provider::opencrabs_acquire::ensure_install_at;
use crate::brain::provider::opencrabs_install::{INSTALL_PATH, InstallError};
use crate::brain::provider::opencrabs_store::{self, StoredInstall};
use crate::config::profile::with_home_override;

fn in_temp_home(f: impl FnOnce()) {
    let dir = tempfile::tempdir().expect("tempdir");
    let opencrabs = dir.path().join(".opencrabs");
    std::fs::create_dir_all(&opencrabs).expect("create .opencrabs");
    with_home_override(opencrabs, f);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn fresh_home_requests_a_token_and_stores_it() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .with_status(200)
                .with_body(r#"{"token":"tok-new-123","label":"install-abc"}"#)
                .expect(1)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("token issued");

            mock.assert_async().await;
            assert_eq!(install.token, "tok-new-123");
            assert_eq!(install.install_id.len(), 32);
            assert_eq!(opencrabs_store::load(), Some(install));
        });
    });
}

#[test]
fn stored_install_is_reused_without_calling_the_proxy() {
    in_temp_home(|| {
        let stored = StoredInstall {
            install_id: "abcd1234abcd1234abcd1234abcd1234".to_string(),
            token: "tok-kept-456".to_string(),
        };
        opencrabs_store::save(&stored).expect("save");
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .expect(0)
                .create_async()
                .await;

            let install = ensure_install_at(&server.url())
                .await
                .expect("stored install");

            mock.assert_async().await;
            assert_eq!(install, stored);
        });
    });
}

#[test]
fn refused_issuance_stores_nothing() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", INSTALL_PATH)
                .with_status(403)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("issuance is off");

            assert_eq!(err, InstallError::IssuanceOff);
            assert_eq!(opencrabs_store::load(), None);
        });
    });
}

#[test]
fn repeated_conflict_retries_once_then_gives_up() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", INSTALL_PATH)
                .with_status(409)
                .expect(2)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("conflict every time");

            mock.assert_async().await;
            assert_eq!(err, InstallError::AlreadyIssued);
            assert_eq!(opencrabs_store::load(), None);
        });
    });
}

#[test]
fn proxy_server_error_is_named_not_hidden() {
    in_temp_home(|| {
        runtime().block_on(async {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", INSTALL_PATH)
                .with_status(502)
                .expect(1)
                .create_async()
                .await;

            let err = ensure_install_at(&server.url())
                .await
                .expect_err("proxy failed");

            assert_eq!(err, InstallError::ProxyFailed);
        });
    });
}

#[test]
fn unreachable_proxy_is_named() {
    in_temp_home(|| {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        runtime().block_on(async {
            let err = ensure_install_at(&format!("http://127.0.0.1:{port}"))
                .await
                .expect_err("nothing listens there");

            assert_eq!(err, InstallError::Unreachable);
            assert_eq!(opencrabs_store::load(), None);
        });
    });
}
