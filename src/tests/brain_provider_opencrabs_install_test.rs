use crate::brain::provider::opencrabs_install::*;

#[test]
fn generated_install_id_is_valid_and_32_hex_chars() {
    let id = new_install_id();
    assert_eq!(id.len(), 32);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(is_valid_install_id(&id));
}

#[test]
fn generated_install_ids_differ() {
    assert_ne!(new_install_id(), new_install_id());
}

#[test]
fn install_id_shape_matches_proxy_rules() {
    assert!(is_valid_install_id("abcd1234"));
    assert!(is_valid_install_id("a-b_c-d-e-f-g-h"));
    assert!(!is_valid_install_id("short"));
    assert!(!is_valid_install_id(&"a".repeat(65)));
    assert!(!is_valid_install_id("has space 12345"));
    assert!(!is_valid_install_id("dot.dot.dot.dot"));
}

#[test]
fn request_body_carries_the_install_id() {
    let body = install_request_body("abcd1234abcd1234");
    assert_eq!(body["install_id"], "abcd1234abcd1234");
    assert_eq!(body.as_object().map(|o| o.len()), Some(1));
}

#[test]
fn ok_reply_yields_token_and_label() {
    let issued = parse_install_response(200, r#"{"token":"tok-123","label":"install-abc"}"#)
        .expect("200 with a token issues");
    assert_eq!(issued.token, "tok-123");
    assert_eq!(issued.label, "install-abc");
}

#[test]
fn ok_reply_without_label_still_issues() {
    let issued = parse_install_response(200, r#"{"token":"tok-123"}"#).expect("token is enough");
    assert_eq!(issued.token, "tok-123");
    assert_eq!(issued.label, "");
}

#[test]
fn ok_reply_without_token_is_malformed() {
    assert_eq!(
        parse_install_response(200, r#"{"label":"x"}"#),
        Err(InstallError::MalformedReply)
    );
    assert_eq!(
        parse_install_response(200, r#"{"token":""}"#),
        Err(InstallError::MalformedReply)
    );
}

#[test]
fn ok_reply_that_is_not_json_is_malformed() {
    assert_eq!(
        parse_install_response(200, "<html>oops</html>"),
        Err(InstallError::MalformedReply)
    );
}

#[test]
fn each_refusal_status_maps_to_its_own_error() {
    assert_eq!(
        parse_install_response(400, ""),
        Err(InstallError::IdRejected)
    );
    assert_eq!(
        parse_install_response(409, ""),
        Err(InstallError::AlreadyIssued)
    );
    assert_eq!(
        parse_install_response(403, ""),
        Err(InstallError::IssuanceOff)
    );
    assert_eq!(
        parse_install_response(404, ""),
        Err(InstallError::IssuanceOff)
    );
    assert_eq!(
        parse_install_response(429, ""),
        Err(InstallError::RateLimited)
    );
    assert_eq!(
        parse_install_response(500, ""),
        Err(InstallError::ProxyFailed)
    );
    assert_eq!(
        parse_install_response(503, ""),
        Err(InstallError::ProxyFailed)
    );
    assert_eq!(
        parse_install_response(418, ""),
        Err(InstallError::Unexpected(418))
    );
}

#[test]
fn each_error_has_its_own_message() {
    let errors = [
        InstallError::IdRejected,
        InstallError::AlreadyIssued,
        InstallError::IssuanceOff,
        InstallError::RateLimited,
        InstallError::ProxyFailed,
        InstallError::Unexpected(418),
        InstallError::MalformedReply,
    ];
    let messages: Vec<&str> = errors.iter().map(install_error_message).collect();
    for (i, a) in messages.iter().enumerate() {
        assert!(!a.is_empty());
        for b in &messages[i + 1..] {
            assert_ne!(a, b, "two refusals share one message");
        }
    }
}

#[test]
fn messages_contain_no_em_dash() {
    let errors = [
        InstallError::IdRejected,
        InstallError::AlreadyIssued,
        InstallError::IssuanceOff,
        InstallError::RateLimited,
        InstallError::ProxyFailed,
        InstallError::Unexpected(418),
        InstallError::MalformedReply,
    ];
    for err in &errors {
        assert!(!install_error_message(err).contains('\u{2014}'));
    }
}
