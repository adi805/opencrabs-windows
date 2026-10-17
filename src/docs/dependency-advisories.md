# Dependency advisory review

Evidence record for the twelve advisories that `cargo audit` suppresses in the
`audit` job of `.github/workflows/ci.yml`. The workflow comment next to the
`--ignore` list is the summary; this file is the durable record, so the
comment does not have to carry the whole argument.

Nine were reviewed on 2026-10-09; the `zip` trio was added on 2026-10-17.

| | |
|---|---|
| Reviewed | 2026-10-09, re-reviewed 2026-10-17 |
| `Cargo.lock` | `9c5b1372` |
| Advisory source | `rustsec/advisory-db` (`patched`, `unaffected`, `informational`); `64f92cdc` for the zip trio |
| Graph source | `Cargo.lock` reverse dependencies, recomputed for this review |
| Feature source | `Cargo.toml` |

Static review only. Nothing here establishes runtime exploitability; the
reachability column says what was and was not verified, and by what.

## Method

1. Read the resolved version of each advisory crate out of `Cargo.lock`, not
   from the comment. The comment is prose that can drift; the lock is the
   graph that ships.
2. Read `patched`, `unaffected` and `informational` from the RustSec advisory
   itself, so "there is no fix" is a quoted field rather than an assumption.
3. Recompute reverse dependencies from `Cargo.lock` for every advisory crate,
   so each parent path in the table is a lock fact and not a memory.
4. Read the feature that gates each path out of `Cargo.toml`.
5. For the zip trio, grep the resolved version of every parent crate in
   `~/.cargo/registry/src` for the advisory's API, and grep this tree for the
   same, so "not reachable" is a search that ran rather than an assumption.

## What the 2026-10-09 review changed

The workflow comment, dated 2026-09-04, closes with:

> The nine below are each at their upstream's LATEST published release,
> verified against crates.io on 2026-09-04, so there is no version to move to
> from here.

That is right for eight of the nine. For `lru` it is the wrong reason, and the
difference matters, because it changes what would lift the suppression:

- `lru` **does** have fixed releases: `>= 0.16.3` for RUSTSEC-2026-0002 and
  `>= 0.18.2` for RUSTSEC-2026-0253.
- Both fixes are **already in the graph**. `lru 0.18.4` arrives through
  `ratatui-core 0.1.2` and `azul-layout 0.0.14`, and 0.18.4 is unaffected by
  both advisories.
- The affected instance is `lru 0.12.5`, pulled by `kalosm-language-model
  0.4.1`, which requires `lru ^0.12.3`. For a `0.x` crate that range excludes
  every patched release, so a `[patch.crates-io]` substitution to 0.16+ is
  rejected by cargo rather than merely unnecessary.
- `kalosm-language-model 0.4.1` arrives through `rwhisper 0.4.1`, the latest
  published release of that crate, behind the `local-stt` feature.

So the suppression still stands, but the accurate reason is "the parent's
requirement range excludes the fix", not "no fix exists". The workflow comment
is corrected to say that.

`h2` needs no correction. The advisory's fix is `>= 0.4.16`; the graph carries
`h2 0.4.19` (patched, via `hyper 1.11.1`) **and** `h2 0.3.27` (affected, via
`hyper 0.14.32` and `reqwest 0.11.27`). The 0.3 line is end-of-life and has no
fixed release, so "no version to move to" is literally true here.

## The zip trio (added 2026-10-17)

`cargo audit` went red on 2026-10-17 with 12 findings: three advisories times
four resolved `zip` versions. They landed in the advisory database at commit
`64f92cdc` (authored 2026-10-16T19:44:19Z, committed 2026-10-16T23:27:42Z),
about ten hours after the last green CI run on `main` (`fb9eb9f12`, created
2026-10-16T12:40:22Z). So the red is a new advisory, not a change in this tree:
the PR that first hit it touches neither `Cargo.toml` nor `Cargo.lock`.

| Advisory | CVSS | What fires |
|---|---|---|
| RUSTSEC-2026-0367 | 7.3 high | Extraction preserves SUID, SGID and sticky bits from the archive instead of masking them |
| RUSTSEC-2026-0368 | none assigned | `ZipArchive::extract()` creates symlinks whose attacker-chosen target is outside the destination |
| RUSTSEC-2026-0369 | 9.1 critical | `ZipStreamReader::extract()` / `ZipArchive::extract()` apply Unix permissions through symlinks without confining the target |

All three are patched only in `zip >= 9.0.0`, and the graph cannot reach it:

