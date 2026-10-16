#!/usr/bin/env python3
r"""Fail CI when a config field is declared but nothing in production reads it.

Why this exists. A config key that nothing reads is worse than a missing
feature: it parses, it defaults, it validates, and it does exactly nothing.
The operator sets it, sees no error, and waits for an effect that can never
arrive. That is indistinguishable from "the traffic is quiet tonight".

Three of these shipped in one month:

  * `channels.whatsapp.bot_owner` -- the allow-list the channel actually
    routed on was `allowed_phones[0]`, so setting `bot_owner` was a no-op that
    read as configuration.
  * `channels.telegram.silence_group_start` -- documented, defaulted to true,
    and pinned by 13 assertions in a defaults test, with zero production
    readers. The tests passed because the VALUE was right; nothing asserted
    that the value was ever consulted.
  * `[channels.google_chat]` / `[channels.imessage]` -- `enabled = true` for a
    channel this build has no module for.

The defaults test is what made the first two survive review, so this gate does
not look at values at all. It looks for `.field`, which is how Rust reads a
struct field, and nothing else.

Scope, chosen to stay cheap and to stay honest about false negatives:

  * Declaration side: only `config/types.rs` and `config/types/**/*.rs`. Other
    files under `config/` hold runtime helpers, not operator-facing keys, and
    their structs are not knobs.
  * Reader side: a dot-prefixed use anywhere under `--root` except `tests/`,
    minus assignments. `.field = x` is a write, not a read, and a knob that is
    only ever written (the `OPENCRABS_*` env overrides do exactly this) is as
    dead as one with no reference at all. `+=` and `==` still count, because
    both read the field.
  * `field: value` in a literal and the declaration itself do not match `.field`,
    which is deliberate: an initializer is not a reader, and that is exactly the
    shape that made `silence_group_start` look alive.
  * References in comments or doc prose count as readers. A comment naming the
    key is a false negative, and a false negative here is a miss, never a false
    alarm. The gate is a floor, not a proof.
  * A field name that is also a common English word (`name`, `model`, `path`)
    matches unrelated code and will not be flagged. Same trade: misses, no
    noise.

A finding means one of three things, in this order of preference: wire it,
delete it, or add it to KNOWN_DEAD with the fix that removes it (or to
ALLOW_UNREADABLE with the reason it cannot have a dot reader).
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

# Fields known to be dead, each with a link to the work that removes it. This is
# the gate's escape hatch, and it is deliberately visible: a reviewer sees every
# knob that has been granted a pass, and adding an entry is a decision someone
# made rather than a finding someone silenced. Anything here that outlives its
# fix is a bug in this list.
KNOWN_DEAD: dict[str, str] = {
    # Empty, and it should stay that way: every entry that lived here was
    # removed by the fix it named. `google_chat` and `imessage` (#255) and
    # `silence_group_start` (#254) had already outlived theirs, which is the
    # bug this table warns about, and `signal`, `debug_lsp` and `profiling`
    # went with #260. A non-empty table means work in flight, not a standing
    # exemption.
}

# Fields with no `.field` reader that are NOT dead: consulted by serde, by
# `Default`, or by name from a string. Each carries the reason it cannot have a
# dot reader.
ALLOW_UNREADABLE: dict[str, str] = {}

DECL_SUFFIX = "config/types.rs"
DECL_DIR = "config/types"


def _strip_noise(line: str) -> str:
    """Drop a trailing `//` comment so prose cannot fake a reader."""
    out = []
    in_str = False
    escaped = False
    for i, ch in enumerate(line):
        if in_str:
            out.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_str = False
            continue
        if ch == '"':
            in_str = True
            out.append(ch)
            continue
        if ch == "/" and i + 1 < len(line) and line[i + 1] == "/":
            break
        out.append(ch)
    return "".join(out)


def declared_fields(root: Path) -> list[tuple[str, int, str]]:
    """`(file, lineno, name)` for every `pub` field of a config struct.

    Only structs named `*Config` count. `config/types/loader.rs` also declares
    runtime types (`ConfigLoadStatus`), whose fields are status flags rather than
    operator-facing keys: flagging those would be scope creep dressed up as a
    finding.
    """
    found: list[tuple[str, int, str]] = []
    for path in sorted(root.rglob("*.rs")):
        rel = path.as_posix()
        if not (rel.endswith(DECL_SUFFIX) or f"/{DECL_DIR}/" in rel):
            continue
        in_struct = False
        for lineno, raw in enumerate(path.read_text(errors="replace").splitlines(), 1):
            line = _strip_noise(raw)
            m = re.match(r"\s*pub struct (\w+)", line)
            if m:
                in_struct = m.group(1).endswith("Config")
                continue
            if not in_struct:
                continue
            if line.strip() == "}":
                in_struct = False
                continue
            f = re.match(r"\s*pub (\w+):", line)
            if f:
                found.append((rel, lineno, f.group(1)))
    return found


def load_counts(root: Path) -> tuple[dict[str, int], dict[str, int]]:
    """Tally every `.identifier` in the tree, once, split by prod/tests.

    One pass with one regex, then a dict lookup per field. Counting per field
    instead would re-scan the whole tree 346 times, which is the difference
    between seconds and minutes on a gate that runs on every PR.
    """
    use = re.compile(r"\.([A-Za-z_]\w*)\b(?!\s*=(?!=))")
    prod: dict[str, int] = {}
    tests: dict[str, int] = {}
    for path in root.rglob("*.rs"):
        rel = path.as_posix()
        lines = path.read_text(errors="replace").splitlines()
        text = "\n".join(_strip_noise(l) if "//" in l else l for l in lines)
        bucket = tests if "/tests/" in rel else prod
        for m in use.finditer(text):
            name = m.group(1)
            bucket[name] = bucket.get(name, 0) + 1
    return prod, tests


def scan(root: Path) -> list[tuple[str, int, str, int, int]]:
    prod, tests = load_counts(root)
    findings = []
    for rel, lineno, name in declared_fields(root):
        if name in KNOWN_DEAD or name in ALLOW_UNREADABLE:
            continue
        p = prod.get(name, 0)
        if p == 0:
            findings.append((rel, lineno, name, p, tests.get(name, 0)))
    return findings


FIXTURE_DECL = """\
pub struct DemoConfig {
    pub wired_knob: bool,
    /// Mentions dead_knob in prose, which must not count as a reader.
    pub dead_knob: bool,
    pub written_knob: bool,
}
"""

FIXTURE_READER = """\
fn wire(cfg: &mut DemoConfig) -> bool {
    cfg.demo.written_knob = false;
    cfg.demo.wired_knob
}
"""

FIXTURE_TEST = """\
fn t(cfg: &DemoConfig) -> bool {
    // A test-only reference is the signature, not a licence.
    cfg.demo.dead_knob
}
"""


def self_test() -> int:
    """Prove the two answers on a fixture whose verdict is known by hand."""
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "src"
        (root / "config").mkdir(parents=True)
        (root / "channels" / "tests").mkdir(parents=True)
        (root / "config" / "types.rs").write_text(FIXTURE_DECL)
        (root / "channels" / "wire.rs").write_text(FIXTURE_READER)
        (root / "channels" / "tests" / "demo_test.rs").write_text(FIXTURE_TEST)
        got = [(n, p, t) for _, _, n, p, t in scan(root)]
    ok = got == [("dead_knob", 0, 1), ("written_knob", 0, 0)]
    if not ok:
        print(f"self-test FAILED: expected dead_knob and written_knob, got {got}",
              file=sys.stderr)
        return 1
    print("self-test: wired field passes, prose and write-only field are "
          "flagged, 3 cases", file=sys.stderr)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="config dead-knob gate")
    ap.add_argument("--root", default="src", type=Path)
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--self-test", action="store_true",
                    help="run the built-in fixtures and exit")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if not args.root.is_dir():
        print(f"error: {args.root} is not a directory", file=sys.stderr)
        return 2

    findings = scan(args.root)
    for rel, lineno, name, prod, tests in findings:
        print(f"{rel}:{lineno}: dead-knob: `{name}` has no production reader "
              f"(prod={prod}, test={tests}); wire it, delete it, or add it to "
              f"KNOWN_DEAD in scripts/check-dead-config-knobs.py with the fix that "
              f"removes it")
    if not args.quiet:
        decls = len(declared_fields(args.root))
        print(f"{decls} config field(s), {len(KNOWN_DEAD) + len(ALLOW_UNREADABLE)} allowlisted, "
              f"{len(findings)} finding(s)", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())