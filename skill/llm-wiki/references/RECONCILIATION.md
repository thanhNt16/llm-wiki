# RECONCILIATION.md — classifying new evidence against existing knowledge

Decide exactly one relationship per candidate-vs-existing match. Work from the
`reconcile-prepare` comparison output (subject+predicate key, `same_scope`,
`same_value`, `temporal`, current `status` and `value`).

## Decision procedure

1. **No matches** (or nothing sharing the subject/predicate key) → `UNRELATED`.
2. Matches exist. Compare **scope first**:
   - Same key, different scope (production vs sandbox, different services),
     and neither subsumes the other → `SCOPE_DIFFERENCE` (both stay current).
   - Same scope, continue.
3. Same scope. Compare **values and validity**:
   - Same value → `DUPLICATE` (if evidence origins differ, merge and let the
     script add a duplicate_identity review item) or `CORROBORATION` (value
     restated by an independent origin — prefer this when the new source is
     not derived from the old one).
   - Different value:
     - The new source **explicitly says the value changed** (memo, new ADR,
       changelog, "as of <date>") →
       - `POLICY_CHANGE` when the old claim has `valid_from` and the change is
         effective-dated: pass `valid_from` of the new claim; the script sets
         the old claim's `valid_to` to the day before.
       - `CORRECTION` when the old claim was simply **wrong** (misreading,
         typo, wrong number): the old claim becomes superseded immediately.
     - Both sources still claim to describe current reality and only one can
       be right → `CONTRADICTION` (candidate stored as disputed; review item
       created — a human decides).
     - Uncertain which applies → `CONTRADICTION` (safe default; never guess).

## Authority rules (PRD §31-32)

Set `authority.type` on the classification from the SOURCE OF THE EVIDENCE:

- `explicit_project_decision` — ADR, decision record, explicit user statement.
- `authoritative_source` — current config file, official spec, signed doc.
- `implementation_verification` — you read the actual code/config and verified.
- `agent_inference` — you inferred it. This never auto-accepts.

Auto-accept policy (configurable in `wiki.json`): the first three accept
immediately; `independent_corroboration` accepts at 2 distinct `root_origin`s;
`agent_inference` stays `candidate`.

## Worked examples

| Existing | New evidence | Relationship | Why |
|---|---|---|---|
| prod window 30d (ADR-019) | memo: "prod now 7d from Sep 1" | POLICY_CHANGE | effective-dated change |
| prod window 7d | config prod.json says 7d | CORROBORATION | independent origin, same value |
| retries 3 (ADR) | config says 5, no memo | CONTRADICTION | silent disagreement → human |
| prod window 7d | sandbox config says 30d | SCOPE_DIFFERENCE | environments differ |
| "we use outbox" claim | meeting notes quoting the wiki | DUPLICATE | derived, not independent |
| window 30d (ADR) | errata: ADR misread, always 7d | CORRECTION | old claim was wrong |

Red flag: classifying as CORRECTION/POLICY_CHANGE without evidence that the
old value was wrong or superseded. If unsure, CONTRADICTION is the honest call.
