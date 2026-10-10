//! The shared/group session gate for memory results (#1051, ADR-003;
//! internal surfaces joined via #1957).
//!
//! The gate must know whether the current session is a shared/group chat
//! (several people can read the reply) or the owner's own session. Channel
//! handlers know that when they resolve a session (they see the chat type)
//! so they mark it here; `memory_search` checks it before returning external
//! content, and since #1957 the internal surfaces (brain/memory scopes,
//! personal brain files through `load_brain_file`, per-turn MEMORY.md
//! recall) consult the same decision through `internal_content_blocked`.
//! Process-local by design: on restart the set is
//! empty and each channel re-marks its group sessions on first use, which is
//! harmless because the gate only ever denies until then.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use uuid::Uuid;

/// Session IDs of shared/group channel sessions.
static SHARED_SESSIONS: LazyLock<Mutex<HashSet<Uuid>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Mark a session as a shared/group channel session (#1051). Called by the
/// channel handlers when they resolve a session for a group chat.
pub fn mark_session_shared(session_id: Uuid) {
    if let Ok(mut g) = SHARED_SESSIONS.lock() {
        g.insert(session_id);
    }
}

/// Whether a session is a shared/group channel session (#1051). Consulted by
/// the `memory_search` external gate.
pub fn is_session_shared(session_id: Uuid) -> bool {
    SHARED_SESSIONS
        .lock()
        .map(|g| g.contains(&session_id))
        .unwrap_or(false)
}

/// Whether a brain file carries the owner's personal context and is therefore
/// main-session-only in shared/group sessions (#1957).
///
/// The boundary comes from the brain-file ownership docs themselves: generic
/// files (SOUL/AGENTS/CODE/TOOLS/SECURITY/BOOT) ship the same for everyone
/// and shared sessions legitimately consult them for conventions;
/// USER/MEMORY accumulate per user and stay private; HEARTBEAT is the owner's
/// standing checklist; everything else under the home dir is user-created
/// content, private by default. Anything path-like (memory/... daily logs)
/// is personal too.
pub fn is_personal_brain_file(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    const SHARED_SAFE: [&str; 6] = [
        "soul.md",
        "agents.md",
        "code.md",
        "tools.md",
        "security.md",
        "boot.md",
    ];
    !SHARED_SAFE.contains(&lower.as_str())
}
