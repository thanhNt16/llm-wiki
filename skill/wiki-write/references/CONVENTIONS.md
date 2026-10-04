# CONVENTIONS.md — ownership, eligibility, citations

## Ownership regimes

- **Agent-owned via `write-page` only:** `wiki/{sources,concepts,entities,
  procedures,questions,changes}/*.md`. NEVER hand-edit these files — not to
  fix a typo, not to update frontmatter. Every change goes through
  `wiki.py write-page --file ... --artifact ... --deps 'claim_X@3,...'` so
  deps are re-registered and the stale flag clears atomically.
- **Script-owned (`write-page` rejects them):** `wiki/index.md`,
  `wiki/overview.md`, `wiki/decisions/*`, every `*/index.md`. `build-pages`
  generates them from the pages — there is no overview writer.

## The `deps` contract

- **`deps` mandatory** (question pages may be empty). Every authored page
  lists its dependencies at current versions.
- **Typed dep kinds:** `claim_X@N`, `decision_Y@N` (always with the current
  `@N` version), or a bare `wiki/<kind>/<slug>.md` artifact path — allowed
  only when that artifact exists on disk. Context-pack receipts are not
  deps.
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

`page-targets` picks the page kind via `.llm-wiki/page-kinds.json` — extend
by adding rules, no code changes:

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
- Missing/malformed file → everything routes to `concepts` (a `warnings`
  entry reports why). Invalid `kind` or matcher-less rules are skipped with
  a warning. Each target reports `matched_rule` (rule index or null).
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

## Status handling

- `accepted` — cite normally.
- `provisional` — include with a visible "(provisional)" label in the text.
- `disputed` — include only when the disagreement itself is the content
  (Tensions section), labeled "(disputed)", both sides shown.
- `rejected`, `superseded` — excluded: not in the body, not in `deps`.
