# AGENTS.md - working rules for this repository

This file is for AI coding agents working in `adi805/opencrabs-windows`.
`CONTRIBUTING.md` is the human-facing guide and still applies; read it too.
`governance.toml` is the machine-readable source of truth and
`scripts/policy-lint.py` enforces it on every PR: where prose and those two
disagree, the machine wins. Where `CONTRIBUTING.md` and this file disagree about
the checks below, this file wins, because those are mechanical gates rather than
style preferences.

## Formatting is a precondition, not a reminder

`cargo fmt --all -- --check` is a hard CI gate (`.github/workflows/ci.yml`,
"Check formatting", #120). It is not advisory. A diff there fails the whole
`Lint` job **before** clippy runs, so a single unformatted line hides every
clippy verdict in that round and costs a full CI cycle to discover.

Before every commit or push:

1. `cargo fmt --all`
2. `cargo fmt --all -- --check`
3. `git diff` - review what the formatter changed before staging it
4. If either check fails, do not commit and do not push. Fix it first.
5. If you cannot run the checks, say the change is **UNVERIFIED**. Never claim CI
   will pass when you did not verify formatting yourself.

`cargo fmt` takes the edition from `Cargo.toml` (2024). `.rustfmt.toml` still
pins `edition = "2021"`, so a bare `rustfmt --check <file>` reports phantom
import-order diffs. Pass the manifest edition explicitly.

### The hook enforces it

`.githooks/pre-push` runs the check and refuses the push when it fails. Git does
not pick it up from the repository: it has to be installed, once per working
environment.

```bash
sh scripts/install-hooks.sh
```

Run that in every clone and worktree you push from, and re-run it after
`.githooks/` changes. To verify it is live:

```bash
hooks=$(git rev-parse --path-format=absolute --git-common-dir)/hooks
ls -l "$hooks/pre-push"
```

Never push with `--no-verify` to get past it. If the hook refuses, fix the
formatting, or report the push as unverified. Do not reorder imports by hand to
match a formatter you did not run.

### When cargo is not installed

The STB agent host ships `rustc` + `rustfmt` without `cargo`. The hook then
drives `rustfmt` from the crate roots (`src/lib.rs`, `src/main.rs`, `build.rs`,
`examples/*.rs`) with the manifest edition, which visits the same files
`cargo fmt --all` does.

Do not "fix" the fallback by handing `rustfmt` a raw list of changed files.
`src/tests/mod.rs` keeps several `//pub mod ...` entries commented out, so those
files are outside the module tree: `rustfmt <file>` lints them anyway and reports
14 phantom diffs that `cargo fmt --all` never sees.

## Do not weaken the gate

Do not set `continue-on-error` on the formatting step, do not add a path to an
ignore list to silence a real diff, and do not delete a failing check to make a
PR green. Fix the code.

## The other hard gates

`cargo clippy --locked --lib --bins --tests --examples --all-features -- -D
warnings` and the test suite are hard gates too, and the Windows runtime slice is
blocking (#150). The exact commands, and why clippy rather than `cargo check`, are
in `CONTRIBUTING.md` ("Build & Test").
