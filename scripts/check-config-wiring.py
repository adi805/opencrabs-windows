#!/usr/bin/env python3
"""Config-wiring linter: a config field nobody reads is a dead knob.

Signature it catches: a `pub field` declared in the config schema whose name
appears nowhere outside the declaring file and the test tree.

That shape bit three times in the 2026-10-15 audit:

  * `channels.telegram.silence_group_start` -- declared, defaulted to true, and
    pinned by 13 assertions in the config tests, with ZERO production readers.
    The doc comment described behaviour the code never had, so the operator who
    set it got no error, no warning and no effect. Those 13 assertions made it
    look wired, which is what makes this shape worse than a plainly unused
    field.
  * `channels.google_chat` and `channels.imessage` -- schema placeholders that
    read as enabled channels and do nothing.

Deliberately lexical. Resolving real usages needs the type checker; this runs
in seconds and catches the shape that actually occurred, which is a field name
that is never mentioned again. A field read only by another config module
counts as read, since that is still behaviour changing.

Usage:
    python3 scripts/check-config-wiring.py --root src
    python3 scripts/check-config-wiring.py --self-test
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

# `pub name:` (a field), not `pub fn name` / `pub mod name` / `pub(crate) x:`
FIELD_RE = re.compile(r"^\s*pub\s+([a-z_][a-z0-9_]*)\s*:", re.MULTILINE)

# Fields whose readers land in a PR that has not merged yet, plus fields the
# lexical rule cannot judge. Each entry is a dated debt marker, and the run
# reports it as stale the moment a reader appears, so the list cannot rot
# quietly.
#
# 2026-10-15:
#   * #254 wires `silence_group_start`, #255 warns about the two placeholder
#     channels. Drop each entry once its PR is in `main`.
#   * `autofixed` is not a config knob: it is a field of `ConfigLoadStatus`, the
#     value `Config::load_with_status` returns. It is written by the loader (the
#     declaring file, which the reader scan excludes) and consumed by callers.
#     The lexical rule cannot tell a returned DTO field from a knob.
#   * `debug_lsp` is genuinely dead as of 2026-10-15: `[debug] debug_lsp` and its
#     `OPENCRABS_DEBUG_LSP` override are both write-only, and no LSP debug
#     logging exists to switch on. Wire it or drop the field -- a config-compat
#     decision, not a drive-by -- so it is parked here with a reason rather than
#     silently passed over.
KNOWN_UNREAD = {
    "silence_group_start": "wired by #254",
    "google_chat": "warned about by #255",
    "imessage": "warned about by #255",
    "autofixed": "ConfigLoadStatus field, not a config knob",
    "debug_lsp": "dead knob awaiting a wire-or-remove decision (2026-10-15)",
}


def declaring_files(root: Path) -> list[Path]:
    """Config modules that declare schema fields.

    `types.rs` is the bulk of the schema; `types/` holds the parts split out by
    concern (#1525-era refactor), so both are scanned.
    """
    files = []
    types_rs = root / "config" / "types.rs"
    if types_rs.is_file():
        files.append(types_rs)
    files.extend(sorted((root / "config" / "types").glob("*.rs")))
    return files


def declared_fields(files: list[Path]) -> dict[str, str]:
    """Field name -> the file that declares it. First declaration wins."""
    out: dict[str, str] = {}
    for path in files:
        for name in FIELD_RE.findall(path.read_text(encoding="utf-8", errors="replace")):
            out.setdefault(name, path.name)
    return out


def reader_files(root: Path, skip: set[Path]) -> list[Path]:
    """Production Rust sources: everything under `root` minus the declarers and
    the test tree."""
    out = []
    for path in sorted(root.rglob("*.rs")):
        if path in skip:
            continue
        rel = path.relative_to(root)
        if rel.parts and rel.parts[0] == "tests":
            continue
        out.append(path)
    return out


def scan(root: Path) -> tuple[list[tuple[str, str]], list[str]]:
    """Return (unread, stale): fields with no production reader, and allowlist
    entries that have since gained one."""
    declarers = declaring_files(root)
    skip = set(declarers)
    sources = reader_files(root, skip)
    text = "\n".join(p.read_text(encoding="utf-8", errors="replace") for p in sources)

    unread: list[tuple[str, str]] = []
    for name, where in sorted(declared_fields(declarers).items()):
        if re.search(r"\b" + re.escape(name) + r"\b", text):
            continue
        unread.append((name, where))

    stale = sorted(n for n in KNOWN_UNREAD if not any(n == u for u, _ in unread))
    return unread, stale


def report(unread: list[tuple[str, str]], stale: list[str]) -> int:
    blocking = [u for u in unread if u[0] not in KNOWN_UNREAD]
    for name, where in blocking:
        print(
            f"error: config field '{name}' (declared in {where}) has no reader outside the "
            f"config schema and the test tree -- setting it changes nothing, silently.\n"
            f"       Either wire it into the code that should honour it, or remove the field "
            f"and its default.",
            file=sys.stderr,
        )
    for name in stale:
        # Not a failure: a stale entry costs nothing at runtime. Reported so the
        # list gets pruned instead of quietly outliving its PR.
        print(
            f"note: '{name}' now has a reader -- remove it from KNOWN_UNREAD "
            f"({KNOWN_UNREAD[name]})."
        )
    if blocking:
        print(f"\n{len(blocking)} dead config field(s).", file=sys.stderr)
        return 1
    print(f"config wiring OK ({len(unread)} allowlisted, {len(stale)} stale).")
    return 0


def self_test() -> int:
    """Prove the linter fails on a dead knob and passes once it is read."""
    failures = []

    def build(sources: dict[str, str]) -> Path:
        tmp = Path(tempfile.mkdtemp(prefix="wiring-selftest-"))
        (tmp / "config" / "types").mkdir(parents=True)
        for rel, body in sources.items():
            path = tmp / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body, encoding="utf-8")
        return tmp

    schema = "pub struct Tg {\n    pub silence_me: bool,\n}\n"

    # 1. A field nobody reads must be reported.
    dead = build(
        {
            "config/types.rs": schema,
            "channels/telegram.rs": "fn f() { let x = 1; }\n",
        }
    )
    unread, _ = scan(dead)
    if [n for n, _ in unread] != ["silence_me"]:
        failures.append(f"dead knob not detected, got {unread}")

    # 2. A production reader clears it.
    wired = build(
        {
            "config/types.rs": schema,
            "channels/telegram.rs": "fn f(c: Tg) -> bool { c.silence_me }\n",
        }
    )
    unread, _ = scan(wired)
    if unread:
        failures.append(f"wired field still flagged, got {unread}")

    # 3. A reader that exists ONLY in the test tree is still a dead knob: that
    #    is the shape that shipped `silence_group_start`.
    test_only = build(
        {
            "config/types.rs": schema,
            "tests/config_test.rs": "fn t(c: Tg) { assert!(c.silence_me); }\n",
        }
    )
    unread, _ = scan(test_only)
    if [n for n, _ in unread] != ["silence_me"]:
        failures.append(f"test-only reader treated as wired, got {unread}")

    for message in failures:
        print(f"self-test FAILED: {message}", file=sys.stderr)
    if failures:
        return 1
    print("self-test OK (3 cases)")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, help="source root to scan (e.g. src)")
    parser.add_argument("--self-test", action="store_true", help="prove the linter has teeth")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if not args.root:
        parser.error("--root is required unless --self-test is given")
    if not args.root.is_dir():
        print(f"error: {args.root} is not a directory", file=sys.stderr)
        return 2

    unread, stale = scan(args.root)
    return report(unread, stale)


if __name__ == "__main__":
    sys.exit(main())