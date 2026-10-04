# PAGES.md — per-type page contracts

Frontmatter (yamlite) keys for every page: `type` (`concept | entity |
procedure | question | source | change` — must match the artifact directory),
`subject`, `title`, `created`, `updated`, `deps`, `stale` (bool). Slug:
subject lowercased, non-alphanumeric runs → `-`, trimmed, ≤60 chars cut at a
`-` boundary.

## Worked example — a first page

Copy this, swap the values. `created` is ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SSZ`)
and is REQUIRED on new pages. Do NOT hand-compute `updated` or `stale` —
`write-page` sets both (`updated` to now, `stale: false`), and on a refresh it
backfills `created` from the existing page. `deps` lists every `[[claim_id]]`
you cite, at the claim's current version:

```markdown
---
type: concept
subject: order.attribution
title: Order attribution
created: 2026-10-02T10:00:00Z
updated: 2026-10-02T10:00:00Z
stale: false
deps: ["claim_01J9XQ5G7M4T2WY8Z3R6CVE0KH@3", "claim_01J9XR2N8P6Q4B1D9F5H7J2S4M@1"]
---

## Synthesis

Orders attribute to the last-touch campaign within a 7-day window
[[claim_01J9XQ5G7M4T2WY8Z3R6CVE0KH]]. The window was shortened from 30 days
in September [[claim_01J9XR2N8P6Q4B1D9F5H7J2S4M]].

## Tensions

- Staging still runs the 30-day window [[claim_01J9XR2N8P6Q4B1D9F5H7J2S4M]].
```

Commit it with:

```bash
python3 "$WIKI" write-page --file page.md --artifact wiki/concepts/order-attribution.md \
  --deps 'claim_01J9XQ5G7M4T2WY8Z3R6CVE0KH@3,claim_01J9XR2N8P6Q4B1D9F5H7J2S4M@1'
```

## What `write-page` actually enforces

Every item below is a hard rejection; anything not listed is your judgment,
not a gate:

- `type` present and equal to the singular of the artifact directory
  (`wiki/concepts/…` → `concept`).
- `subject` non-empty, and `slugify(subject)` equals the artifact slug —
  collision-suffixed slugs (`<slug>-<hash6>`) match when they start with the
  base slug.
- `title` non-empty.
- `created` present and ISO-8601 (`YYYY-MM-DDT…`) on NEW pages; backfilled
  from the old page on refresh.
- Every `[[claim_id]]` in the body resolves to an existing claim, and the
  linked set is a subset of `deps` (cite nothing you didn't declare).
- Every dep normalizes to the claim's/decision's CURRENT version
  (`claim_X@N`, `decision_Y@N`); unknown ids, stale versions, and
  `rejected`/`superseded` claims are rejected. A bare `wiki/<kind>/<slug>.md`
  artifact path is also a valid dep when that file exists on disk (context
  receipts are not deps).
- If any dep claim is `disputed`, frontmatter must carry `disputed: true`.
- Question pages may have empty `deps`; every other kind needs at least one.
- The artifact is `wiki/<kind>/<slug>.md`, not a reserved path
  (`wiki/index.md`, `wiki/overview.md`, `wiki/decisions/*`, `*/index.md`).
- On success it overwrites `updated`, `stale: false`, and `deps` in the
  frontmatter (your hand-written `deps` line is replaced by the normalized
  set), registers the deps (replacing any previous set — see CONVENTIONS.md),
  and clears the stale flag in one transaction.

## source page — `wiki/sources/<slug>.md` (300–600 words)

- `## Summary` — what the source is and what it establishes, in prose.
- `## Key claims` — one bullet per claim; every bullet ends with its
  `[[claim_id]]` citation.
- No synthesis across sources. If two claims from this source tension, state
  both.

## concept / entity page — `wiki/concepts|entities/<slug>.md` (200–500 words)

- Facts come only from cited claims. Merge duplicates: one fact, attributed
  to every source that states it.
- `## Synthesis` — the only section where connective judgment is allowed
  ("therefore", "in practice", "as a result"). Every sentence still cites the
  claims it rests on.
- `## Tensions` — disagreements preserved, both sides labeled with their
  `[[claim_id]]`s; never averaged into a quiet compromise.
- On refresh: preserve existing `created` and any human-curated aliases;
  update `updated` and `deps` to current versions.

## procedure page — `wiki/procedures/<slug>.md`

- Ordered steps; every step grounded in ≥1 cited claim. No invented steps, no
  steps from general knowledge. If claims disagree about a step, Tensions
  section as above.

## question page — `wiki/questions/<slug>.md` (≤25 lines)

- Frontmatter adds `asked: YYYY-MM-DD` (the date the question was asked).
  `deps` may be empty — questions record intent, not evidence.
- Body: the verbatim question, the answer pointer (which page(s) answer it),
  and the cited pages. Nothing else.
- Reference pages as backticked `wiki/<kind>/<slug>.md` paths (e.g.
  `` `wiki/concepts/order-attribution.md` ``). These are plain text, NOT
  `[[...]]` links — `write-page` does not validate them, so point only at
  pages that exist.

## changes page — `wiki/changes/<slug>.md`

- Chronological record of what changed and when: one entry per correction or
  policy change, each dated and cited to the claim(s) that carry it
  (`[[claim_id]]` at current versions in `deps`, as always).
- Keep entries additive — superseded values stay listed with their dates and
  the replacing value; do not rewrite history in place. `## Tensions`
  applies when two sources still disagree.
- Typical use: the `.changelog` kind route from `page-kinds.json`.

## Reserved artifacts

`wiki/index.md`, `wiki/overview.md`, `wiki/decisions/*`, `*/index.md` —
script-generated by `build-pages`; `write-page` rejects them.
