# CONVENTIONS.md — ownership, eligibility, citations

## Ownership regimes

- **Agent-owned via `write-page` only:** `wiki/{sources,concepts,entities,
  procedures,questions,changes}/*.md`. NEVER hand-edit these files — not to
  fix a typo, not to update frontmatter. Every change goes through
  `wiki.py write-page --file ... --artifact ... --deps 'claim_X@3,...'` so
  deps are re-registered and the stale flag clears atomically.
- **Script-owned (`write-page` rejects them):** `wiki/index.md`,
  `wiki/overview.md`, `wiki/review.md`, `wiki/log.md`, `wiki/decisions/*`,
  every `*/index.md`. `build-pages` generates them from the pages — there
  is no overview writer.

## The `deps` contract

- **`deps` mandatory** (question pages may be empty). Every authored page
  lists its dependencies at current versions.
- **Typed dep kinds:** `claim_X@N`, `decision_Y@N` (always with the current
  `@N` version), or a bare `wiki/<kind>/<slug>.md` artifact path — allowed
  only when that artifact exists on disk. Context-pack receipts are not
  deps.
- **Page links self-declare.** Any markdown link `[x](../entities/y.md)` /
  `[x](./y.md)` / `[x](wiki/concepts/x.md)` in the body is validated against disk and
  appended to `deps` automatically; a bundle-internal link that
  doesn't resolve is a hard rejection. Claim `[[...]]` links still must be
  listed manually. Rewriting a page with a changed body stale-flags every
  page that links to it (transitively) — identical-body rewrites don't.
- **`linked ⊆ deps` enforced.** Every `[[claim_id]]` in the body must appear
  in `deps`; citing an undeclared claim is a hard rejection.
- **Replacement, not merge.** Each `write-page` sets the page's dep set to
  exactly what you passed this time. Re-authoring with a shorter list
  shrinks the set; dropped deps are unregistered.
- **Disputed gate.** If any dep claim is `disputed`, the page frontmatter
  must carry `disputed: true` — write-page rejects the write otherwise.
  (Body-level rule below still applies: disputed claims appear only as
  labeled Tensions.)

## Slug collisions

Two subjects slugify identically → the second artifact gets a
`<slug>-<hash6>` suffix (assigned by target selection; use `target.artifact`
verbatim). `write-page` still checks `slugify(subject)` — a suffixed artifact
matches when it starts with the base slug. On refresh the subject is
immutable: a page whose artifact is `foo-a1b2c3` must keep a subject that
slugifies to `foo`.

## Eligibility (what `page-targets` emits)

A subject qualifies for a page when either:

- it has ≥2 claims with eligible status (`accepted` or `provisional`) AND
  those claims span ≥2 distinct `root_origin`s (independence is decided by
  the script — four documents quoting one ADR are one origin), or
- it has ≥1 eligible claim whose `authority.type` is
  `explicit_project_decision`.

Anything else stays claim-only. Do not author pages for unqualified subjects
and do not merge subjects to manufacture eligibility.

## Kind routing (which dir a subject lands in)

`page-targets` resolves each subject's kind in this order: `--kind` on the
CLI wins outright; then ordered file rules in `.llm-wiki/page-kinds.json`;
then built-in heuristics; then `default`. Extend file rules by adding
entries — no code changes:

```json
{
  "default": "concepts",
  "rules": [
    {"kind": "entities",   "prefix": "svc."},
    {"kind": "entities",   "regex": "\\.(api|service)$"},
    {"kind": "procedures", "predicate": "^step|runbook$"},
    {"kind": "questions",  "subject": "open.billing-migration"},
    {"kind": "changes",    "suffix": ".changelog", "when": {"min_claims": 2}}
  ]
}
```

