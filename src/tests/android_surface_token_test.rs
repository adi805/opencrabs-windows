//! The Android app must seed the loopback surface's bearer token itself.
//!
//! `src/session/surface.rs` exempts a token-less listener on a loopback bind,
//! on the reasoning that `127.0.0.1` is reachable only by the same user. That
//! reasoning holds on a desktop and fails on Android, where the loopback
//! interface is shared by every app on the device: a token-less surface on
//! `127.0.0.1` is reachable by any other installed app, and that surface can
//! run the agent with tools. So on Android the token is mandatory, and the app
//! is the only thing that can supply it (the core cannot tell it runs in an
//! APK).
//!
//! The app is Java, which nothing in this repo compiles before merge: the only
//! job that runs `javac` is `android-artifact.yml`, and its `push` trigger
//! fires on `main`. So a Java regression here would be invisible to the PR that
//! introduced it. These tests read the Java source instead, which does run on
//! every PR, and pin the three properties the security argument rests on:
//! the token is generated, it reaches the config the core reads, and it is
//! generated before the core spawns.

/// The service that spawns and supervises the core process.
const CORE_SERVICE: &str = "android/java/io/opencrabs/mobile/CoreService.java";

fn core_service() -> String {
    let path = format!("{}/{CORE_SERVICE}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// The body of `onStartCommand`, up to its closing brace at method indent.
fn on_start_command(src: &str) -> &str {
    let start = src
        .find("public int onStartCommand(")
        .expect("CoreService no longer declares onStartCommand; update this test");
    let rest = &src[start..];
    let end = rest
        .find("\n    }")
        .expect("onStartCommand body not found; update this test");
    &rest[..end]
}

#[test]
fn the_token_is_seeded_before_the_core_spawns() {
    let src = core_service();
    let body = on_start_command(&src);
    let seed = body.find("ensureSurfaceToken(").expect(
        "onStartCommand no longer seeds the surface token; without it the core \
         reads a config with no api_key and the surface is unauthenticated on \
         a loopback interface shared with every other app on the device",
    );
    let spawn = body
        .find("spawnCore()")
        .expect("onStartCommand no longer spawns the core; update this test");
    assert!(
        seed < spawn,
        "the token is seeded after the core spawns, so the core's first config \
         read can observe an empty api_key. Seed first, then spawn."
    );
}

#[test]
fn the_seeded_config_declares_the_surface_table_and_key() {
    let src = core_service();
    // The Java literals are escaped in the source because they are written as
    // `.append("api_key = \"")`. Match the source text, not the rendered TOML.
    for needle in [
        "[session_surface]",
        "api_key = \\\"",
        "enabled = true",
        "bind = \\\"127.0.0.1\\\"",
    ] {
        assert!(
            src.contains(needle),
            "CoreService.java no longer writes '{needle}' into the seeded \
             config. The Rust gate in src/session/surface.rs reads exactly \
             these keys; a rename here silently disarms the token."
        );
    }
}

#[test]
fn an_existing_surface_table_is_never_rewritten() {
    let src = core_service();
    // A second `[session_surface]` header makes the file unparseable, and the
    // core then refuses to start. So the seeder has to skip a config that
    // already declares the table rather than appending blindly.
    assert!(
        src.contains("contains(SURFACE_TABLE)"),
        "the seeder no longer checks for an existing [session_surface] table. \
         Appending a second header makes config.toml unparseable and the core \
         will not start."
    );
}

#[test]
fn the_token_is_random_and_not_base64_encoded() {
    let src = core_service();
    assert!(
        src.contains("new SecureRandom()"),
        "the surface token is no longer generated from a SecureRandom source"
    );
    assert!(
        src.contains("byte[] raw = new byte[32]"),
        "the surface token is no longer 32 bytes of entropy"
    );
    // Base64 is the obvious encoding and the wrong one here: java.util.Base64
    // is API 26, android.util.Base64 is not on the JDK classpath javac resolves
    // java.* against. This file has already been bitten once by an API-level
    // mismatch that compiled cleanly and threw on device.
    //
    // Comment lines are stripped first: this file explains the Base64 trap in
    // its own docs, so a raw `contains` would match that prose and fail on a
    // file that is correct.
    let code: String = src
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with('*') && !t.starts_with("//") && !t.starts_with("/*")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("Base64"),
        "CoreService.java now uses Base64 for the token. java.util.Base64 needs \
         API 26 and android.util.Base64 is absent from the javac classpath; use \
         the hex encoder or the APK build will fail or throw on device."
    );
}
