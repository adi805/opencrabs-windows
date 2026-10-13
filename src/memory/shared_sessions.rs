//! The shared/group session gate for memory results (#1051, ADR-003;
//! internal surfaces joined via #1957; durable classification #217).
//!
//! The gate must know whether the current session is a shared/group chat
//! (several people can read the reply) or the owner's own session. Channel
//! handlers know that when they resolve a session (they see the chat type)
//! so they mark it here; `memory_search` checks it before returning external
//! content, and since #1957 the internal surfaces (brain/memory scopes,
//! personal brain files through `load_brain_file`, per-turn MEMORY.md
//! recall) consult the same decision through `internal_content_blocked`.
//!
//! #217: the in-memory mark is a fast path, not the answer. It used to be the
//! only answer, and absence read as "not shared", i.e. private. That is
//! fail-open, and it fired exactly when it mattered: boot recovery dispatches
//! interrupted channel turns straight to the resume path, bypassing ingress,
//! so a resumed guild turn could prepare context before any inbound message
//! re-marked it. Classification is therefore tri-state
//! ([`SessionSharing`]), `Unknown` is a real answer, the gate treats it as
//! shared for personal content, and the durable column on `sessions` lets a
//! resumed turn read back what ingress decided in an earlier process.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use uuid::Uuid;

/// Session IDs known to be shared/group channel sessions.
static SHARED_SESSIONS: LazyLock<Mutex<HashSet<Uuid>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Session IDs known to be the owner's own (DM or local) session.
static PERSONAL_SESSIONS: LazyLock<Mutex<HashSet<Uuid>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// What this process knows about one session's audience (#217).
///
/// `Unknown` is not "private by default". It means nothing has classified the
/// session in this process and no durable classification was readable, so the
/// safe reading is the shared one: several people *might* be able to read the
/// reply, and the owner's personal context must not ride along on a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSharing {
    /// Several people can read the reply.
    Shared,
    /// Only the owner can read the reply.
    Personal,
    /// Never classified, and not readable from storage.
    Unknown,
}

impl SessionSharing {
    /// The durable spelling written to `sessions.sharing`.
    pub(crate) fn as_str(self) -> Option<&'static str> {
        match self {
            SessionSharing::Shared => Some("shared"),
            SessionSharing::Personal => Some("personal"),
            SessionSharing::Unknown => None,
        }
    }

    /// Parse the durable spelling. Anything unrecognised, including a NULL
    /// column, is `Unknown` rather than a guess.
    pub(crate) fn from_durable(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("shared") => SessionSharing::Shared,
            Some("personal") => SessionSharing::Personal,
            _ => SessionSharing::Unknown,
        }
    }
}

/// Mark a session as a shared/group channel session (#1051). Called by the
/// channel handlers when they resolve a session for a group chat.
pub fn mark_session_shared(session_id: Uuid) {
    if let Ok(mut g) = SHARED_SESSIONS.lock() {
        g.insert(session_id);
    }
    if let Ok(mut g) = PERSONAL_SESSIONS.lock() {
        g.remove(&session_id);
    }
}

/// Mark a session as the owner's own (#217). Called by the channel handlers
/// for a DM and by session creation for a local session, so that "not shared"
/// is an explicit answer instead of the absence of one.
pub fn mark_session_personal(session_id: Uuid) {
    if let Ok(mut g) = PERSONAL_SESSIONS.lock() {
        g.insert(session_id);
    }
    if let Ok(mut g) = SHARED_SESSIONS.lock() {
        g.remove(&session_id);
    }
}

/// What this process knows about the session right now, without touching
/// storage. See [`restore_sharing`] for the durable answer.
pub fn session_sharing(session_id: Uuid) -> SessionSharing {
    if let Ok(g) = SHARED_SESSIONS.lock()
        && g.contains(&session_id)
    {
        return SessionSharing::Shared;
    }
    if let Ok(g) = PERSONAL_SESSIONS.lock()
        && g.contains(&session_id)
    {
        return SessionSharing::Personal;
    }
    SessionSharing::Unknown
}

/// Whether a session is a KNOWN shared/group channel session (#1051).
///
/// Strict on purpose: this is the predicate the external-content gate reads,
/// and it answers only what it can prove. Use [`restore_sharing`] or
/// [`session_sharing`] when the difference between "private" and "unknown"
/// matters, because this returns `false` for both.
pub fn is_session_shared(session_id: Uuid) -> bool {
    matches!(session_sharing(session_id), SessionSharing::Shared)
}

/// Mark a session shared AND persist it (#217), so a later process (a restart,
/// a resumed turn, a second instance) reads the same answer back.
pub async fn mark_session_shared_durable(session_id: Uuid) {
    mark_session_shared(session_id);
    persist(session_id, SessionSharing::Shared).await;
}

/// Mark a session personal AND persist it (#217). See
/// [`mark_session_shared_durable`].
pub async fn mark_session_personal_durable(session_id: Uuid) {
    mark_session_personal(session_id);
    persist(session_id, SessionSharing::Personal).await;
}

async fn persist(session_id: Uuid, sharing: SessionSharing) {
    let Some(value) = sharing.as_str() else {
        return;
    };
    let Some(pool) = crate::db::global_pool() else {
        // No database in this process (unit tests, a degraded boot). The
        // in-memory mark still stands for the current run.
        return;
    };
    let repo = crate::db::repository::SessionRepository::new(pool.clone());
    if let Err(e) = repo.set_sharing(session_id, value).await {
        tracing::warn!("[MEMORY] could not persist session sharing for {session_id}: {e}");
    }
}

/// Resolve the session's classification for the current turn, reading the
/// durable column when this process has no mark (#217).
///
/// Called before the personal-content gates so a resumed or cold-start turn
/// gets the same answer ingress decided in an earlier process. A missing or
/// unreadable value stays [`SessionSharing::Unknown`], which the gate treats
/// as shared for personal content: absence is not privacy.
pub async fn restore_sharing(session_id: Uuid) -> SessionSharing {
    let known = session_sharing(session_id);
    if known != SessionSharing::Unknown {
        return known;
    }
    let Some(pool) = crate::db::global_pool() else {
        return SessionSharing::Unknown;
    };
    let repo = crate::db::repository::SessionRepository::new(pool.clone());
    let restored = match repo.sharing_of(session_id).await {
        Ok(raw) => SessionSharing::from_durable(raw.as_deref()),
        Err(e) => {
            tracing::warn!("[MEMORY] could not read session sharing for {session_id}: {e}");
            SessionSharing::Unknown
        }
    };
    match restored {
        SessionSharing::Shared => mark_session_shared(session_id),
        SessionSharing::Personal => mark_session_personal(session_id),
        SessionSharing::Unknown => {}
    }
    restored
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
