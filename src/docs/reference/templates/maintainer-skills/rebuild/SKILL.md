---
name: rebuild
description: >
  Rebuild OpenCrabs from source and hot-swap the running binary in place.
  Maintainer-only template (source checkout required) replacing the built-in
  /rebuild removed by #1929 — spec in #1958. Detached build, .bak + 3-second
  insta-death rollback, setsid restart with session continuity, self-report
  on completion. Not compiled into the shipped binary; /evolve stays the
  single shipped update path.
---

# Rebuild (maintainer template)

Mechanical loop, exactly like the old built-in: the agent never thinks during
it — it launches, waits, reports.

## The drill

1. Resolve the current session id (it is arg-passed, never re-derived) and
   launch the loop as a DETACHED background task so the maintainer keeps
   working and the task stays watchable:

   ```bash
   bash ~/.opencrabs/skills/rebuild/rebuild.sh <session-id>
   ```

   Optional env: `OPENCRABS_SRC` (default `~/srv/rs/opencrabs`),
   `OPENCRABS_BIN` (default: `opencrabs` on PATH),
   `ROLLBACK_SECS` (default 3).

2. Do nothing until the task completion receipt lands. No LLM in the loop
   before that — the script does sync, build, swap, restart and rollback.

3. On the receipt, read the task log and report ONCE, where the rebuild was
   triggered:
   - `rebuild complete` — new binary swapped, alive past the guard window,
     session resumed via `chat --session <id>`.
   - `rolling back` / exit 1 — the new binary insta-died; old binary was
     restored from `.bak` and relaunched on the same session. Include the
     compiler tail if the build itself failed.
   - exit 2 — usage/environment error (no session id, no binary, no source
     checkout). Fix the input, do not retry blind.

## What the script guarantees

- Sync first: `git fetch` + `git pull --ff-only`, aborting loudly on failure
  (the old built-in swallowed this silently).
- Swap only after a green build; the previous binary always sits at
  `<binary>.bak`.
- Restart mirrors `SelfUpdater::restart_into` (`src/brain/self_update.rs`):
  `<binary> chat --session <uuid>` with `OPENCRABS_EVOLVED_FROM` set,
  detached from the dying tty (setsid on Linux, nohup on macOS) so the fresh
  session leader reopens the orphaned tty by path (pty takeover proven
  2026-10-05).
- 3-second insta-death guard: a successor that dies within the window gets
  rolled back to `.bak` and the old binary relaunched, exit 1.

## Platform

Unix (macOS/Linux) only. Windows restart needs the spawn-successor +
`CREATE_NEW_CONSOLE` + `exit(0)` pattern baked into the binary itself
(`restart_into` cfg(windows) arm) and is out of scope for this bash script.
