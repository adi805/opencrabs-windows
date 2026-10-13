//! Bounded HTTP response reads (#216).
//!
//! Two fetch paths buffered the whole body before any limit applied:
//! `web_scrape`'s static fetch called `resp.text()` and only truncated the
//! rendered Markdown afterwards, and dynamic HTTP tools did the same. A large
//! or slow chunked response therefore had to arrive in full, in memory, before
//! the tool's own output budget could reject any of it. The limit has to run
//! DURING the read, not after it, or it is a display rule rather than a bound.
//!
//! [`read_body_capped`] streams the body, stops as soon as the cap is reached,
//! and reports whether it cut the body short so callers can say so instead of
//! silently returning a prefix as if it were the whole document.
//!
//! The accumulation rule lives in [`accumulate_capped`], which is pure and
//! synchronous, so the bound is unit-tested against synthetic chunk shapes
//! (including one chunk larger than the whole cap) without a server.

use futures::StreamExt;

/// Default ceiling for a response body we are willing to hold in memory.
///
/// 5 MiB is far above any HTML page or JSON API response these tools are meant
/// for, and far below the point where buffering one would matter on a small
/// host. Callers may pass their own smaller limit.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// A body read under a byte ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyRead {
    /// The bytes read, decoded lossily as UTF-8. Lossy because a cut at the cap
    /// can land inside a multi-byte sequence, and a decoded prefix is more
    /// useful to a model than a hard error.
    pub text: String,
    /// Whether the source had more to give when the cap was reached.
    pub truncated: bool,
    /// How many bytes were actually read.
    pub bytes_read: usize,
}

/// Accumulate chunks under a cap, reporting whether the cap cut the body short.
///
/// Pure: it takes the chunks already decoded to `Vec<u8>` so the rule can be
/// tested directly. Stops consuming as soon as `max_bytes` is reached, which is
/// the whole point: the caller never holds more than the cap.
pub fn accumulate_capped<I>(chunks: I, max_bytes: usize) -> (Vec<u8>, bool)
where
    I: IntoIterator<Item = Vec<u8>>,
{
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    for chunk in chunks {
        if buf.len() >= max_bytes {
            truncated = true;
            break;
        }
        let room = max_bytes - buf.len();
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    (buf, truncated)
}

/// Decode an accumulated body into a [`BodyRead`].
fn into_body_read(buf: Vec<u8>, truncated: bool) -> BodyRead {
    BodyRead {
        bytes_read: buf.len(),
        text: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
    }
}

/// Read a `reqwest` response body under a byte ceiling (#216).
///
/// Returns an error only when the stream itself fails; a truncated body is a
/// SUCCESS with `truncated = true`, because a partial page is still worth
/// processing and the caller needs to know it was partial.
pub async fn read_body_capped(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<BodyRead, String> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("failed to read response body: {e}"))?;
        if buf.len() >= max_bytes {
            truncated = true;
            break;
        }
        let room = max_bytes - buf.len();
        // `Bytes` converts into `Vec<u8>` without naming the crate, so this
        // module needs no direct `bytes` dependency (adding one would change
        // Cargo.lock, and CI runs cargo with `--locked`).
        let chunk: Vec<u8> = chunk.into();
        if chunk.len() > room {
            buf.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(into_body_read(buf, truncated))
}

/// Append a truncation notice to a body that was cut at the cap, so the model
/// is told the document is partial instead of assuming it read all of it.
pub fn note_truncation(body: String, read: &BodyRead, max_bytes: usize) -> String {
    if !read.truncated {
        return body;
    }
    format!(
        "{body}\n\n[truncated: response exceeded the {max_bytes}-byte limit; \
         {} bytes read]",
        read.bytes_read
    )
}
