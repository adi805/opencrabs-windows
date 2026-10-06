# rebuild.sh — mechanical self-rebuild loop for OpenCrabs source maintainers.

# Companion template to the built-in /rebuild removed by #1929 (spec: #1958).
# Install: copy this directory into ~/.opencrabs/skills/rebuild/
#
# The loop is purely mechanical: sync -> detached build -> swap -> setsid
# restart with session continuity -> 3-second insta-death rollback guard.
# No LLM runs anywhere in the loop; the agent only reports the receipt after
# this script exits (its output IS the receipt).
#
# Restart mirrors SelfUpdater::restart_into (src/brain/self_update.rs):
#   <binary> chat --session <uuid>   with OPENCRABS_EVOLVED_FROM set.
# Unix only: exec()/setsid handover. Windows uses the spawn-successor +
# CREATE_NEW_CONSOLE + exit(0) pattern, which a bash script cannot provide.

set -uo pipefail

SRC_DIR="${OPENCRABS_SRC:-$HOME/srv/rs/opencrabs}"
SESSION_ID="${1:-}"
ROLLBACK_SECS="${ROLLBACK_SECS:-3}"
BIN="${OPENCRABS_BIN:-$(command -v opencrabs || true)}"

log() { printf '[rebuild] %s\n' "$*" >&2; }

if [ -z "$SESSION_ID" ]; then
  log "usage: rebuild.sh <session-id>   (arg-passed to 'chat --session', never re-derived)"
  exit 2
fi
if [ -z "$BIN" ]; then
  log "no opencrabs binary on PATH; set OPENCRABS_BIN=/path/to/opencrabs"
  exit 2
fi

cd "$SRC_DIR" || { log "source checkout not found: $SRC_DIR"; exit 2; }

# 1. Sync first — loudly. The old built-in swallowed pull failures (let _ =).
log "syncing source tree"
if ! git fetch origin; then
  log "git fetch failed — aborting before building stale history"
  exit 1
fi
if ! git pull --ff-only origin main; then
  log "pull --ff-only failed (diverged or dirty tree) — aborting"
  exit 1
fi

# 2. Detached build. This script itself is launched as a detached task, so
# the build is watchable while the maintainer keeps working.
log "building (detached task; you can keep working)"
if ! cargo build --release; then
  log "build FAILED — running binary untouched"
  exit 1
fi

NEW_BIN="$SRC_DIR/target/release/opencrabs"
if [ ! -x "$NEW_BIN" ]; then
  log "built binary missing at $NEW_BIN"
  exit 1
fi

# 3. Swap with .bak.
cp "$BIN" "$BIN.bak" || { log "could not back up $BIN"; exit 1; }
cp "$NEW_BIN" "$BIN" || { log "could not install new binary"; exit 1; }
chmod +x "$BIN"
log "binary swapped: $BIN (.bak kept)"

# Portable detach helper: setsid where it exists (Linux), nohup on macOS
# (macOS ships no setsid). Both free the successor from this dying tty; the
# fresh session leader reopens the orphaned tty by path (pty-proven).
spawn_successor() {
  if command -v setsid >/dev/null 2>&1; then
    setsid "$BIN" chat --session "$SESSION_ID" </dev/null >/dev/null 2>&1 &
  else
    nohup "$BIN" chat --session "$SESSION_ID" </dev/null >/dev/null 2>&1 &
  fi
  NEW_PID=$!
  log "successor spawned pid=$NEW_PID (chat --session $SESSION_ID)"
}

# 4. Restart with session continuity (mirror restart_into's invocation:
# "<binary> chat --session <uuid>" with OPENCRABS_EVOLVED_FROM set).
# Informational only: version of the binary being replaced, never a gate.
OLD_VER="$($BIN --version 2>/dev/null | tail -1 | awk '{print $NF}')"
export OPENCRABS_EVOLVED_FROM="${OLD_VER:-unknown}"
spawn_successor

# 5. Insta-death rollback guard.
sleep "$ROLLBACK_SECS"
if ! kill -0 "$NEW_PID" 2>/dev/null; then
  log "new binary died within ${ROLLBACK_SECS}s — rolling back to .bak"
  cp "$BIN.bak" "$BIN"
  spawn_successor
  log "old binary restored and relaunched on the same session"
  exit 1
fi

log "new binary alive after ${ROLLBACK_SECS}s — rebuild complete"
exit 0
