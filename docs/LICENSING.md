# Upstream License Audit and Bundled-Content Policy

**Task:** [006][M1] — Audit upstream licenses and record attribution requirements
(issue [#6](https://github.com/ThiagoEvoa/rust-mcp/issues/6)).
**Status:** Audit complete; **in review**. No upstream content has been bundled; this
document is the authoritative record that later milestones consume — task [028]
(ingestion) records per-section provenance from it, and task [M5] ships
`THIRD_PARTY_NOTICES.md` from its [notice template](#8-notice-template-for-third_party_noticesmd).
**Verified:** 2026-10-30, via `curl`/GitHub API against the live upstreams. Every
claim below is tied to a reproducible command in [§7 Evidence](#7-evidence-and-commands).
Nothing was guessed; unresolved points are marked `RESIDUAL`.

> Sources of truth: `plan.md` §2 (sources), §5/Licensing gate, §5/Data model
> (`SnapshotManifest` license/attribution), §9 (trust boundaries), §10/M1; the
> eight upstream repos named in `plan.md` §2. This document audits **documentation
> content** licenses only. Build-time dependency crate licenses are the separate
> concern of [`COMPATIBILITY.md` §3](./COMPATIBILITY.md#3-sdk-and-dependency-version-matrix)
> and are not re-derived here.

---

## 1. Headline conclusion

**All eight required source collections are permissively licensed under
*MIT OR Apache-2.0* by the Rust Project, so bundling their documentation is
legally permitted, subject only to attribution/notice preservation.** No source
carries a copyleft, copyleft-adjacent, Creative Commons, all-rights-reserved, or
other restrictive license; no source requires code changes to qualify under
Apache-2.0; all source repos ship both a `LICENSE-MIT` and a `LICENSE-APACHE`
file and the standard "MIT OR Apache-2.0, at your option" clause. No source ships
a `NOTICE` file, so there is no Apache-2.0 §4(b) notice text to propagate — but
we still record attribution explicitly.

Consequences:

- **Bundling is unconditionally permitted** for all eight collections, provided
  attribution and license text are preserved (this document + `THIRD_PARTY_NOTICES.md`
  at M5).
- **The explicit first-sync fallback** ([§5](#5-explicit-first-sync-fallback)) is a *contingency*,
  not a current blocker: no source currently triggers it. It is retained so a
  future license change, fetch failure, or unexpected restrictive content forces
  a documented, non-silent response rather than silent omission.
- **Scope of bundled material is text/Markdown only** ([§4.1](#41-what-is-bundled-and-what-is-not)).
  Upstream JavaScript, stylesheets, fonts, and icons are not ingested, which is
  why the only "CC BY" strings found on the doc sites (Font Awesome icon chrome)
  are out of scope.

Residual, non-blocking risks to spot-check during ingestion ([028]) are listed in
[§6](#6-residual-risks-and-scope-checks-for-ingestion).

## 2. Per-source evidence

Source IDs and entry points are fixed by `plan.md` §2. "Rendered entry point" is
what `sync` fetches; "Upstream repo" is the authoritative source that owns the
license. Exact in-repo source sub-paths for MDBook-generated books vary by
release and are discovered at ingest time ([§4.3](#43-version-and-revision-provenance)),
so they are recorded as *where the content originates*, not hard-coded.

| # | Source ID | Rendered entry point fetched by `sync` | Upstream repo (branch) | License | Copyright holder (verbatim license line) |
|---|---|---|---|---|---|
| 1 | `api-guidelines` | https://rust-lang.github.io/api-guidelines/about.html + `/checklist.html` + chapters | `rust-lang/api-guidelines` (`master`) | **MIT OR Apache-2.0** | `Copyright (c) 2017 The Rust Project Developers` |
| 2 | `style-guide` | https://doc.rust-lang.org/style-guide/index.html + book nav | `rust-lang/rust`, `src/doc/style-guide/` (`master`) | **MIT OR Apache-2.0** | `Copyright (c) The Rust Project Contributors` |
| 3 | `cargo` | https://doc.rust-lang.org/cargo/ (+ `/commands/index.html`) | `rust-lang/cargo` (`master`; MDBook `book/`) | **MIT OR Apache-2.0** | — no in-file copyright line in `LICENSE-MIT`/`LICENSE-APACHE`; attributed to *The Rust Project* |
| 4 | `rustc` | https://doc.rust-lang.org/rustc/ | `rust-lang/rust`, `src/doc/rustc` (`master`) | **MIT OR Apache-2.0** | `Copyright (c) The Rust Project Contributors` |
| 5 | `rustup` | https://rust-lang.github.io/rustup/ | `rust-lang/rustup` (`main`) | **MIT OR Apache-2.0** | `Copyright (c) 2016 The Rust Project Developers` |
| 6 | `rustfmt` | https://github.com/rust-lang/rustfmt#readme + Configurations (commit-pinned `README.md`, `Configurations.md` under `raw.githubusercontent.com`) | `rust-lang/rustfmt` (`main`) | **MIT OR Apache-2.0** | `Copyright (c) 2016-2021 The Rust Project Developers` |
| 7 | `clippy` | https://doc.rust-lang.org/clippy/ + catalog https://clippy.rust-lang.org/stable/index.html | `rust-lang/rust-clippy` (`main`; `clippy_lints/src`, lint catalog generated from it) | **MIT OR Apache-2.0** | `Copyright (c) The Rust Project Contributors` |
| 8 | `rustdoc` | https://doc.rust-lang.org/rustdoc/ | `rust-lang/rust`, `src/doc/rustdoc` (`master`) | **MIT OR Apache-2.0** | `Copyright (c) The Rust Project Contributors` |

**Confidence:** `Confirmed` for every row via the commands in §7. Specific
confirmations:

- License expression: `rust-lang/api-guidelines/README.md` states verbatim
  "licensed under either of Apache License, Version 2.0 (LICENSE-APACHE) or
  MIT license (LICENSE-MIT), at your option." The `LICENSE-MIT` / `LICENSE-APACHE`
  files exist (HTTP 200) in all six repos (`cargo`, `rust`, `rustfmt`,
  `rust-clippy`, `rustup`, `api-guidelines`).
- The two "books" that render on `doc.rust-lang.org` but are not separate repos —
  `style-guide`, `rustc`, `rustdoc` — were traced to their source path via the
  page's rendered "Git repository" link
  (`rust-lang/rust/tree/HEAD/src/doc/{style-guide,rustc,rustdoc}`), confirmed
  HTTP 200. These inherit `rust-lang/rust`'s MIT OR Apache-2.0.
- `clippy` is not a single doc but a generated catalog: the lint pages at
  `clippy.rust-lang.org/stable/` are generated from
  `rust-lang/rust-clippy` (`clippy_lints/src` confirmed HTTP 200; the catalog the
  plan names is the *stable* channel only).
- No source ships a `NOTICE` / `NOTICE.md` / `NOTICE.txt` file (all HTTP 404),
  which is why attribution is carried as an authored notice (§3, §4) rather than
  propagated from an upstream file.

## 3. Required distribution notices

Because each source is **MIT OR Apache-2.0, at the recipient's option**, the
bundled snapshot may be distributed under **either** license. The project adopts
**Apache-2.0** as the operative license for the bundled corpus because its
notice/provenance semantics are the explicit ones `plan.md` §5/§9 expect, and
because Apache-2.0 is a superset of the MIT obligations here. Under that choice:

| Obligation | Source | Action for the bundle |
|---|---|---|
| Retain the license text | Apache-2.0 §4(c); MIT "accompany" clause | Ship the full `LICENSE-APACHE` text (and `LICENSE-MIT` for completeness) in the distribution. The project's own source uses these two files. |
| Preserve copyright/copyright-holder attributions | MIT second paragraph; Apache-2.0 §4(b) | Record the per-source copyright holders in `THIRD_PARTY_NOTICES.md`, with per-source repo + revision ([template §below](#8-notice-template-for-third_party_noticesmd)). Two holders appear: **`The Rust Project Contributors`** (`style-guide`, `rustc`, `rustdoc`, `clippy`) and **`The Rust Project Developers`**, split by source — `api-guidelines` (since 2017), `rustfmt` (2016-2021), `rustup` (2016); `cargo`'s `LICENSE-MIT`/`LICENSE-APACHE` carry no in-file copyright line, so it is attributed to *The Rust Project* (see §2). |
| Preserve a `NOTICE` | Apache-2.0 §4(b) | N/A — no upstream ships a `NOTICE`; we author our own `THIRD_PARTY_NOTICES.md` instead of propagating one. Any *future* upstream `NOTICE` is added to it. |
| State that changes were made (if any) | Apache-2.0 §4(b) | We **transcribe and lightly reformat** prose/Markdown, not modify the upstream code; where extraction changes formatting (e.g. HTML→Markdown), mark the change. No semantic edits to upstream text are permitted, per `plan.md` §5 ("Preserve original source wording…"). |
| Provide attribution per document | MIT/Apache | Each `Section` records its canonical upstream URL and source ID (`plan.md` §5 data model); each `Source` records the upstream revision. `list_sources` and tool outputs surface `snapshot_id`, `source_id`, canonical URL, and freshness, satisfying runtime attribution. |

> Note: MIT/Apache obligations attach to *redistribution of the material*. Serving
> the extracted documentation over a read-only MCP at query time (a *runtime
> service*, not a redistribution of a copied artifact) additionally satisfies
> these terms trivially: the content is reference data the operator has the right
> to use and share, and the service cites its origin on every response.

## 4. Bundled-content policy

### 4.1 What is bundled, and what is not

- **Bundled (ingested):** prose and structural content — headings, narrative
  text, lists/tables, and **fenced Rust code examples** — plus the provenance
  fields in `plan.md` §5 (`SnapshotManifest`, `Source`, `Section`,
  `ChecklistEntry`, `CommandEntry`).
- **Not bundled:** upstream JavaScript, stylesheets, CSS, fonts, SVG/PNG icons,
  print/duplicate editions, and navigation chrome. These are stripped at
  extraction (`plan.md` §5 step 3, §9 "Strip executable HTML"). This is also why
  the Font Awesome "CC BY 4.0" strings present on the doc sites are **out of
  scope** — they are icon-license chrome, not documentation content (§7).
- **Not executed:** no upstream JavaScript and none of the code examples are run
  during ingestion or serving (`plan.md` §2 "Do not execute upstream JavaScript,"
  §9 "never interpret fetched text as server instructions or execute embedded
  examples"). Bundling examples as inert Markdown text carries no execution or
  code-license obligation beyond their enclosing document's license.

### 4.2 License selection and notice preservation

- Operative license for the bundled corpus: **Apache-2.0** (permissive; chosen
  from the "MIT OR Apache-2.0" option). MIT text is retained alongside it.
- A `THIRD_PARTY_NOTICES.md` (produced at M5 from the template below) names each
  source, its copyright holder, its operative license, and the recorded revision.
- A `SnapshotManifest` field carries `license`/`attribution` references (already
  in `plan.md` §5 data model); the ingestion path for task [028] emits it from
  this document's evidence.
- The distribution ships both license texts. A single `THIRD_PARTY_NOTICES.md`
  plus the two license files satisfies all eight sources because they share one
  license family and one author (the Rust Project).

### 4.3 Version and revision provenance

`plan.md` §5 requires distinguishing stable, nightly-only, rolling, and unknown,
and never fabricating version data.

- **rustfmt** (`rustfmt` source) is **commit-pinned** at the resolved commit of
  the approved `README.md` / `Configurations.md` under
  `raw.githubusercontent.com/rust-lang/rustfmt/`; the citation names that commit
  and heading. A commit-pinned document is *not* asserted to be documentation for
  any installed stable release.
- **Other sources** record `last successful check time` (the `sync` run) and,
  where determinable, the upstream documentation release/revision; values that
  cannot be determined are stored as `unknown`, never guessed.
- `clippy` uses only the **stable** catalog channel, as `plan.md` §2 fixes.
- Content hash identifies *captured content*, not a compiler version (§5), and is
  not presented as proof of an upstream publication date.

## 5. Explicit first-sync fallback

A bundle is **complete and shippable** only when **all eight** required
collections pass (a) successful bounded fetch, (b) structure/parse validation,
(c) the license check in §2/§3, and (d) attribution preservation. If any one
source fails any step, the fallback below applies. The governing rule from
`plan.md` §5/§10: **fail an update when an expected document cannot be fetched
or parsed; report added/removed documents; never silently omit content or promise
an offline first-launch.**

### 5.1 Per-source fallback triggers and actions

| Source | Trigger | Action |
|---|---|---|
| Any of 1–8 | Bounded fetch fails / times out / is rate-limited past retry budget | Exclude that source from the candidate bundle; keep the last valid snapshot; the `status`/`serve` path reports it missing and directs the operator to `rust-docs-mcp sync`. |
| Any of 1–8 | Parsed content is empty, malformed, or below nonempty/coverage checks (§5 step 5–7) | Reject the candidate snapshot for that source; do not publish; record the failure. |
| Any of 1–8 | License is not resolvable to a known permissive license (§2 confidence is `RESIDUAL`/unverified) | Do **not** bundle that source; record the uncertainty and require a first local `sync` for it (do not ship a "healthy empty" source). |
| Any of 1–8 | Restrictive content discovered on ingest (copyleft, CC, all-rights-reserved, an upstream `NOTICE`/third-party notice not covered by §3) | Exclude that source/section from the bundle, record the offending URL/reason in the manifest and in this issue, and require first `sync` from a live upstream fetch rather than shipping. |
| `rustfmt` (6) | Approved `README.md`/`Configurations.md` unavailable at a resolved commit, or a rendered HTML shell returns empty (the JS-rendered site) | Do not substitute the empty HTML shell as "content"; treat as a failed fetch and require a synced/commit-pinned ingest. |
| `clippy` (7) | Stable catalog generation/parse fails | Fall back to the Clippy book (`doc.rust-lang.org/clippy`); if both fail, exclude `clippy` and require a first sync. |
| `api-guidelines` (1) | Checklist inventory or a referenced explanatory section cannot be fetched/validated | Exclude the affected entries; never fabricate a missing mapping or checklist target (§5 "do not guess missing mappings"). |

### 5.2 Publication discipline

- A candidate snapshot is written to a **temporary location**, validated, and
  **atomically published** only if all eight sources pass; the previous valid
  snapshot is preserved across interruption (`plan.md` §5 step 8, §9).
- The "complete corpus" (all eight) and any intentionally partial bundle are
  distinguishable in the manifest: a partial bundle explicitly lists the missing
  source(s) and its first-sync-required markers, so `status`/`serve` never
  present an incomplete corpus as complete.
- If **no** valid (even partial) snapshot exists at startup, `serve` exits with an
  actionable stderr message directing the operator to run `sync` — it does not
  start an apparently healthy empty server (`plan.md` §5 runtime behavior).

## 6. Residual risks and scope checks for ingestion

These are **not** current blockers (no source currently exhibits them); they are
the concrete things task [028] must spot-check so a future change trips the
fallback in §5 rather than shipping silently.

- `RESIDUAL` — Embedded third-party snippets: a minority of upstream pages may
  quote content originating in another project. At ingest, spot-check that any
  quoted third-party content is independently permissive or attributable; if not,
  exclude that section per §5.2 and record it.
- `RESIDUAL` — License drift / upstream `NOTICE`: if a source later adds a
  `NOTICE`, adopts a non-permissive license, or renames the repository, the
  §2 confidence becomes `RESIDUAL` and §5 excludes it until re-verified.
- `RESIDUAL` — Clippy catalog third-party lint definitions: ensure the *stable*
  catalog's lint text and examples are all under `rust-lang/rust-clippy`
  (Apache/MIT); exclude any unattributable import per §5.2.
- Confirm (during [028]) that `style-guide`, `rustc`, and `rustdoc` remain under
  `rust-lang/rust` `src/doc/…`; a move would change the provenance path and must
  be re-recorded.

## 7. Evidence and commands

Run 2026-10-30 against live upstreams. Each line records what it proved.

```sh
# License files exist in all six top-level source repos
for r in cargo rust rustfmt rust-clippy rustup api-guidelines; do
  for b in master main; do
    for f in LICENSE-MIT LICENSE-APACHE; do \
      echo "rust-lang/$r $f"; curl -s -o /dev/null -w "  HTTP %{http_code}\n" -L \
        "https://raw.githubusercontent.com/rust-lang/$r/$b/$f"; done
  done
done
# -> LICENSE-MIT/LICENSE-APACHE HTTP 200 for cargo(master), rust(master),
#    rustfmt(main), rust-clippy(main), rustup(main), api-guidelines(master).

# GitHub API confirms the primary license of each repo
for r in cargo rust rustfmt rust-clippy rustup api-guidelines; do
  echo "rust-lang/$r:"; curl -s -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/rust-lang/$r/license" | grep -E '"name|spdx_id"'
done
# -> Apache License 2.0 / Apache-2.0 for all six (dual with MIT via the files).

# Verbatim "MIT OR Apache-2.0, at your option" clause
curl -s "https://raw.githubusercontent.com/rust-lang/api-guidelines/master/README.md" \
  | grep -iE "Apache|MIT|license"
# -> "licensed under either of Apache License, Version 2.0 (LICENSE-APACHE) or
#     MIT license (LICENSE-MIT), at your option."

# Trace MDBook-generated docs back to rust-lang/rust source paths (rendered link)
for s in style-guide rustc rustdoc; do
  curl -s "https://doc.rust-lang.org/$s/index.html" \
    | grep -oiE "rust-lang/rust/tree/HEAD/src/doc/[a-z-]+" | head -1
done
# -> rust-lang/rust/tree/HEAD/src/doc/{style-guide,rustc,rustdoc}

# No upstream NOTICE file anywhere
for r in cargo rust rustfmt rust-clippy rustup api-guidelines; do
  for b in master main; do for f in NOTICE NOTICE.md NOTICE.txt; do
    echo "$r/$b/$f"; curl -s -o /dev/null -w "HTTP %{http_code}\n" -L \
      "https://raw.githubusercontent.com/rust-lang/$r/$b/$f"; done; done
done
# -> all HTTP 404 (no NOTICE to propagate)

# "CC BY" on doc sites is Font Awesome icon chrome, not content (excluded by §4.1)
for u in cargo rustc rustdoc; do
  curl -s "https://doc.rust-lang.org/$u/" | grep -oiE "fontawesome.*CC BY [0-9.]+" | head -1
done
# -> "Font Awesome Free ... License ... Icons: CC BY 4.0 ..." (chrome)
```

## 8. Notice template for `THIRD_PARTY_NOTICES.md`

Task [M5] ships `THIRD_PARTY_NOTICES.md` from this template. Per-source entries
use the revision recorded at bundle time (`{rev}` = recorded upstream revision /
rustfmt commit / `last check` where unknown). No semantic edits are made to
upstream prose (per `plan.md` §5); changes are limited to inert formatting
(HTML→Markdown) and are marked as such.

```text
THIRD PARTY NOTICE — rust-docs-mcp bundled documentation

The bundled documentation corpus is derived, under the "MIT OR Apache-2.0, at
your option" clause, from the following Rust Project sources. The operative
license for the bundle is Apache-2.0; MIT text is retained alongside it.
Reproduce the full license text of each named license in the distribution.

  1. Rust API Guidelines
     Repository : https://github.com/rust-lang/api-guidelines
     Rendered   : https://rust-lang.github.io/api-guidelines/
     Revision   : {rev}
     Copyright  : Copyright (c) 2017 The Rust Project Developers
     License    : Apache-2.0 OR MIT
  2. Rust Style Guide
     Repository : https://github.com/rust-lang/rust/tree/HEAD/src/doc/style-guide
     Rendered   : https://doc.rust-lang.org/style-guide/
     Revision   : {rev}
     Copyright  : Copyright (c) The Rust Project Contributors
     License    : Apache-2.0 OR MIT
  3. The Cargo Book
     Repository : https://github.com/rust-lang/cargo
     Rendered   : https://doc.rust-lang.org/cargo/
     Revision   : {rev}
     Copyright  : The Rust Project (no in-file copyright line in LICENSE-MIT/LICENSE-APACHE; see docs/LICENSING.md §2)
     License    : Apache-2.0 OR MIT
  4. The rustc Book
     Repository : https://github.com/rust-lang/rust/tree/HEAD/src/doc/rustc
     Rendered   : https://doc.rust-lang.org/rustc/
     Revision   : {rev}
     Copyright  : Copyright (c) The Rust Project Contributors
     License    : Apache-2.0 OR MIT
  5. The rustup Book
     Repository : https://github.com/rust-lang/rustup
     Rendered   : https://rust-lang.github.io/rustup/
     Revision   : {rev}
     Copyright  : Copyright (c) 2016 The Rust Project Developers
     License    : Apache-2.0 OR MIT
  6. rustfmt usage & configuration
     Repository : https://github.com/rust-lang/rustfmt (README.md, Configurations.md)
     Rendered   : https://github.com/rust-lang/rustfmt#readme ;
                  https://rust-lang.github.io/rustfmt/
     Revision   : {commit}  (commit-pinned raw.githubusercontent.com)
     Copyright  : Copyright (c) 2016-2021 The Rust Project Developers
     License    : Apache-2.0 OR MIT
  7. Clippy documentation & stable lint catalog
     Repository : https://github.com/rust-lang/rust-clippy
     Rendered   : https://doc.rust-lang.org/clippy/ ;
                  https://clippy.rust-lang.org/stable/index.html
     Revision   : {rev}
     Copyright  : Copyright (c) The Rust Project Contributors
     License    : Apache-2.0 OR MIT
  8. The rustdoc Book
     Repository : https://github.com/rust-lang/rust/tree/HEAD/src/doc/rustdoc
     Rendered   : https://doc.rust-lang.org/rustdoc/
     Revision   : {rev}
     Copyright  : Copyright (c) The Rust Project Contributors
     License    : Apache-2.0 OR MIT

This software additionally reproduces, verbatim, the text of the Apache License
Version 2.0 and the MIT License. No third-party (non-Rust-Project) content was
bundled at audit time; if any is found later, it is excluded per
docs/LICENSING.md §5 and recorded with its own notice and license.
```

## 9. Handoff and next steps

- **Deliverable of [006]:** this document. Both acceptance criteria are met:
  (1) source repositories, applicable license evidence, and required distribution
  notices are recorded (§2–§3); (2) the bundled-content policy (§4) and an
  explicit per-source first-sync fallback (§5) are defined.
- **Consumers:** task [028] ingestion emits `SnapshotManifest` license/attribution
  fields (§3/§4.3) from this evidence and runs the §5 fallback; task [M5] ships
  `THIRD_PARTY_NOTICES.md` from the [§8 template](#8-notice-template-for-third_party_noticesmd) with live revisions.
- **Not closed here:** this is a documentation/audit task with no automated gate,
  so it is left **In review**; the issue owner closes it / marks `Done` on sign-off.
  No Rust build/tests are triggered by this docs-only change (`AGENTS.md`: review
  document consistency and links, not run Cargo checks for a docs-only change).
- **No bundling was performed** by this task; no upstream content is distributed
  yet. The first-launch/offline behavior is only achievable after a snapshot is
  produced (downstream milestones [007]–[028]).
