# ARCHITECTURE.md — information model

## Four layers (PRD §3)

```
EVIDENCE   what a source verbatim says      raw/, sources/          immutable
   ↓ supports
CLAIM      structured interpretation       claims/claim_X.json     status lifecycle
   ↓ accepted as
DECISION   what the project chose          decisions/decision_X.json
   ↓ rendered into
DERIVED    wiki/ context/ views/           rebuildable, disposable
```

Deleting everything under `wiki/ context/ views/` must leave semantic state
recoverable by recompiling (exact prose may differ; state must not).

## Claim anatomy

`subject / predicate / value [+ unit]` with:

- **scope** — environment/service/model qualifiers. `production → 7 days` and
  `sandbox → 30 days` are NOT a contradiction (PRD §6).
- **valid time vs recorded time** — `valid_from/valid_to` describe the world;
  `recorded_at` describes when the wiki learned it (PRD §5).
- **status** — `candidate provisional accepted disputed superseded rejected`.
- **authority** — named class, never a numeric confidence (PRD §4):

```
explicit_project_decision > authoritative_project_artifact >
verified_implementation_state > corroborated evidence >
single-source evidence > agent inference
```

- **evidence** — each citation carries `source_id@version` plus a precise
  locator (heading, line range, spreadsheet range, repo path…, PRD §12).
- **provenance ancestry** — `root_origin` records the independent origin.
  ADR → wiki summary → meeting notes quoting it → session quoting notes is
  4 references but 1 independent origin (PRD §7).

## Correction beats accumulation (PRD §8.4, §16)

New evidence meeting old knowledge classifies into exactly one relationship
(see RECONCILIATION.md). CORRECTION supersedes immediately; POLICY_CHANGE
closes the old validity interval the day before the new one starts. Every
change invalidates dependents transitively (`.state/dependencies.json`);
stale artifacts must be rebuilt before `verify` passes.

## Deterministic vs semantic boundary (PRD §66)

Scripts own acquire/preserve/normalize, validation, commits, invalidation,
selection under budget. The agent owns interpretation: extraction,
relationship classification, prose. Never move work across this line —
don't hand-edit what a script should compute; don't script semantic judgment.

## Storage rule

Canonical = JSON under `sources/ claims/ decisions/ notes/` plus `.state/`.
Derived = Markdown + yamlite frontmatter under `wiki/ context/ views/`.
Every derived artifact's frontmatter lists `deps: ["claim_X@N", …]` —
`wiki.py verify` enforces this.
