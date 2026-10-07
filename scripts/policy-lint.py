#!/usr/bin/env python3
"""policy-lint: enforce the machine-readable workflow rules (#1934).

`governance.toml` at the repo root is the source of truth; this script is the
only thing that reads it. It runs as the `policy-lint` job in
`.github/workflows/ci.yml`, on pull requests.

Two modes:

  policy-lint.py --self-test
      Prove the checks have teeth, and that the committed policy parses.
      Runs in CI before the real pass, so a rule that silently stopped
      matching fails the job instead of passing everything.

  policy-lint.py --base <sha> --head <sha>
      Lint every non-merge commit in `base..head`, and warn on the PR's diff
      size. Exit 1 on a FAIL rule; warnings never fail.

Scope fence: only what a machine can see without judgement. Tone, scope
creep, and "is this one problem?" stay in prose review forever.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
GOVERNANCE = REPO_ROOT / "governance.toml"

# A `Co-authored-by:` trailer — the shape AGENTS.md forbids. Anchored at line
# start so the phrase in ordinary prose does not trip it.
COAUTHOR_RE = re.compile(r"^[ \t]*co-authored-by[ \t]*:", re.IGNORECASE | re.MULTILINE)

# Generation banners a tool leaves behind. Narrow on purpose: these match a
# banner or a bot author, not the words "Claude" / "AI" in ordinary prose.
AI_BRANDING_RES = (
    re.compile(r"generated\s+with\s+\[?claude", re.IGNORECASE),
    re.compile(r"\U0001f916\s*generated", re.IGNORECASE),
    re.compile(r"noreply@anthropic\.com", re.IGNORECASE),
    re.compile(r"generated\s+by\s+(claude|chatgpt|copilot|an?\s+ai\b)", re.IGNORECASE),
)

# `type(scope)!: subject` / `type: subject`.
CONVENTIONAL_RE = re.compile(
    r"^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)"
    r"(\([^()]+\))?!?: \S"
)


# ── pure checks (exercised by --self-test) ───────────────────────────────────


def check_coauthor(message: str) -> str | None:
    """The offending trailer line, or None when `message` has no trailer."""
    m = COAUTHOR_RE.search(message)
    if m is None:
        return None
    return message[m.start() :].splitlines()[0].strip()


def check_ai_branding(message: str) -> str | None:
    """The matched branding marker, or None."""
    for pattern in AI_BRANDING_RES:
        m = pattern.search(message)
        if m is not None:
            return m.group(0)
    return None


def check_conventional(subject: str) -> bool:
    return CONVENTIONAL_RE.match(subject) is not None


# ── git plumbing ─────────────────────────────────────────────────────────────


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO_ROOT, check=True, capture_output=True, text=True
    ).stdout

def merge_base(base: str, head: str) -> str:
    """The fork point, so `base..head` is the PR's own commits.

    The PR event carries the *current* tip of the base branch, which moves
    while the PR is open. Diffing against that tip would blame this PR for
    commits that landed on main after it branched, and `--no-merges` would
    not hide them: they are ordinary commits. The merge base is stable.
    """
    return git("merge-base", base, head).strip()


def load_policy(path: Path = GOVERNANCE) -> dict:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def commits_between(base: str, head: str) -> list[tuple[str, str, str]]:
    """`(sha, subject, body)` for every non-merge commit in `base..head`."""
    raw = git("log", "--no-merges", "--format=%H%x1f%s%x1f%b%x1e", f"{base}..{head}")
    out: list[tuple[str, str, str]] = []
    for record in raw.split("\x1e"):
        record = record.strip()
        if not record:
            continue
        sha, _, rest = record.partition("\x1f")
        subject, _, body = rest.partition("\x1f")
        out.append((sha, subject, body))
    return out


def changed_files(base: str, head: str) -> list[str]:
    return [f for f in git("diff", "--name-only", f"{base}...{head}").splitlines() if f]


# ── the lint pass ────────────────────────────────────────────────────────────


def lint(policy: dict, base: str, head: str) -> int:
    commits_cfg = policy.get("commits", {})
    prs_cfg = policy.get("prs", {})
    # The PR event carries the base branch's *current* tip, which moves while
    # the PR is open. Resolve the fork point first so the walk sees this PR's
    # own commits and not everything that landed on main after it branched.
    base = merge_base(base, head)
    commits = commits_between(base, head)
    failures: list[str] = []

    for sha, subject, body in commits:
        short = sha[:9]
        message = f"{subject}\n\n{body}"
        if commits_cfg.get("no_coauthor_trailers"):
            if (line := check_coauthor(message)) is not None:
                failures.append(
                    f"::error::{short}: commit carries a co-author trailer "
                    f"(forbidden by governance.toml [commits] no_coauthor_trailers): {line!r}"
                )
        if commits_cfg.get("no_ai_branding"):
            if (hit := check_ai_branding(message)) is not None:
                failures.append(
                    f"::error::{short}: commit carries an AI branding marker "
                    f"(forbidden by governance.toml [commits] no_ai_branding): {hit!r}"
                )
        if commits_cfg.get("conventional_titles") and not check_conventional(subject):
            failures.append(
                f"::error::{short}: subject is not a conventional commit "
                f"(governance.toml [commits] conventional_titles): {subject!r}"
            )

    files = changed_files(base, head)
    limit = prs_cfg.get("max_files_warn", 0)
    if limit and len(files) > limit:
        print(
            f"::warning::this PR touches {len(files)} files "
            f"(governance.toml [prs] max_files_warn = {limit}) — is it one problem?"
        )

    for line in failures:
        print(line)
    if failures:
        print(f"\npolicy-lint: {len(failures)} violation(s).", file=sys.stderr)
        return 1

    print(
        f"policy-lint: {len(commits)} commit(s), {len(files)} file(s) — no violations."
    )
    return 0


# ── self-test: the linter proves its own teeth ───────────────────────────────


def self_test() -> int:
    cases: list[tuple[str, bool]] = [
        (
            "co-author trailer at line start is caught",
            check_coauthor("fix: a thing\n\nCo-Authored-By: Bot <bot@example.com>")
            is not None,
        ),
        (
            "indented trailer is caught",
            check_coauthor("fix: a thing\n\n  co-authored-by: Bot <b@e.c>") is not None,
        ),
        (
            "the phrase in ordinary prose is not a trailer",
            check_coauthor("docs: explain why we reject co-authored-by: see AGENTS.md")
            is None,
        ),
        (
            "a clean message has no trailer",
            check_coauthor("fix(discord): durable plan card\n\nBody only.") is None,
        ),
        (
            "a generation banner is caught",
            check_ai_branding("feat: x\n\n\U0001f916 Generated with [Claude Code]")
            is not None,
        ),
        (
            "a bot author address is caught",
            check_ai_branding("chore: x\n\nnoreply@anthropic.com") is not None,
        ),
        (
            "the word Claude in prose is not branding",
            check_ai_branding("docs: the Claude model list moved") is None,
        ),
        (
            "conventional subject accepted",
            check_conventional("fix(compaction): bound the continuation document"),
        ),
        (
            "bang form accepted",
            check_conventional("feat(discord)!: drop the legacy card path"),
        ),
        (
            "bare subject rejected",
            check_conventional("updated some stuff") is False,
        ),
        (
            "merge subject rejected",
            check_conventional("Merge pull request #94 from adi805/x") is False,
        ),
    ]

    failed = [name for name, ok in cases if not ok]
    for name, ok in cases:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")

    # The committed policy must parse, or the job is enforcing nothing.
    try:
        policy = load_policy()
    except Exception as exc:  # noqa: BLE001 - surfaced as a test failure
        print(f"FAIL governance.toml does not parse: {exc}")
        failed.append("governance.toml parses")

    for section in ("commits", "prs"):
        if section not in policy:
            print(f"FAIL governance.toml is missing the [{section}] section")
            failed.append(f"[{section}] present")

    if failed:
        print(f"\nself-test: {len(failed)} failure(s).", file=sys.stderr)
        return 1
    print(f"\nself-test: {len(cases)} checks passed.")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="prove the checks bite")
    parser.add_argument("--base", help="base sha (exclusive)")
    parser.add_argument("--head", help="head sha (inclusive)")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    if not args.base or not args.head:
        parser.error("--base and --head are required unless --self-test is given")

    return lint(load_policy(), args.base, args.head)


if __name__ == "__main__":
    sys.exit(main())
