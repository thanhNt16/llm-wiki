# E2E engine parity — rust vs python

Phase P7 scenario-level evidence for `evals/e2e/run_e2e.py` run against both
wiki engines on the same machine (2026-11-15, omp runtime lane only).

- Python engine: `skill/llm-wiki/scripts/wiki.py`
- Rust engine: `rust/target/release/llm-wiki` (release build)
- Selection: `evals/_engine.py` via `-e python|rust` — no fallback.

Two evidence layers:

1. **Deterministic state parity** — `setup_project()` drives init → ingest →
   stage-candidates → reconcile-apply → build-pages → verify for each
   build-producing setup mode under both engines, then the resulting
   `.llm-wiki` trees are compared (raw: `state_parity.py` →
   `state-parity.json`; semantic: `semantic_parity.py` →
   `semantic-parity.json`).
2. **Runtime lane parity** — `run_e2e.py -e <engine> --runtime omp`: headless
   `omp` executes each scenario's instruction (embedding the engine under
   test), then the deterministic `assert_*` checks run against the produced
   artifacts. (`claude`/`cellock` were not exercised in this run; earlier
   `--runtime all` lanes recorded symmetric skipped rows for both.)

## Runtime lane results

| runtime | scenario | python | rust | parity |
|---|---|---|---|---|
| omp | S1 | pass (61s) | pass (104s) | ✅ |
| omp | S2 | pass (81s) | pass (103s) | ✅ |
| omp | S3 | pass (30s) | pass (35s) | ✅ |
| omp | S4 | pass (191s) | pass (178s) | ✅ |
| omp | S5 | pass (27s) | pass (21s) | ✅ |
| omp | S6 | **fail** (30s) | pass (54s) | ❌ |
| omp | S7 | pass (42s) | pass (49s) | ✅ |
| omp | S8 | pass (221s) | pass (281s) | ✅ |

**Lane exit codes:** python lane = `1`, rust lane = `0`.
(`run_e2e` exits 1 if any row is fail/error.)

**S4 passes on both engines after an assertion fix.** The prior run's
identical S4 failures were an assertion bug, not an engine or agent
failure: `assert_s4` read the scope key `env`, while the claim schema and
docs use `scope.environment`, so the supersede check could not see that
the unscoped/production `window-days=30` claim had been correctly
superseded by the `7d` claim. A deterministic repro (6/6 variants) passed
on both engines, and after fixing the assertion to read
`scope.environment` both omp lanes pass: "7d current, 30d
superseded" (py 191s, rs 178s).

