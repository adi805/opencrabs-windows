#!/bin/sh
# Install this repository's git hooks into the clone or worktree you run this
# from. A hook file committed to the repository does nothing until it is
# installed, so every agent working environment needs this run once.
#
#     sh scripts/install-hooks.sh
#
# Idempotent: re-run it after .githooks/ changes. Worktrees of the same clone
# share one common git directory, so installing once covers all of them.
set -eu

if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "install-hooks: not inside a git repository." >&2
    exit 1
fi

cd "$(git rev-parse --show-toplevel)"
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
hooks_dir="$common_dir/hooks"
mkdir -p "$hooks_dir"

installed=0
for hook in .githooks/*; do
    [ -f "$hook" ] || continue
    name=$(basename "$hook")
    cp -f "$hook" "$hooks_dir/$name"
    chmod +x "$hooks_dir/$name"
    echo "install-hooks: $name -> $hooks_dir/$name"
    installed=$((installed + 1))
done

if [ "$installed" = "0" ]; then
    echo "install-hooks: nothing to install (.githooks/ holds no file)." >&2
    exit 1
fi

echo "install-hooks: $installed hook(s) installed in $hooks_dir"
