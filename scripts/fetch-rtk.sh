#!/usr/bin/env bash
#
# Fetch the pinned RTK release, verify it against a digest compiled into this
# script, and drop the binary into <dest-dir>.
#
# Why the digest lives here and not in the workflow: it is the trust anchor.
# Fetching it from the same release as the artifact proves nothing, because a
# swapped asset would come with a swapped checksum. Keeping the table in one
# place also means the five call sites cannot drift apart, which is how the
# previous inline copies went stale.
#
# Bump RTK_VERSION and every digest together. An unmapped target exits non-zero
# rather than shipping an unverified binary.
#
# Usage: fetch-rtk.sh <rust-target-triple> <dest-dir>

set -euo pipefail

RTK_VERSION="0.40.0"

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <rust-target-triple> <dest-dir>" >&2
  exit 2
fi

target="$1"
dest="$2"

case "$target" in
  x86_64-unknown-linux-gnu)
    asset="rtk-x86_64-unknown-linux-musl.tar.gz"
    sha256="a75d210a445874106bc16da2b4efba01d36d297afa33ec134728f2d5f42ef5af"
    ;;
  aarch64-unknown-linux-gnu)
    asset="rtk-aarch64-unknown-linux-gnu.tar.gz"
    sha256="1d0087ad62a182c0833c2251ac678b5e05356418d91aa57305ac51a126c9b102"
    ;;
  x86_64-apple-darwin)
    asset="rtk-x86_64-apple-darwin.tar.gz"
    sha256="8eac502fb812056973da2a8c2f0c00e1427ba5f71bd14c01520bc540630cb98a"
    ;;
  aarch64-apple-darwin)
    asset="rtk-aarch64-apple-darwin.tar.gz"
    sha256="60c2c325b4edf0367cfa9716ac2e2c888abcd065eff45d01510da6561ab82e3c"
    ;;
  x86_64-pc-windows-msvc)
    asset="rtk-x86_64-pc-windows-msvc.zip"
    sha256="7fc90190f76f55dc170898d0ac755e89f405fc2d1d89f717ad8600640ab0f1ed"
    ;;
  *)
    echo "fetch-rtk: no pinned RTK asset for target '$target'" >&2
    exit 1
    ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

url="https://github.com/rtk-ai/rtk/releases/download/v${RTK_VERSION}/${asset}"
echo "fetch-rtk: downloading ${url}"
# --fail so a 404 lands as a non-zero exit instead of an HTML error page that
# only blows up later, inside tar.
curl -sSL --fail --retry 5 --retry-connrefused -o "$tmp/$asset" "$url"

# sha256sum is GNU coreutils (Linux, and Git for Windows' bash); shasum is what
# macOS runners carry. certutil is the last resort so the check cannot degrade
# into a silent no-op on a runner with neither.
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
elif command -v certutil >/dev/null 2>&1; then
  actual="$(certutil -hashfile "$tmp/$asset" SHA256 | sed -n '2p' | tr -d ' \r' | tr 'A-Z' 'a-z')"
else
  echo "fetch-rtk: no sha256 tool available; refusing to use an unverified artifact" >&2
  exit 1
fi

if [ "$actual" != "$sha256" ]; then
  echo "fetch-rtk: digest mismatch for ${asset}" >&2
  echo "  want ${sha256}" >&2
  echo "  got  ${actual}" >&2
  exit 1
fi

mkdir -p "$tmp/extract"
case "$asset" in
  *.zip) unzip -q "$tmp/$asset" -d "$tmp/extract" ;;
  *) tar xzf "$tmp/$asset" -C "$tmp/extract" ;;
esac

# The archives carry the binary at the top level, but find it rather than assume
# the layout, and accept either name so one path covers every platform.
bin="$(find "$tmp/extract" -type f \( -name rtk -o -name rtk.exe \) | head -1)"
if [ -z "$bin" ]; then
  echo "fetch-rtk: no rtk binary inside ${asset}" >&2
  ls -laR "$tmp/extract" >&2
  exit 1
fi

mkdir -p "$dest"
cp "$bin" "$dest/"
if [ "$target" != "x86_64-pc-windows-msvc" ]; then
  chmod +x "$dest/$(basename "$bin")"
fi

echo "fetch-rtk: ${asset} verified against pinned sha256 ${sha256} -> ${dest}/$(basename "$bin")"
