//! Tell the model how to format for Discord.
//!
//! Discord renders markdown, but it has no table markup. A pipe table
//! therefore reaches the channel as its own source, `| File | Delta |` over
//! `|---|---|`, in a proportional font where no column can line up. On a phone
//! the long paths wrap mid-row, so the column relationships are lost outright
//! and the table reads as noise.
//!
//! `table_convert::tables_to_discord` repairs that for text the channel sends
//! itself, but a table the model never emits is a table that never needs
//! repairing. Asked explicitly to format for Discord, the model produces the
//! right shape: section headers, key-value lines, status glyphs, and no tables
//! at all. It was never told. This is that instruction, made permanent.
//!
//! The guidance names constructs Discord actually honours, so the prompt and
//! the renderer agree instead of the model guessing at a dialect.

/// Formatting rules appended to the Discord channel preamble.
///
/// Kept as one block so the wording lives in a single place, and out of
/// `handler.rs`, where it would be a second multi-line string literal inside
/// an already long function.
pub(crate) const DISCORD_FORMATTING: &str = "\
[Discord formatting - your answer is rendered as Discord markdown, which has \
NO table markup. Format for it deliberately:\n\
\n\
- NEVER use markdown tables. Discord has no table syntax, so pipes and dashes \
render literally as text and nothing aligns. Present tabular data as \
key-value lines instead: a bold label, then the value in `backticks`. For a \
before/after, write it inline: `RestartCount:` 272 -> 0\n\
- Bold is **double asterisks**, italic is *single asterisks*, strikethrough \
is ~~double tildes~~.\n\
- Start a heading line with #, ## or ###. Discord renders three levels.\n\
- Put every literal in `backticks`: commands, paths, file names, IDs, config \
keys, numeric readings. It is monospace and it survives copy-paste.\n\
- Use a status glyph where a reader scans for pass/fail: ✅ done, ⚠️ caveat, \
🟡 pending, ❌ failed.\n\
- Bulleted lists use a leading dash. A line starting with > renders as a quote \
block, which is the right shape for a callout.\n\
- Multi-line code, logs and command output go in a fenced block. Everything \
else should not.\n\
\n\
Write for someone scanning on a phone: lead with the outcome, keep paragraphs \
short, and let the structure carry the reading order.]";

/// The preamble line plus the formatting rules.
///
/// `channel_id` is surfaced so the agent can target this channel for cron
/// reports and cross-surface sends without guessing (#533).
pub(crate) fn discord_preamble(channel_id: u64) -> String {
    format!(
        "[Channel: Discord (channel_id: {channel_id}) - your text response is automatically \
         sent to this channel. Do NOT call discord_send to deliver your answer. Only use \
         discord_send for: sending to a different channel, embeds, reactions, threads, \
         files, or moderation.]\n{DISCORD_FORMATTING}\n"
    )
}