- First matching rule wins; all matcher keys in one rule are AND-ed.
- Matchers: `subject` (exact), `prefix`, `suffix`, `regex`, `predicate`
  (regex against the subject's claim predicates). `when.min_claims` gates on
  eligible claim count.
- `--kind <dir>` on the command line overrides rules for every target.
- Built-in heuristics run per subject *after* file rules — a matching
  file rule wins, a non-matching one falls through to the heuristics.
  `init` writes a starter `{"rules": [], "default": "concepts"}` —
  heuristics handle everything until rules are added. In order: (1) a
  predicate matching `^(step|procedure|runbook|how[ _-]?to)` → `procedures`;
  (2) a subject matching `^[a-z]+-\d+$` (ticket ids) or
  `\.(api|service|ui|db|cli)$` → `entities`; (3) a single-token
  code-identifier subject (snake/kebab/camel, no spaces) with ≥2 claims →
  `entities`; (4) subject or predicate matching
  `open[ _-]?question|unresolved|tbd` → `questions`; (5) else `default`.
  To pin everything to concepts, add a catch-all
  `{"kind": "concepts", "regex": ".*"}`.
- Each target reports `matched_rule`: an integer index into `rules[]`
  when a file rule matched, `"heuristic:N"` for a heuristic hit, `null`
  when it fell to `default`. A wrong-looking `matched_rule` is overridden
  by adding a file rule and re-running `page-targets`.
- Malformed files and skipped rules (invalid `kind`, no matcher key)
  produce `warnings` entries; routing continues on whatever remains.
- Kinds: `concepts`, `entities`, `procedures`, `questions`, `sources`,
  `changes` — the artifact is `wiki/<kind>/<slug>.md` and frontmatter `type`
  must equal the singular (`entity`, `procedure`, …).

## Citations

- Cite a claim in prose as `[[claim_id]]`. The script resolves every
  `[[...]]` link in the body and enforces `linked ⊆ deps` (see above); an
  undeclared or unresolved link is a hard rejection.
- `deps` entries use current versions (`claim_X@N`, N = the claim's current
  version). Any other N is a hard rejection. Unknown claim ids likewise.
- `deps` citing `rejected` or `superseded` claims is a hard rejection; cite
  the successor claim instead.

## Page links (`[[wiki:…]]`)

- Body links to other pages use `[[wiki:<kind>/<slug>]]` (kinds:
  `concepts|entities|procedures|questions|sources|changes`). Each maps to
  dep path `wiki/<kind>/<slug>.md`: it MUST appear in `deps` AND the target
  file must exist at write time — an unresolved or undeclared page link is
  a hard rejection, same as claim links.
- Concept and entity pages SHOULD end with a `## See also` section — see
  the dedicated section below; it is the primary defense against
  `orphan_page`/`dead_end_page` doctor findings.
- Circular first-creation needs two passes: write page B without the link,
  write page A linking B, then re-write B linking A.
- Run `wiki.py subjects` before authoring — if a near-duplicate subject
  already has a page, refresh it instead of minting a twin (doctor flags
  unlinked near-dupes as `near_duplicate_subject`).
- Optional frontmatter `stale_after: YYYY-MM-DD` gives a page a TTL; doctor
  reports `stale_page_ttl` once the date passes.

## See also (cross-linking pages)

Authored pages SHOULD cross-link related pages in the body — it keeps the
page graph traversable and, over time, drains two whole classes of doctor
findings: `orphan_page` (nothing links in) and `dead_end_page` (the page
links nowhere).

- End every concept/entity page with a `## See also` section carrying 1–3
  `[[wiki:<kind>/<slug>]]` links to genuinely related pages — not filler.
- Every link still needs its dep (`wiki/<kind>/<slug>.md` in `deps`; the
  auto-derive adds them for you) and the target file must exist at write
  time.
- Pick links that make the graph bidirectional: link the sibling pages the
  reader would want next, and when you notice a related page pointing
  nowhere, refresh it to point back.

Example tail of `wiki/concepts/attribution-window.md`:

```markdown
## See also

- [[wiki:concepts/click-lookback]]
- [[wiki:procedures/monthly-attribution-audit]]
```

## Status handling

- `accepted` — cite normally.
- `provisional` — include with a visible "(provisional)" label in the text.
- `disputed` — include only when the disagreement itself is the content
  (Tensions section), labeled "(disputed)", both sides shown.
- `rejected`, `superseded` — excluded: not in the body, not in `deps`.
