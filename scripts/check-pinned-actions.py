#!/usr/bin/env python3
"""check-pinned-actions: every remote `uses:` must be a full commit SHA.

A tag or branch ref is mutable. `@v2`, `@latest` and `@master` all resolve on
the runner to whatever that ref points at *now*, so whoever controls the ref can
change what executes inside this repo without a commit here. A 40-hex commit
cannot move, which is why every third-party action in this repo is pinned and
why this check exists: a pin without a guard is a pin that regresses the next
time someone copies a snippet out of an action's README.

Local refs (`./path`) and container refs (`docker://...`) are exempt: they are
not remote git refs, and the failure mode this guards against does not apply.

Exit 0 when every remote ref is a commit SHA, 1 otherwise.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

WORKFLOWS = pathlib.Path(".github/workflows")

# `uses: owner/repo@ref`, allowing a leading `- ` (list item) and a trailing
# comment. The ref is whatever follows the last `@`.
USES = re.compile(r"^\s*(?:-\s+)?uses:\s*(?P<value>[^\s#]+)\s*(?:#.*)?$")

FULL_SHA = re.compile(r"^[0-9a-f]{40}$")


def check_line(value: str) -> str | None:
    """Return a violation message, or None when the ref is acceptable."""
    if value.startswith("./") or value.startswith("docker://"):
        return None
    if "@" not in value:
        return f"'{value}' has no ref; a bare action name resolves to the default branch"
    ref = value.rsplit("@", 1)[1]
    if FULL_SHA.match(ref):
        return None
    return f"'{value}' is pinned to '{ref}', which is a mutable ref; use a full commit SHA"


def scan(root: pathlib.Path = WORKFLOWS) -> list[tuple[pathlib.Path, int, str]]:
    findings: list[tuple[pathlib.Path, int, str]] = []
    for path in sorted(root.glob("*.y*ml")):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            match = USES.match(line)
            if match is None:
                continue
            if (problem := check_line(match.group("value"))) is not None:
                findings.append((path, number, problem))
    return findings


def self_test() -> int:
    cases = [
        ("uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09", None),
        ("- uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5", None),
        ("      - uses: swatinem/rust-cache@v2", "mutable"),
        ("uses: lukka/get-cmake@latest", "mutable"),
        ("uses: dtolnay/rust-toolchain@master", "mutable"),
        ("uses: actions/checkout", "no ref"),
        ("uses: ./local-action", None),
        ("uses: docker://alpine:3.20", None),
        ("run: echo 'uses: fake@v1'", None),
    ]
    failed = 0
    for line, want in cases:
        match = USES.match(line)
        got = None if match is None else check_line(match.group("value"))
        if want is None:
            ok = got is None
        elif want == "mutable":
            ok = got is not None and "mutable ref" in got
        else:
            ok = got is not None and "no ref" in got
        if not ok:
            failed += 1
            print(f"self-test FAIL: {line!r} -> {got!r}, wanted {want!r}")
    if failed:
        print(f"check-pinned-actions: {failed} self-test failure(s).", file=sys.stderr)
        return 1
    print(f"check-pinned-actions: self-test passed ({len(cases)} cases).")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="run the built-in cases and exit")
    parser.add_argument("--root", default=str(WORKFLOWS), help="workflow directory to scan")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    root = pathlib.Path(args.root)
    if not root.is_dir():
        print(f"::error::no workflow directory at {root}", file=sys.stderr)
        return 1

    findings = scan(root)
    for path, number, problem in findings:
        print(f"::error file={path},line={number}::{problem}")
    if findings:
        print(
            f"\ncheck-pinned-actions: {len(findings)} unpinned action ref(s). "
            "Resolve the ref with `gh api repos/<owner>/<repo>/commits/<ref> --jq .sha` "
            "and keep the tag in a trailing comment.",
            file=sys.stderr,
        )
        return 1

    print(f"check-pinned-actions: every remote ref under {root} is a commit SHA.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
