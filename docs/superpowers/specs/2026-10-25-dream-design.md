# `wiki dream` — corpus reconciliation & garbage collection (design)

Date: 2026-10-25 · Branch: skill-effectiveness · Status: approved 2026-10-25

## Intent

One command that acts as the wiki's "sleep cycle": reconcile lifecycle
(superseded/expired/duplicate/orphan data), invalidate what's outdated, and
collect junk — as a **dry-run report** by default, **applied** via `--apply`,
with a 90-day archive that makes every canonical change recoverable.

`doctor` finds problems and prints fix advice; `dream` executes the safe
subset of that advice under the transaction journal, and refuses the unsafe
subset with reasons.

## Command surface

```
wiki dream                    # dry-run plan; exit 0 always
wiki dream --apply            # archive + delete + edge cleanup, one txn
wiki dream --apply --phase junk        # single phase only
wiki dream --purge            # delete _archive entries ≥90d & unreferenced
wiki dream --purge --dry-run  # plan only
```

Output: JSON via `_emit`, same envelope style as `doctor` —
`{ok, applied, phases, counts, purge}`; a `dream_report.json` is staged into
`.state/` on every apply/purge commit.

## Phases (in order)

| phase | finds | --apply action |
|-------|-------|----------------|
| `supersede` | live claims whose `status=="superseded"` or `superseded_by` set | archive → `claims/_archive/<id>.json` |
| `expire` | non-terminal claim, `valid_to` < now | archive |
| `dedupe` | ≥2 live claims sharing (subject, predicate, canonical scope, canonical value) — doctor's `possible_duplicate` groups | keep newest `recorded_at`; archive the rest |
| `orphan` | claim with no `evidence`, status `candidate`, not open in review queue | archive (accepted-but-unevidenced stays — `unsupported_accepted` is human review) |
| `sources` | source dir with zero citing claims (`unextracted_source`/`orphan_source`) | move dir → `.state/dream-archive/sources/<sid>/` |
| `junk` | `.state/staging/*`, `.state/journal*.json`, `.state/.backup-*`, `.state/locks/*` non-held, `receipt.tmp` | hard delete (direct fs, pre-txn; never canonical) |
| `derived` | `.state/dependencies.json` edges pointing at missing artifacts or nonexistent claim/decision/page ids | drop those edges via `stage_state`; stale-list entries left (they're rebuild signals, not junk) |

## Mutation rules (agreed)

- **Canonical data is never hard-deleted at apply time.** Claims go to
  `claims/_archive/` (invisible to `list_claims`, doctor, query — they only
  list `claim_*.json` at top level). Archived JSON embeds
  `dreamed_at` + `dream_reason` (schemas allow extra keys).
- **Sources** can't be moved through the transaction (ops are file-level, not
  dir-level): moved pre-commit to `.state/dream-archive/sources/` — safe
  because they are by definition dead weight; if the txn later aborts, an
  orphan archive dir costs nothing.
- **Junk** lives under `.state/` (non-canonical): direct unlink/rmtree,
  recorded in the report's `deleted` list.
- **`commit.lock`**: only removed if `flock LOCK_EX|LOCK_NB` succeeds →
  proven unheld; a held lock aborts the whole dream (LockedError propagates).

## Purge (`--purge`, 90-day rule)

- Scans `claims/_archive/*.json` and `.state/dream-archive/sources/*`.
- Eligible: `now − dreamed_at ≥ 90 days` AND id not referenced by any live
  claim (`supersedes`, `superseded_by`, `evidence[].source_id`), decision
  (`claims[]`), review queue item (`affected[]`), or dependency edge.
- Ineligible entries listed under `purge.skipped` with the reason
  (`too_recent` or `still_referenced`).
- `--dry-run` prints the same plan without deleting.
- `now` is injectable (`dream.run(wiki, now=...)`) for deterministic tests.

## Non-goals

- No rebuild execution — dream marks/cleans, `wiki compile` rebuilds.
- No LLM calls — all checks are deterministic over files already on disk.
- `stale_decision` (decision resting on superseded claims) is reported, not
  archived — retraction is a human call.
- Page graph lints (orphan_page etc.) stay doctor+skill-side.

## Testing

`tests/test_dream.py` with the standard `fresh()` harness:

1. superseded claim → archived, gone from `list_claims`, embedded reason
2. `valid_to` past → archived; future → untouched
3. dup group → newest survives, rest archived
4. evidence-less candidate → archived; open-review-item candidate → skipped
5. source unreferenced → dir moved; cited source → untouched
6. staging/journal/lock junk → deleted; held lock aborts
7. dep edge to missing artifact → dropped
8. dry-run plans identical actions, writes nothing (revision unchanged)
9. `--purge` with `now=+91d` deletes unreferenced archive; referenced skipped
10. `--phase junk` runs only junk