| zip | Pulled in by | Parent at its latest release? |
|---|---|---|
| 1.1.4 | `candle-core 0.8.4` (via `rwhisper 0.4.1`, feature `local-stt`) | yes, `rwhisper 0.4.1` is latest and pins `candle-core 0.8.4` |
| 6.0.0 | direct dependency, `Cargo.toml:200` | it is ours; `9.0.4` exists but is an API break |
| 7.2.0 | `rust_xlsxwriter 0.96.0` | no, 0.99.1 exists and still requires `zip ^8.3` |
| 8.6.0 | `calamine 0.36.1`, `docx-rs 0.4.22` | yes, both latest, both require `zip ^8` |

A maximal upgrade therefore still leaves 8.6.0 and 1.1.4 in the lock, which
means version bumps alone cannot make this gate green. Only `zip 6.0.0` is ours
to move, and moving it changes nothing about the audit result.

### Why they are ignored: reachability, not an unmaintained parent

All three advisories fire inside an extraction API that writes entries to disk.
Nothing in this tree calls one:

- Our five `zip` call sites are `src/rtk/rewrite.rs:409`,
  `src/utils/file_extract.rs:675`, `src/brain/tools/doc_parser.rs:392`,
  `src/brain/tools/evolve/archive.rs:45` and `src/cli/crash_recovery.rs:302`.
  All five open with `ZipArchive::new`, then use `by_index`/`by_name` and
  `read_to_end`/`read_to_string` into memory. None calls `extract()`.
- The four crates that pull `zip` in were grepped at their resolved versions
  for `.extract(`, `ZipStreamReader` and `unpack`: zero hits in `candle-core`,
  `calamine`, `docx-rs` and `rust_xlsxwriter`. They read model weights,
  spreadsheets and DOCX parts, and write archives, but never extract one to
  disk.

One near-miss, recorded so it is not re-filed as a finding:
`src/config/profile.rs:617` does call `unpack()`, but on `tar::Archive` (line
582, `use tar::Archive;`), not `zip`. `tar` is a different crate with its own
path sanitisation, and no `zip` advisory applies to it.

This is a static argument. It says the vulnerable API has no caller in this
tree. It does not claim the advisories are harmless in general, and it says
nothing about runtime exploitability.

### Verified for this review

| Check | Result |
|---|---|
| `cargo audit` with the nine older ignores only | exit 1, `error: 12 vulnerabilities found!`, all 12 are `zip` |
| `cargo audit` with those nine plus the three zip ignores | exit 0, no findings |
| `Cargo.lock` / `Cargo.toml` touched by the PR that first failed | no, the diff is empty |

## The twelve advisories

| Advisory | Crate | Lock | Kind | Patched | Pulled in by | Feature |
|---|---|---|---|---|---|---|
| RUSTSEC-2024-0436 | paste | 1.0.15 | unmaintained | none | gemm, gemm-*, metal 0.27/0.29, tokenizers 0.21.4 | local-stt, local-tts |
| RUSTSEC-2025-0119 | number_prefix | 0.4.0 | unmaintained | none | indicatif 0.17.11 | local-stt |
| RUSTSEC-2025-0134 | rustls-pemfile | 1.0.4 | unmaintained | none | reqwest 0.11.27 | local-stt |
| RUSTSEC-2025-0141 | bincode | 1.3.3 | unmaintained | none | syntect 5.3.0, hyphenation 0.8.4 | default (syntect is direct), pdfium |
| RUSTSEC-2026-0002 | lru | 0.12.5 | unsound (memory corruption) | >= 0.16.3 | kalosm-language-model 0.4.1 | local-stt |
| RUSTSEC-2026-0173 | proc-macro-error2 | 2.0.1 | unmaintained | none | aquamarine 0.6.0 | telegram |
| RUSTSEC-2026-0192 | ttf-parser | 0.25.1 | unmaintained | none | lopdf 0.42.0 | pdfium |
| RUSTSEC-2026-0253 | lru | 0.12.5 | unsound (use-after-free) | >= 0.18.2 | kalosm-language-model 0.4.1 | local-stt |
| RUSTSEC-2026-0258 | h2 | 0.3.27 | vulnerability (DoS, low) | >= 0.4.16 | hyper 0.14.32, reqwest 0.11.27 | local-stt |
| RUSTSEC-2026-0367 | zip | 1.1.4, 6.0.0, 7.2.0, 8.6.0 | privilege escalation (SUID/SGID) | >= 9.0.0 | candle-core 0.8.4, direct, rust_xlsxwriter 0.96.0, calamine 0.36.1 + docx-rs 0.4.22 | local-stt, default |
| RUSTSEC-2026-0368 | zip | 1.1.4, 6.0.0, 7.2.0, 8.6.0 | privilege escalation (symlink escape) | >= 9.0.0 | same four as above | local-stt, default |
| RUSTSEC-2026-0369 | zip | 1.1.4, 6.0.0, 7.2.0, 8.6.0 | privilege escalation (permissions via symlink) | >= 9.0.0 | same four as above | local-stt, default |

