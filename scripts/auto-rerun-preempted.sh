#!/usr/bin/env bash
# Rerun a CI run once when the only failures are GitHub runner preemptions.
#
# Why this exists: hosted runners occasionally get their VM shut down mid-job.
# The runner agent then prints "The runner has received a shutdown signal" and
# exits 143 (128 + SIGTERM). The job dies before a single test writes a result
# line, so the red check says nothing about the code. Between 2026-10-23 and
# 2026-10-24 that happened five times in a row across five different runners,
# and the same suite passed on the sixth attempt with no code change.
#
# The dangerous half is the opposite case: a real assertion failure must NEVER
# be rerun, because a green retry would hide a broken test. So a rerun happens
# only when EVERY failed job carries the preemption signature in its log. One
# failed job without it, or a log we cannot read, means no rerun.
#
# Usage: auto-rerun-preempted.sh [RUN_ID] [REPO]
#   RUN_ID and REPO may also come from the environment, which is how the
#   workflow passes them. When REPO is absent it is detected with
#   `gh repo view`, so a local DRY_RUN against a historical run needs no setup.
# Env: REPO (owner/name), RUN_ID, MAX_ATTEMPTS (default 6), DRY_RUN (1 = report only)
set -uo pipefail

log() { printf '%s\n' "$*"; }

RUN_ID="${1:-${RUN_ID:-}}"
REPO="${2:-${REPO:-}}"

if [ -z "$REPO" ]; then
    REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null) || true
fi
if [ -z "$REPO" ]; then
    REPO=$(git remote get-url origin 2>/dev/null \
        | sed -E 's#^git@[^:]+:##; s#^https?://[^/]+/##; s#\.git$##') || true
fi
if [ -z "$REPO" ]; then
    log "FAIL: cannot determine repo; pass it as the second argument or set REPO"
    exit 1
fi
if [ -z "$RUN_ID" ]; then
    log "FAIL: run id required; pass it as the first argument or set RUN_ID"
    exit 1
fi

# Total attempts allowed, counting manual reruns. Six, not three: the
# 2026-10-23/24 preemption streak needed exactly six attempts before the same
# suite passed with no code change, so a lower cap would have abandoned a run
# that was going to go green. The cap still exists so a permanently broken
# runner cannot loop forever.
MAX_ATTEMPTS="${MAX_ATTEMPTS:-6}"
DRY_RUN="${DRY_RUN:-0}"

# Markers the runner agent itself emits when its VM is torn down. Both come
# from the agent, never from test output, so they cannot be spoofed by a test
# that merely prints "143".
PREEMPT_MARKERS='has received a shutdown signal|Process completed with exit code 143'

# An aggregator job (ci-gate) fails *because* another job failed, so counting
# it as a real failure would veto every rerun. It is identified two ways: by
# name, and by the relay line it prints. If ci.yml's gate message is ever
# reworded, update RELAY_MARKERS with it; the name check is the backstop.
RELAY_JOB_NAMES="${RELAY_JOB_NAMES:-CI Required Gate}"
RELAY_MARKERS="${RELAY_MARKERS:-One or more CI jobs failed or were cancelled}"

log() { printf '%s\n' "$*"; }

# Fetch a job log. gh refuses to print logs containing terminal escape
# sequences unless --allow-escape-sequences is passed (it exits 1 with an
# empty body otherwise, which reads exactly like "log not found"). Older gh
# builds lack the flag, so fall back rather than dying.
fetch_log() {
    local job_id="$1"
    if gh api "repos/$REPO/actions/jobs/$job_id/logs" --allow-escape-sequences 2>/dev/null; then
        return 0
    fi
    gh api "repos/$REPO/actions/jobs/$job_id/logs" 2>/dev/null
}

run_json=$(gh api "repos/$REPO/actions/runs/$RUN_ID" 2>/dev/null) || {
    log "FAIL: cannot read run $RUN_ID (check token scope: actions:read)"
    exit 1
}

conclusion=$(printf '%s' "$run_json" | jq -r '.conclusion // "null"')
attempt=$(printf '%s' "$run_json" | jq -r '.run_attempt // 1')
head_sha=$(printf '%s' "$run_json" | jq -r '.head_sha // "unknown"')
head_branch=$(printf '%s' "$run_json" | jq -r '.head_branch // "unknown"')
event=$(printf '%s' "$run_json" | jq -r '.event // "unknown"')