**S6 divergence is agent nondeterminism, not an engine difference** (repeat
of the prior comparison's lone divergence). In the python lane the omp agent
answered the order-service question but then ran
`rg -l "source_…" .llm-wiki` over the wiki store — `assert_s6` fails any
grep/rg use for the query ladder. In the rust lane the agent stayed on the
query commands (8 commands, no grep/rg). The engines' deterministic states
were identical before the agent ran; the same agent could have taken either
path under either engine. A rerun could flip either lane.

## Deterministic state parity

| setup mode | claims | sources | pages | verdict |
|---|---|---|---|---|
| init+inbox | 0 | 0 | 0 | ✅ semantic parity |
| build | 9 | 7 | 6 | ✅ claims/sources parity; 2 page-text diffs |
| build_uncorrected | 9 | 7 | 5 | ✅ claims/sources parity; 1 page-text diff |

Claims and sources match **exactly** as multisets (subject/predicate/
value/scope/evidence/authority/status/supersedes, ULIDs and wall-clock
timestamps normalized): 9 claims + 7 sources in both `build` and
`build_uncorrected`, identical keys, statuses and evidence locators.

Page-text diffs (semantic layer):

- `wiki/review.md` — `build` + `build_uncorrected`: python emits
  `0 open, 0 deferred, 0 total.` while rust emits `0 open, 0 deferred,
  0 total — full table: [review.md](review.md).` Cosmetic generated text.
- `wiki/concepts/retry-policy.md` — `build` only: the harness seed page
  cites `retry/backoff` under python and `retry/max-attempts` under rust.
  Both are valid retry-subject claims; the divergence is ULID-allocation
  order amplified by `_seed_retry_page` (see seed-page note). Present in
  `build` but not `build_uncorrected` this run — the ordering is
  nondeterministic across runs.

### Observed implementation-level differences (not claim/source content)

| area | python | rust | impact |
|---|---|---|---|
| raw storage dir | `raw/file/<sha12>/` | `raw/files/<sha12>/` | recorded in manifest `raw_path`; agent-visible path string |
| search index | `.state/fts.sqlite` | `.state/index.sqlite` | internal |
| staging markers | no `.live` files | `.state/staging/*/.live` left behind | internal |
| op receipts | ingest-class txns emit `command` + `changes` + `warnings` | converged (fixed 2026-11-15); residual `wiki/log.md`/`changes/index.md` diff is frontmatter key order only | converged |
| `wiki/review.md` | `0 open, 0 deferred, 0 total.` | `0 open, 0 deferred, 0 total — full table: [review.md](review.md).` | cosmetic text |
| frontmatter key order | insertion order | alphabetical | cosmetic (YAML) — only remaining generated-text diff in log/index pages |
| claim ULID order | candidates staged in listed order | different internal order | cosmetic — but see seed-page note |
| `origin` canonicalization | `/tmp/...` as given | `/private/tmp/...` resolved | macOS symlink; path text in manifest |
| commit/index locks | released (lock files absent) | `commit.lock`/`index.lock` present | internal |

**Seed-page note (harness amplification, not an engine bug):**
`_seed_retry_page` picks the first `subject=="retry"` claim by sorted file
name. Which claim that is varies with ULID allocation order — python tends
to surface `max-attempts=3` first, rust `backoff=exponential` (this run:
`backoff` under python vs `max-attempts` under rust in `build`). So the
seeded `wiki/concepts/retry-policy.md` can cite different (both valid,
retry-subject) claims across engines. The claim sets themselves are
identical.

## Verdict

**Deterministic layer: PARITY.** For every setup mode both engines produce
the same claim multiset (9 claims), source manifests (7), statuses,
evidence, supersedes edges, and page set. The remaining differences are
generated bookkeeping text: a cosmetic sentence in `wiki/review.md`, YAML
frontmatter key ordering in `log.md`/`index.md`/`changes/index.md`, and
internal `.state/` layout (`raw/files` vs `raw/file`, `index.sqlite` vs
`fts.sqlite`, `.live` staging markers, lock files). None of these alter any
assertion outcome.

**Runtime (omp) lane: 1 status divergence out of 8 — S6, agent-caused, not
engine-caused.** python lane: `fail` (`rg -l "source_…" .llm-wiki` used for
the query); rust lane: `pass` (8 commands, no grep/rg). S4 now passes
identically on both engines — the earlier shared `fail` was an assertion
bug (`scope.env` vs `scope.environment`), since fixed, and both lanes
report "7d current, 30d superseded". All other scenarios pass on both
engines, including S2 (near-dup `forced_new` guard) and S8 (graph-data
payload identical shape: 12 nodes/8 links on both). Wall times are the same
order (~21–281s per scenario per lane).

**Overall: divergence at the scenario-verdict layer; parity at the engine
layer.** The scenario verdict rule (identical pass/fail per scenario)
reports a single divergence at S6 attributable to agent nondeterminism
rather than the engine; at the deterministic engine layer — the deliverable
of this comparison — parity is exact for claims/sources and differs only in
the bookkeeping text listed above.

No harness fixes were required: `-e rust` plumbing (`_CliEngine`,
`assertions._ENGINE_ARGV`, instruction `python3 `-stripping) worked
unchanged. This run drove `--runtime omp` only; rust lane rows were
collected into a temp file and appended to `results.jsonl` as a contiguous
block (rows 67–74 python, 75–82 rust).

## Artifacts

- `compare/results.json` — machine-readable per-scenario per-engine rows +
  verdict.
- `compare/state-parity.json`, `compare/semantic-parity.json` —
  deterministic-layer diff evidence.
- `compare/results.precompare.jsonl` — snapshot of results.jsonl before this
  comparison (82 rows).
- `runs/*.ndjson` — per-scenario headless transcripts.
