#!/usr/bin/env python3
"""Produce a lite AndroidManifest.xml: the accessibility service is removed.

Why this exists
---------------
Play Protect refuses a sideloaded install whose manifest declares one of four
sensitive capabilities, and BIND_ACCESSIBILITY_SERVICE is one of them. The
refusal is a hard stop: the dialog offers only "OK", with no "install anyway".
It keys on the DECLARATION, not on the signing certificate, so re-signing the
APK does not lift it (measured 2026-10-24: a debug-signed and a release-signed
build produced the identical dialog).

The only lever left on our side is the declaration itself. Dropping it yields
a build that installs without asking the user to disable scanning, at the cost
of the automation bridge, which cannot work without that permission.

The service is stripped together with its explanatory comment block, because
that comment spells BIND_ACCESSIBILITY_SERVICE. Leaving it behind would keep
the string in the manifest and defeat a grep-based check.

Usage
-----
    strip-lite-manifest.py <in.xml> <out.xml>

Exit 0 on success. Exit 1 on a failed guard: a "lite" build that still declares
the service is worse than no build at all, because it fails on the phone with a
dialog the user cannot dismiss.
"""

import re
import sys

REQUIRED_SURVIVORS = ("<application", "</application>", "</manifest>", "BootReceiver")
FORBIDDEN_TOKENS = (
    "BIND_ACCESSIBILITY_SERVICE",
    "CrabsAccessibilityService",
    "accessibility_service_config",
)


def strip(text: str) -> tuple[str, list[str]]:
    """Return (lite_xml, errors). errors non-empty means the build must fail."""
    errors: list[str] = []

    service = re.search(
        r"[ \t]*<service\b(?:(?!</service>).)*CrabsAccessibilityService.*?</service>[ \t]*\n",
        text,
        re.S,
    )
    if service is None:
        return text, ["cannot find the CrabsAccessibilityService element"]

    out = text[: service.start()] + text[service.end() :]

    # The comment directly above the service explains the permission and names
    # it, so it has to go too.
    comment = re.search(
        r"[ \t]*<!--(?:(?!-->).)*BIND_ACCESSIBILITY_SERVICE(?:(?!-->).)*-->[ \t]*\n",
        out,
        re.S,
    )
    if comment is not None:
        out = out[: comment.start()] + out[comment.end() :]

    for token in FORBIDDEN_TOKENS:
        if token in out:
            errors.append(f"lite manifest still mentions {token}")

    before = text.count("<service")
    after = out.count("<service")
    if after != before - 1:
        errors.append(f"expected one <service> to disappear, {before} -> {after}")

    for survivor in REQUIRED_SURVIVORS:
        if survivor not in out:
            errors.append(f"lite manifest lost {survivor}")

    return out, errors


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: strip-lite-manifest.py <in.xml> <out.xml>", file=sys.stderr)
        return 2

    src, dst = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as handle:
        text = handle.read()

    out, errors = strip(text)
    if errors:
        for problem in errors:
            print(f"::error::{problem}", file=sys.stderr)
        return 1

    with open(dst, "w", encoding="utf-8") as handle:
        handle.write(out)

    print(
        f"lite manifest: {len(text)} -> {len(out)} bytes, "
        f"{text.count('<service')} -> {out.count('<service')} services"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())