log "run=$RUN_ID event=$event branch=$head_branch sha=${head_sha:0:9} attempt=$attempt conclusion=$conclusion"

# Only a genuinely failed run is a candidate. success/skipped/cancelled are
# either fine or deliberate (cancelled = concurrency superseding this run).
case "$conclusion" in
    failure|timed_out) ;;
    *)
        log "SKIP: conclusion is '$conclusion', nothing to repair"
        exit 0
        ;;
esac

if [ "$attempt" -ge "$MAX_ATTEMPTS" ]; then
    log "SKIP: attempt $attempt reached the cap of $MAX_ATTEMPTS, leaving this to a human"
    exit 0
fi

# Collect failed jobs. Paginate: a matrix run can exceed the 30-per-page default.
jobs_json=$(gh api "repos/$REPO/actions/runs/$RUN_ID/jobs?per_page=100" --paginate 2>/dev/null) || {
    log "FAIL: cannot list jobs for run $RUN_ID"
    exit 1
}

failed_count=$(printf '%s' "$jobs_json" | jq -s '[.[].jobs[] | select(.conclusion == "failure")] | length')
if [ "$failed_count" -eq 0 ]; then
    log "SKIP: no job concluded 'failure'"
    exit 0
fi
log "failed jobs: $failed_count"

# Every failed job must be either preempted or a pure relay. A single real
# failure vetoes the rerun.
preempted=0
real_failures=0
relayed=0
while IFS=$'\t' read -r job_id job_name; do
    [ -n "$job_id" ] || continue

    # Aggregator by name: no log fetch needed.
    case " $RELAY_JOB_NAMES " in
        *" $job_name "*)
            log "  relay: '$job_name' ($job_id)"
            relayed=$((relayed + 1))
            continue
            ;;
    esac

    body=$(fetch_log "$job_id")
    if [ -z "$body" ]; then
        # Unreadable log is ambiguous, not evidence. Rerunning blind could
        # paper over a real failure, so stop and say so.
        log "STOP: log for job '$job_name' ($job_id) is unreadable; not rerunning blind"
        exit 0
    fi

    if printf '%s' "$body" | grep -Eq "$PREEMPT_MARKERS"; then
        log "  preempted: '$job_name' ($job_id)"
        preempted=$((preempted + 1))
    elif printf '%s' "$body" | grep -Fq "$RELAY_MARKERS"; then
        # Aggregator by behaviour: it only reported someone else's failure.
        log "  relay: '$job_name' ($job_id)"
        relayed=$((relayed + 1))
    else
        log "  REAL FAILURE: '$job_name' ($job_id) has no shutdown/143 marker"
        real_failures=$((real_failures + 1))
    fi
done <<EOF
$(printf '%s' "$jobs_json" | jq -rs '.[].jobs[] | select(.conclusion == "failure") | "\(.id)\t\(.name)"')
EOF

if [ "$real_failures" -gt 0 ]; then
    log "SKIP: $real_failures failed job(s) failed for real; a rerun would hide them"
    exit 0
fi

if [ "$preempted" -eq 0 ]; then
    log "SKIP: no preempted job found (relayed=$relayed); nothing a rerun would fix"
    exit 0
fi

log "all $preempted failed job(s) were runner preemptions (relayed=$relayed)"

if [ "$DRY_RUN" = "1" ]; then
    log "DRY_RUN: would rerun failed jobs of run $RUN_ID (attempt $attempt -> $((attempt + 1)))"
    exit 0
fi

# POST is the action itself, not a probe. rerun-failed-jobs keeps green jobs
# green and only restarts what died, so a rerun costs minutes not a full cycle.
if gh api -X POST "repos/$REPO/actions/runs/$RUN_ID/rerun-failed-jobs" >/dev/null 2>&1; then
    log "RERUN: restarted failed jobs of run $RUN_ID (attempt $attempt -> $((attempt + 1)))"
else
    log "FAIL: rerun request rejected for run $RUN_ID"
    exit 1
fi
