# Dependency advisory review

Evidence record for the nine advisories that `cargo audit` suppresses in the
`audit` job of `.github/workflows/ci.yml`. The workflow comment next to the
`--ignore` list is the summary; this file is the durable record, so the
comment does not have to carry the whole argument.

| | |
|---|---|
| Reviewed | 2026-10-09 |
| `Cargo.lock` | `62358e30` |
| Advisory source | `rustsec/advisory-db` (`patched`, `unaffected`, `informational`) |
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

## What this review changes

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

## The nine advisories

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

Every advisory crate is a transitive dependency: none is declared in
`Cargo.toml`, so the only lever is an upstream release or dropping the parent.
`syntect` is a direct dependency, but the advisory is on `bincode` underneath
it, and `syntect 5.3.0` requires `bincode ^1`.

## Reachability

Reachability is the part a version table cannot answer, and the part this host
cannot answer from source: there is no vendored crate source and no `cargo` on
the machine that produced this review, so nothing below is a call-graph claim.
Each row states the mechanism and what would have to be true for it to fire.

| Advisory | Mechanism | What must be true to fire | Verified here |
|---|---|---|---|
| lru 2026-0002 | `IterMut::next`/`next_back` create an exclusive reference to the key, invalidating the shared pointer held by the internal `HashMap` (Stacked Borrows) | Code must call `lru::LruCache::iter_mut()` and dereference the yielded key | no |
| lru 2026-0253 | `LruCache::pop()` is not panic-safe: a panicking `Drop` on the key skips `detach()`, leaving a dangling node that a later eviction writes through | Unwinding panics enabled, `catch_unwind` around the call, and key types whose `Drop` can panic | no |
| h2 2026-0258 | Accepts and queues empty DATA frames without limit; unbounded memory if streams are not drained | A hostile HTTP/2 endpoint we are already talking to, plus undrained streams | no |
| six unmaintained | No code defect is claimed by the advisory. The risk is that a future vulnerability in the crate goes unpatched | A vulnerability must be found in the crate after upstream stopped maintaining it | n/a |

The `lru` and `h2` entries are the only ones carrying a defect class. All three
sit behind `local-stt` (`rwhisper` -> `kalosm`), which is in the default feature
set, so they are compiled into every default build rather than only into
`--all-features` builds. That is why the suppressions are documented here
instead of being treated as an exotic configuration.

## Disposition

All nine suppressions are retained. For the six unmaintained advisories the
advisory itself declares no patched release, so there is nothing to move to.
For `h2 0.3.27` the affected line is end-of-life. For `lru 0.12.5` the fix
exists but the parent's `^0.12` requirement excludes it.

## What would lift each suppression

| Trigger | Clears |
|---|---|
| `rwhisper` / `kalosm` move to a `reqwest` on `hyper 1.x` | h2 2026-0258, rustls-pemfile 2025-0134, number_prefix 2025-0119, paste 2024-0436 |
| `kalosm-language-model` widens `lru` past `^0.12` | lru 2026-0002, lru 2026-0253 |
| `teloxide` moves off `aquamarine` | proc-macro-error2 2026-0173 |
| `pdf-extract` / `lopdf` move off `ttf-parser`, or `ttf-parser` gains a maintainer | ttf-parser 2026-0192 |
| `syntect` moves to `bincode 2`, and `printpdf` drops `azul-layout` | bincode 2025-0141 |
| `local-stt` is dropped from the default feature set | every `rwhisper`-rooted row above |

`cargo audit` fails on any advisory that is not in the ignore list, so a new
advisory cannot be silenced by accident: it has to be added to the list in a
reviewable diff. This document is the record for the nine that are there.
