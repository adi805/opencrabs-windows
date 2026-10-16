//! Staging a decoded CLI attachment image on disk, and keeping the staging area
//! from growing without bound (F-BR-04).
//!
//! A CLI provider cannot take an image inline, so the bytes are decoded and
//! written to a temp file whose path is handed to the model in the message
//! ("[User attached an image at …]"). The file has to outlive the turn, because
//! the model reads it later with `analyze_image`, so it cannot be removed at the
//! point of write.
//!
//! Four providers did this and none of them ever removed anything. That is
//! bounded, but only by the host's own tmpfiles policy: `systemd-tmpfiles` keeps
//! `/tmp` for 30 days on a default Debian/Ubuntu, so a busy month of
//! full-resolution images accumulates in the meantime, and on a host where `/` is
//! nearly full that is the wrong thing to accumulate.
//!
//! So the sweep happens at the point of write, over the prefix this module owns.
//! Staging a new image removes the ones older than [`STALE_AFTER`] first, which
//! keeps the ceiling at "the last few hours of attachments" rather than "whatever
//! the OS's tmp policy is", and needs no lifecycle plumbing: the file is still
//! there for the turn that is about to read it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The prefix the CLI providers stage image attachments under.
pub(crate) const CLI_IMAGE_PREFIX: &str = "opencrabs_cli_img_";

/// The prefix the Command Code CLI stages under, kept separate so its files are
/// swept by its own writes rather than by every provider's.
pub(crate) const COMMAND_CODE_IMAGE_PREFIX: &str = "opencrabs_cmd_img_";

/// How old a staged image has to be before a later write may remove it.
///
/// Six hours is chosen to be far longer than any turn: the model reads the file
/// within the same turn it was told about, and a turn that ran for six hours would
/// have failed long before. Anything older is an orphan from a turn that ended,
/// crashed, or was abandoned.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// The file extension to stage a media type under.
pub(crate) fn extension_for_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Remove staged attachments under `dir` that carry `prefix` and are older than
/// `stale_after`.
///
/// Returns how many were removed, which is what the tests assert on and what a
/// caller could log. Errors on an individual file are swallowed: this runs on the
/// path that is about to deliver a user's attachment, and failing to clean up a
/// stale file must never fail that.
///
/// Only regular files whose name starts with `prefix` are candidates, so a
/// directory that happens to share the prefix is left alone.
pub(crate) fn sweep_stale_attachments(dir: &Path, prefix: &str, stale_after: Duration) -> usize {
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(prefix) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        // A clock that moved backwards makes `elapsed` fail; treat that as "not
        // old enough" rather than deleting on an unknown age.
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < stale_after {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Decode `data` (base64) and stage it under `prefix`, returning the file path.
///
/// Sweeps first, so the staging area is bounded by the writes rather than by the
/// host's tmp policy. `None` when the base64 does not decode or the write fails,
/// which is what the callers already treated as "fall back to a failure note".
pub(crate) fn stage_image(prefix: &str, media_type: &str, data: &str) -> Option<PathBuf> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "{prefix}{}.{}",
        uuid::Uuid::new_v4(),
        extension_for_media_type(media_type)
    ));
    // Sweep before the write: a failed sweep must not cost the attachment, and a
    // successful one cannot remove the file we are about to create.
    let removed = sweep_stale_attachments(&dir, prefix, STALE_AFTER);
    if removed > 0 {
        tracing::debug!("Removed {removed} stale CLI image attachment(s) from {prefix}");
    }
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

/// The message text a CLI provider substitutes for an inline image.
///
/// One string, in one place: the four providers each carried their own copy, and
/// a change to the wording that reached three of them would leave the fourth
/// telling the model something different about the same file.
pub(crate) fn image_marker(path: &Path) -> String {
    format!(
        "[User attached an image at {}. Use the analyze_image tool to view it.]",
        path.display()
    )
}