For the nine older advisories every crate is a transitive dependency: none is
declared in `Cargo.toml`, so the only lever is an upstream release or dropping
the parent. `syntect` is a direct dependency, but the advisory is on `bincode`
underneath it, and `syntect 5.3.0` requires `bincode ^1`. The zip trio breaks
that pattern: `zip = "6.0"` is declared directly at `Cargo.toml:200`, so one of
the four affected instances is ours to move. That move is a separate change
that has to compile, not something this suppression review can do.

## Reachability

Reachability is the part a version table cannot answer. Each row states the
mechanism and what would have to be true for it to fire.

| Advisory | Mechanism | What must be true to fire | Verified here |
|---|---|---|---|
| lru 2026-0002 | `IterMut::next`/`next_back` create an exclusive reference to the key, invalidating the shared pointer held by the internal `HashMap` (Stacked Borrows) | Code must call `lru::LruCache::iter_mut()` and dereference the yielded key | no |
| lru 2026-0253 | `LruCache::pop()` is not panic-safe: a panicking `Drop` on the key skips `detach()`, leaving a dangling node that a later eviction writes through | Unwinding panics enabled, `catch_unwind` around the call, and key types whose `Drop` can panic | no |
| h2 2026-0258 | Accepts and queues empty DATA frames without limit; unbounded memory if streams are not drained | A hostile HTTP/2 endpoint we are already talking to, plus undrained streams | no |
| zip 2026-0367/0368/0369 | Extraction writes entries to disk, applying archive-supplied permission bits and symlink targets without confining them | Code must call `ZipArchive::extract()` or `ZipStreamReader::extract()` on an archive the attacker controls, in a process whose privileges are worth escalating | yes: zero callers in this tree and zero in the four parent crates |
| six unmaintained | No code defect is claimed by the advisory. The risk is that a future vulnerability in the crate goes unpatched | A vulnerability must be found in the crate after upstream stopped maintaining it | n/a |

The `lru` and `h2` entries are the only older ones carrying a defect class. All
three sit behind `local-stt` (`rwhisper` -> `kalosm`), which is in the default
feature set, so they are compiled into every default build rather than only
into `--all-features` builds. That is why the suppressions are documented here
instead of being treated as an exotic configuration. The zip instances reach
the default build the same way: through `local-stt`, through the direct
dependency, and through the document tooling.

## Disposition

All twelve suppressions are retained. For the six unmaintained advisories the
advisory itself declares no patched release, so there is nothing to move to.
For `h2 0.3.27` the affected line is end-of-life. For `lru 0.12.5` the fix
exists but the parent's `^0.12` requirement excludes it. For the zip trio the
fix exists at `9.0.0` but two parents at their latest releases still require
`^8`, and the API the advisories describe has no caller in this tree.

## What would lift each suppression

| Trigger | Clears |
|---|---|
| `rwhisper` / `kalosm` move to a `reqwest` on `hyper 1.x` | h2 2026-0258, rustls-pemfile 2025-0134, number_prefix 2025-0119, paste 2024-0436 |
| `kalosm-language-model` widens `lru` past `^0.12` | lru 2026-0002, lru 2026-0253 |
| `teloxide` moves off `aquamarine` | proc-macro-error2 2026-0173 |
| `pdf-extract` / `lopdf` move off `ttf-parser`, or `ttf-parser` gains a maintainer | ttf-parser 2026-0192 |
| `syntect` moves to `bincode 2`, and `printpdf` drops `azul-layout` | bincode 2025-0141 |
| `calamine` and `docx-rs` move to `zip 9`, `rust_xlsxwriter` widens past `^8`, and `rwhisper` moves off `candle-core 0.8.4` | zip 2026-0367, 2026-0368, 2026-0369 |
| `local-stt` is dropped from the default feature set | every `rwhisper`-rooted row above, plus zip 1.1.4 |

`cargo audit` fails on any advisory that is not in the ignore list, so a new
advisory cannot be silenced by accident: it has to be added to the list in a
reviewable diff. This document is the record for the twelve that are there.
