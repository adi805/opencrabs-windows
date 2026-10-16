---
document_type: project-decision-log
project: "OpenCrabs Windows Desktop Control and ACP Bridge"
status: active
created: "2026-10-03"
---

# OpenCrabs Windows Desktop Control and ACP Bridge Decision Log

Use this file as the target project's source of truth for approved and proposed product, architecture, implementation, security, and operational decisions. Do not record PRD Toolkit policy here.

## Entry Format

```markdown
- [YYYY-MM-DD HH:mm] Decision: [short title]
  Status: Proposed | Accepted | Reopened | Superseded
  Approval: [Owner evidence or N/A - not yet accepted]
  Requirement IDs: [FR-###, NFR-###, or N/A - governance-only decision]
  Context: [why the decision was needed]
  Rationale: [why this option is preferred]
  Impact: [what changes because of this decision]
```

## Decisions

- [2026-10-01 11:20] Decision: Use a kernel-held lock for profile exclusion on Windows
  Status: Accepted
  Approval: Merged upstream as the lock change following the filed issue
  Requirement IDs: FR-001
  Context: Two processes opening the same profile store corrupt it, and an advisory lock file survives a crash, leaving the profile locked with no owner.
  Rationale: A kernel-held lock is released by the operating system when the holder dies, so a crashed process cannot strand the profile, and ownership is provable from the handle.
  Impact: The second owner is refused with a message naming the holder, and a crash is followed by a successful start rather than a manual unlock step.

- [2026-10-01 12:05] Decision: Resolve scheduled jobs through the platform command interpreter
  Status: Accepted
  Approval: Merged upstream as the scheduled-job change following the filed issue
  Requirement IDs: FR-002
  Context: Jobs written for a POSIX host failed on Windows only because the interpreter named in the command does not exist there.
  Rationale: Resolving the interpreter per platform keeps one job definition portable instead of forcing two variants.
  Impact: Job definitions no longer name a shell directly, and a missing interpreter is reported as a resolved failure.

- [2026-10-02 04:40] Decision: Take the relay target from environment variables instead of compiling it in
  Status: Accepted
  Approval: Owner instruction to ship one artifact without host information baked in
  Requirement IDs: FR-006, NFR-001
  Context: An early relay revision embedded the operator's host and remote binary path, which made the published artifact operator-specific and leaked infrastructure detail.
  Rationale: A run-time target lets one artifact serve every operator and removes host names and user names from the shipped binary entirely.
  Impact: The relay reads two variables, and a missing target stops the run with a setup instruction instead of defaulting to a host.

- [2026-10-02 05:10] Decision: Log per-direction byte receipts and ship a self-test mode
  Status: Accepted
  Approval: Owner requirement that a failed handshake be diagnosable without guessing
  Requirement IDs: FR-007
  Context: An editor handshake failed silently with no client-side error, and the server logs showed authentication succeeding and the remote process starting, so the fault was in a layer neither side reported.
  Rationale: Counting bytes per direction localizes the fault to a specific hop, and the self-test exercises the same pump without an editor, so the transport can be checked in isolation.
  Impact: Every start writes receipt lines, and the diagnostic path no longer depends on reproducing the failure through the editor.

- [2026-10-01 09:15] Decision: Keep the Windows lane in its own workflow file and make skipped build steps fail the job
  Status: Accepted
  Approval: Merged upstream as the gate change following the filed issue
  Requirement IDs: FR-005
  Context: A path-filtered job concluded success while its build and smoke steps were skipped, so Windows-only code could merge without ever being compiled.
  Rationale: A separate file cannot conflict with the other Windows pull requests, and a job that fails on skipped steps turns a false green into a visible red.
  Impact: The lane produces a real artifact and a real version receipt, and a skipped step is a failure rather than a pass.

- [2026-10-02 06:30] Decision: Adopt the republish rule for release artifacts
  Status: Accepted
  Approval: Adopted after two incorrect freshness claims were retracted
  Requirement IDs: NFR-003
  Context: Replacing an asset on a published release left the published digest describing the old bytes, and the upload step reported success while keeping the previous asset.
  Rationale: Regenerating the digest from the real files and re-downloading them is the only check that does not depend on trusting the upload step.
  Impact: Every publish ends with a re-download and a digest check, and a mismatch is a release defect rather than a documentation note.

- [2026-10-02 07:05] Decision: Keep the parked Windows service branch in the fork
  Status: Proposed
  Approval: N/A - not yet accepted, blocked on a Windows host and a maintainer decision
  Requirement IDs: FR-002
  Context: The upstream maintainer parked the Windows lane, and the open question is whether the service is a scheduled task or a service wrapper.
  Rationale: Dropping the branch discards research that a Windows host would immediately validate, while merging it without validation would ship an unverified control path.
  Impact: The branch stays out of main, and the question is recorded as the PRD's review focus until a host exists.

- [2026-10-16 02:10] Decision: Hold the upstream channel turn and flow cluster back, and record it
  Status: Accepted
  Approval: Merged as the #263 upstream sync, which took the channel layer's declarations from the fork's side; #264 recorded the same reason for the tests that pair with these modules
  Requirement IDs: N/A - governance-only decision
  Context: The #263 sync deposited eight upstream channel modules (`discord/{turns,tracked_turn,options_host}.rs`, `slack/{flow_progress,options_host,resume_turn}.rs`, `whatsapp/{flow_message,flow_render}.rs`, 2,065 lines) as byte-identical copies while the fork's own `mod.rs` files won the declaration conflict, so the files sit in the tree and nothing compiles them. None is reachable from any fork code, and upstream reaches them from its own `handler`/`resume`/`interactions`, which the fork has since rewritten.
  Rationale: Declaring them is not a one-line change and is not a fix. `discord/turns.rs` needs `DiscordState::active_turns`, `discord/tracked_turn.rs` needs `channels::background_work::FlowOutcome`, and `whatsapp/flow_message.rs` needs `whatsapp::followups::send_suggestions`; none exists in the fork. And CI runs `clippy -- -D warnings`, so declaring a module nothing calls fails the build on `dead_code` rather than shipping quietly. Porting the cluster is therefore a feature job against the fork's most diverged area, which does not belong in a release that exists to close a finding list.
  Impact: The eight files stay uncompiled and are listed, with this reason, in `src/tests/undeclared_source_modules_test.rs`, which fails when a source file is reachable from no declaration and is not recorded. The port stays an open follow-up rather than an unstated gap, and the same check covers the deferred test modules.

## AI Agent Rules

1. Read this file before proposing a target-project architecture or policy change.
2. Append decisions; do not silently rewrite prior entries.
3. Mark a decision `Accepted` only with explicit owner approval evidence.
4. Mark an earlier decision `Superseded` instead of deleting it.
5. Do not mix PRD Toolkit governance with this target project's decisions.
