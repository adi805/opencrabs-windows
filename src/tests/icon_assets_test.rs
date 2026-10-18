//! The macOS app icon ships inside a bundle as an `.icns` container. That
//! container is a flat list of typed members, one per pixel size. If the ladder
//! stops short, Finder and the Dock upscale a small member and the icon looks
//! soft on every Retina display.
//!
//! These tests read the shipped asset and assert the ladder is complete, so a
//! future asset swap cannot silently regress it.

use std::path::Path;

fn icns() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/icon.icns");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every member is an 8-byte header (4-char type, big-endian length) followed
/// by the payload. The container itself opens with the same shape: `icns` plus
/// the total byte length.
fn members(bytes: &[u8]) -> Vec<(String, usize)> {
    assert!(bytes.len() > 8, "icns container is truncated");
    assert_eq!(&bytes[0..4], b"icns", "missing icns magic");

    let total = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    assert_eq!(
        total,
        bytes.len(),
        "declared container length {total} does not match file size {}",
        bytes.len()
    );

    let mut out = Vec::new();
    let mut off = 8;
    while off + 8 <= bytes.len() {
        let kind = String::from_utf8_lossy(&bytes[off..off + 4]).to_string();
        let len = u32::from_be_bytes([
            bytes[off + 4],
            bytes[off + 5],
            bytes[off + 6],
            bytes[off + 7],
        ]) as usize;
        assert!(len >= 8, "member {kind} declares length {len}");
        assert!(off + len <= bytes.len(), "member {kind} runs past the file");
        out.push((kind, len));
        off += len;
    }
    assert_eq!(off, bytes.len(), "trailing bytes after the last member");
    out
}

fn kinds() -> Vec<String> {
    members(&icns()).into_iter().map(|(kind, _)| kind).collect()
}

/// `iconutil` emits the ten members a full ladder needs. Without `ic10` the
/// largest member is 512 and a Retina Dock upscales it.
#[test]
fn the_icon_ladder_covers_every_size() {
    let kinds = kinds();
    for (kind, size) in [
        ("ic04", 16),
        ("ic05", 32),
        ("ic07", 128),
        ("ic08", 256),
        ("ic09", 512),
        ("ic10", 1024),
        ("ic11", 32),
        ("ic12", 64),
        ("ic13", 256),
        ("ic14", 512),
    ] {
        assert!(
            kinds.iter().any(|k| k == kind),
            "missing {kind} ({size}px); ladder is {kinds:?}"
        );
    }
}

/// The 1024 member is the one that matters on a Retina display, so it gets its
/// own assertion and its own failure message.
#[test]
fn the_largest_member_is_1024() {
    assert!(
        kinds().iter().any(|k| k == "ic10"),
        "no 1024x1024 member; the largest size is 512 and every Retina display upscales it"
    );
}

#[test]
fn the_container_length_matches_the_file() {
    let bytes = icns();
    let total = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    assert_eq!(
        total,
        bytes.len(),
        "declared container length disagrees with the file on disk"
    );
}
