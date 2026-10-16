//! The CLI attachment staging area: bounded, and bounded by its own writes
//! (F-BR-04).
//!
//! A CLI provider cannot take an image inline, so the bytes are decoded to a temp
//! file whose path is handed to the model. The file has to outlive the turn
//! because the model reads it later, so it cannot be removed at the point of
//! write; four providers did exactly that and none of them ever removed anything.
//!
//! The staging area is now swept at the point of write, over the prefix this
//! module owns. These tests pin the sweep's boundaries: what it removes, what it
//! must leave alone, and that a fresh file is never a candidate.

use crate::utils::cli_attachment::{
    CLI_IMAGE_PREFIX, COMMAND_CODE_IMAGE_PREFIX, STALE_AFTER, extension_for_media_type,
    image_marker, stage_image, sweep_stale_attachments,
};
use std::time::Duration;

fn touch(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"x").unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    path
}

/// A stale prefixed file goes, and everything that is not one stays: a file under
/// the OTHER provider's prefix and an unrelated name.
///
/// The age is expressed through the threshold rather than by backdating an mtime,
/// so the test says the same thing on every platform and does not depend on a
/// filesystem that reports mtime the way the test assumes. That is also why no
/// second file under our OWN prefix is created here: with a zero threshold it
/// would be stale too, so it could not demonstrate that a fresh one survives.
/// The age boundary is `a_fresh_file_is_never_a_candidate`, which uses the real
/// `STALE_AFTER`.
#[test]
fn the_sweep_removes_only_stale_files_under_its_own_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stale = touch(dir.path(), &format!("{CLI_IMAGE_PREFIX}old.png"));
    let other_provider = touch(dir.path(), &format!("{COMMAND_CODE_IMAGE_PREFIX}other.png"));
    let unrelated = touch(dir.path(), "something-else.png");

    // A zero threshold makes every existing file "stale", which isolates the
    // prefix rule from the age rule.
    let removed = sweep_stale_attachments(dir.path(), CLI_IMAGE_PREFIX, Duration::ZERO);

    assert_eq!(removed, 1, "exactly one file is ours and old enough");
    assert!(!stale.exists(), "a stale attachment must be removed");
    assert!(
        other_provider.exists(),
        "the other provider's staging area is swept by its own writes, not this one"
    );
    assert!(unrelated.exists(), "an unrelated name is never a candidate");
}

/// The age boundary: a file under the threshold is not touched.
#[test]
fn a_fresh_file_is_never_a_candidate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fresh = touch(dir.path(), &format!("{CLI_IMAGE_PREFIX}just-written.png"));

    let removed = sweep_stale_attachments(dir.path(), CLI_IMAGE_PREFIX, STALE_AFTER);

    assert_eq!(
        removed, 0,
        "the threshold must protect a file written just now"
    );
    assert!(fresh.exists());
}

/// A directory that happens to carry the prefix must not be deleted: the sweep
/// calls `remove_file`, and it checks `is_file` first so a name collision cannot
/// turn into an error loop.
#[test]
fn a_directory_sharing_the_prefix_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let colliding = dir.path().join(format!("{CLI_IMAGE_PREFIX}not-a-file"));
    std::fs::create_dir(&colliding).expect("create the colliding directory");

    let removed = sweep_stale_attachments(dir.path(), CLI_IMAGE_PREFIX, Duration::ZERO);

    assert_eq!(removed, 0);
    assert!(colliding.exists(), "only regular files are candidates");
}

/// A directory that does not exist is not an error. The sweep runs on the path
/// that is about to deliver a user's attachment, so a failure to clean up must
/// never fail that.
#[test]
fn a_missing_directory_sweeps_zero_rather_than_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("not-there");
    assert_eq!(
        sweep_stale_attachments(&missing, CLI_IMAGE_PREFIX, Duration::ZERO),
        0
    );
}

/// The staging contract: decode, write, return the path, and the bytes on disk
/// are the decoded bytes rather than the base64 text.
#[test]
fn staging_decodes_the_payload_and_returns_the_path() {
    use base64::Engine;
    let payload = b"not really a png, but the bytes are the point";
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);

    let path =
        stage_image(CLI_IMAGE_PREFIX, "image/png", &encoded).expect("a valid payload stages");
    let written = std::fs::read(&path).expect("read back");
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    // Clean up before asserting, so a failure does not leave the file behind.
    let _ = std::fs::remove_file(&path);

    assert_eq!(written, payload, "the file holds the decoded bytes");
    assert!(name.starts_with(CLI_IMAGE_PREFIX), "{name}");
    assert!(name.ends_with(".png"), "{name}");
}

/// A payload that is not base64 is reported rather than staged as garbage, which
/// is what the providers turn into their failure note.
#[test]
fn an_undecodable_payload_stages_nothing() {
    assert!(stage_image(CLI_IMAGE_PREFIX, "image/png", "not base64 !!!").is_none());
}

/// The media-type mapping, including the fallback: an unknown type still stages,
/// because the file's extension is only a hint for the reader.
#[test]
fn the_extension_follows_the_media_type() {
    assert_eq!(extension_for_media_type("image/png"), "png");
    assert_eq!(extension_for_media_type("image/jpeg"), "jpeg");
    assert_eq!(extension_for_media_type("image/gif"), "gif");
    assert_eq!(extension_for_media_type("image/webp"), "webp");
    assert_eq!(extension_for_media_type("image/avif"), "png");
    assert_eq!(extension_for_media_type(""), "png");
}

/// The marker the model reads names the file, since that path is the only way it
/// can reach the image.
#[test]
fn the_marker_names_the_staged_path() {
    let marker = image_marker(std::path::Path::new("/tmp/opencrabs_cli_img_abc.png"));
    assert!(
        marker.contains("/tmp/opencrabs_cli_img_abc.png"),
        "{marker}"
    );
    assert!(marker.contains("analyze_image"), "{marker}");
}
