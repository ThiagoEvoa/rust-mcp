# Third-Party Notice — rust-docs-mcp bundled documentation

> **M2 seed notice.** The snapshot shipped in `data/bundled/snapshot.json`
> (embedded in the binary via `include_str!`, see `src/bundled.rs`) is the
> **seed corpus**: representative, attributed content that makes a fresh install
> serve the Rust toolchain documentation **offline**, with no network and no
> `sync`. A full, current, revision-pinned corpus is produced by
> `rust-docs-mcp sync` and published to the data directory; this file is the
> human-facing companion to the machine-readable `attribution` records in that
> manifest. Full license text and per-source revision pinning are completed by
> M5 (`docs/LICENSING.md` §8); this seed notice carries the per-source
> attribution the audit established and the operative license clause.

The bundled documentation corpus is derived, under the "MIT OR Apache-2.0, at
your option" clause, from the following Rust Project sources. The operative
license for the bundle is **Apache-2.0**; the MIT text is retained alongside it.
Every source is **MIT OR Apache-2.0** (`docs/LICENSING.md` §4); no source is
copyleft, CC, all-rights-reserved, or carries an upstream `NOTICE`, so none
triggers the first-sync fallback of `docs/LICENSING.md` §5 — the seed omits
nothing (see `src/bundled.rs`, `FIRST_SYNC_REQUIRED`).

Per-source entries use the revision recorded at bundle time. For the **seed**,
each revision is *unknown*: the seed carries representative content and has not
checked an upstream revision, so `last_checked`/`upstream_modified` are absent
and the revision is filled in by the first `sync` (`{rev}` below). No semantic
edits are made to upstream prose (`plan.md` §5); the seed's bodies are
representative distillations, not verbatim upstream prose.

## Sources

| # | Source | Repository | Rendered | Revision | License |
|---|---|---|---|---|---|
| 1 | Rust API Guidelines | https://github.com/rust-lang/api-guidelines | https://rust-lang.github.io/api-guidelines/ | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 2 | Rust Style Guide | https://github.com/rust-lang/rust/tree/HEAD/src/doc/style-guide | https://doc.rust-lang.org/style-guide/ | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 3 | The Cargo Book | https://github.com/rust-lang/cargo | https://doc.rust-lang.org/cargo/ | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 4 | The rustc Book | https://github.com/rust-lang/rust/tree/HEAD/src/doc/rustc | https://doc.rust-lang.org/rustc/ | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 5 | The rustup Book | https://github.com/rust-lang/rustup | https://rust-lang.github.io/rustup/ | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 6 | rustfmt usage & configuration | https://github.com/rust-lang/rustfmt (README.md, Configurations.md) | https://github.com/rust-lang/rustfmt#readme ; https://rust-lang.github.io/rustfmt/ | {commit} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 7 | Clippy documentation & stable lint catalog | https://github.com/rust-lang/rust-clippy | https://doc.rust-lang.org/clippy/ ; https://clippy.rust-lang.org/stable/index.html | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |
| 8 | The rustdoc Book | https://github.com/rust-lang/rust/tree/HEAD/src/doc/rustdoc | https://doc.rust-lang.org/rustdoc/ | {rev} — unknown (seed; recorded at sync) | Apache-2.0 OR MIT |

**Copyright.** Each source is © The Rust Project Developers / Contributors, per
`docs/LICENSING.md` §2. The Cargo Book's LICENSE-MIT/LICENSE-APACHE carry no
in-file copyright line; see `docs/LICENSING.md` §2.

## Operative license clause

The following clause, verbatim, governs the bundled content:

> Each file is licensed under either of Apache License, Version 2.0
> (LICENSE-APACHE) or MIT license (LICENSE-MIT), at your option.

The full text of the **Apache License 2.0** and the **MIT License** is
reproduced in the distribution by M5. No third-party (non-Rust-Project) content
was bundled at audit time; if any is found later, it is excluded per
`docs/LICENSING.md` §5 and recorded with its own notice and license.

## First-sync fallback

The seed is complete: all eight required collections pass fetch, parse,
license, and attribution checks, so it omits no source. `docs/LICENSING.md` §5
governs the fallback that would apply if a future source could not be bundled:
it is excluded, the last valid snapshot is preserved, the `status`/`serve` path
reports it missing and directs the operator to `rust-docs-mcp sync`, and the
omission is recorded as a `first-sync-required` marker (see
`src/bundled.rs`, `FIRST_SYNC_REQUIRED` and `manifest_with_exclusions`).
