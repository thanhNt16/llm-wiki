# Skill-Unified Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merge `skill-v2` into `skill-effectiveness`, reconcile the 7 overlapping files per the approved spec, fill all four holes (lint→repair, KI-1, O(N²), ingest triage), and build the five-layer testing strategy plus the multi-version evaluation matrix.

**Architecture:** True git merge with additive conflict policy and `TODO-UNIFY` markers, then TDD reconciliation tasks that drive the suite green, then holes (engine capabilities), then the verification stack (smoke, perf baselines, e2e runtimes, matrix) — each task independently testable and committed.

**Tech Stack:** Python ≥3.9 stdlib only (unittest, subprocess); git; agent CLIs (`omp`, `claude`) for e2e.

**Spec:** `docs/superpowers/specs/2026-10-25-skill-unified-design.md` (commit 14c2d47)

## Global Constraints

- Work ONLY in `/Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness`, branch `skill-effectiveness`.
- Python ≥3.9 stdlib only — no third-party imports anywhere.
- Thresholds verbatim: `NEAR_DUP_WRITE_THRESHOLD = 0.60`, `NEAR_DUP_LINT_THRESHOLD = 0.70`.
- Lint severities: topology/TTL/near-dup findings are `warn` (never flip doctor `ok` or verify); malformed frontmatter is `error`.
- Exact code names: `unlisted_page`, `section_index_unlinked`, `orphan_page`, `dead_end_page`, `walled_garden`, `near_duplicate_subject`, `stale_page_ttl`.
- Exit codes: TxnError → 3 (existing contract).
- SKILL.md body < 600 words (existing `test_skill_body_is_thin`).
- Ingest `--kind` vocabulary post-merge is v2's actual: `choices=["session"]` (wiki.py:397 on v2) — do NOT invent article/paper/repo/notes.
- After every task from Task 4 onward: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests` green and `python3 evals/deterministic/run_gates.py` all PASS. Tasks 2–3 land into a red merge state; their job is to make their own area green.
- Commit style: `feat:`/`fix:`/`test:`/`bench:`/`docs:`/`chore:` prefixes, one logical change per commit.

---

### Task 1: Merge skill-v2 (mechanics only)

**Files:**
- Modify: the 7 conflict files (`wikicore/synthesize.py`, `wikicore/doctor.py`, `scripts/wiki.py`, `SKILL.md`, `references/COMMANDS.md`, `scripts/tests/test_doctor.py`, `evals/semantic/results.jsonl`) — plus whatever git reports.
- Create: nothing.

**Interfaces:**
- Consumes: branches `skill-effectiveness` (HEAD) and `skill-v2` (f42e7bc).
- Produces: merge commit on `skill-effectiveness`; `TODO-UNIFY` markers at each semantic collision; a green-from-git perspective (no unresolved conflicts). Suites MAY be red after this task — that is expected and recorded, not fixed here.

- [ ] **Step 1: Run the merge**

```bash
cd /Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness
git merge --no-ff skill-v2 -m "merge: skill-v2 into skill-effectiveness (additive; semantic collisions marked TODO-UNIFY)"
```

- [ ] **Step 2: Resolve conflicts per policy**

For each conflicted file: keep BOTH sides where additive (both new flags on a parser, both new checks in doctor, both new test classes). Where the same name means different things (`orphan_page`, `dead_end_page` in doctor.py and test_doctor.py), keep EITHER side's hunk and immediately above it add the line `# TODO-UNIFY: reconcile per spec §2 (rename to unlisted_page / v2 semantics)`. For `results.jsonl`, produce the union of both sides' rows (both are line-oriented; concatenate both versions, deduplicate identical lines with `sort -u` semantics but preserve first-seen order).

- [ ] **Step 3: Verify no conflicts remain and commit**

```bash
git status --short | grep -c "^UU"   # expect 0
git add -A && git commit --no-edit
git log --oneline -1
```

- [ ] **Step 4: Record the red/green state (no fixing)**

```bash
python3 -m unittest discover -s skill/llm-wiki/scripts/tests 2>&1 | tail -1 | tee /tmp/merge-state.txt
grep -rn "TODO-UNIFY" --include="*.py" | wc -l | tee -a /tmp/merge-state.txt
```

Expected: possibly FAILED — that number plus the marker count is this task's report data.

- [ ] **Step 5: Commit** (already committed in Step 3 — if Step 2 needed multiple commits, ensure the final state is the merge commit or direct follow-ups prefixed `merge:`).

---

### Task 2: Reconcile `synthesize.py` — guard + page links unified

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/synthesize.py`
- Test: `skill/llm-wiki/scripts/tests/test_synthesize.py` (union both sides' classes; fix any TODO-UNIFY)

**Interfaces:**
- Consumes: existing `similar_pages(wiki, kind, subject, title, exclude_slug=None) -> list` and `write_page(wiki, artifact, markdown_text, dep_list, base_revision, allow_new=False)` (skill-effectiveness side); v2's `PAGE_LINK_RE`/self-link/deps validation (v2 side).
- Produces: module constants `NEAR_DUP_WRITE_THRESHOLD = 0.60`, `NEAR_DUP_LINT_THRESHOLD = 0.70`; `write_page` enforcing BOTH invariants with one unified refusal message; `extract_page_links(body) -> list[str]` (dedup, deterministic order) used by doctor in Task 3.

- [ ] **Step 1: Write the failing tests** (append; keep both sides' existing classes)

```python
class UnifiedWritePageTest(unittest.TestCase):
    def setUp(self):
        self.wiki = fresh()
        for i in (1, 2):
            add_accepted_claim(self.wiki, "attribution window", "length", "%dd" % (7 if i == 1 else 30))

    def _md(self, subject, title, links=()):
        cl = claims.list_claims(self.wiki, subject="attribution window")
        body = "\n".join("[[%s]]" % c["id"] for c in cl)
        if links:
            body += "\n\n## See also\n\n" + "\n".join(links)
        return ("---\ntype: concept\nsubject: %s\ntitle: %s\n"
                "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
                % (subject, title, title, body))

    def test_guard_and_link_validation_coexist(self):
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              self._md("attribution window", "Attribution Window"),
                              None, self.wiki.revision())
        # near-dup refusal message names the twin AND the remediation
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, "wiki/concepts/attribution-window-backup.md",
                self._md("attribution window backup", "Attribution Window Backup"),
                None, self.wiki.revision())
        msg = str(ctx.exception)
        self.assertIn("wiki/concepts/attribution-window.md", msg)
        self.assertIn("--allow-new", msg)
        # self-link rejected even with --allow-new
        with self.assertRaises(TxnError) as ctx2:
            synthesize.write_page(
                self.wiki, "wiki/concepts/other.md",
                self._md("other subject", "Other",
                         links=["[[wiki:concepts/other]]"]),
                None, self.wiki.revision(), allow_new=True)
        self.assertIn("self-link", str(ctx2.exception).lower())

    def test_page_link_requires_existing_target_and_dep(self):
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              self._md("attribution window", "Attribution Window"),
                              None, self.wiki.revision())
        # broken target
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, "wiki/concepts/other.md",
                self._md("other subject", "Other", links=["[[wiki:concepts/nope]]"]),
                None, self.wiki.revision(), allow_new=True)
        self.assertIn("nope", str(ctx.exception))
```

- [ ] **Step 2: Run to verify fail** — `python3 -m unittest discover -s skill/llm-wiki/scripts/tests -p test_synthesize.py 2>&1 | tail -3` → FAIL/ERROR on the new class.
- [ ] **Step 3: Implement** — add the two constants; keep skill-effectiveness's `similar_pages`; add `extract_page_links` (v2 regex `\[\[wiki:([a-z]+/[a-z0-9][a-z0-9\-]*)\]\]`, dedup preserving first-seen order); in `write_page`, after the existing frontmatter validation: (a) v2's link checks — self-link → TxnError "self-link … not allowed"; each link target must exist at `wiki/<kind>/<slug>.md` → TxnError naming it; each link must appear in `dep_list` after auto-derive as `wiki:<kind>/<slug>` token (auto-derive adds them automatically when `dep_list is None`); (b) the near-dup gate for NEW pages with `allow_new=False` → TxnError message: `near-duplicate page: wiki/<kind>/<slug>.md (sim=<s>, title='<t>') already covers this — extend it, or pass --allow-new; twins: <up to 3 paths>`; (c) `allow_new=True` + twins → `front["forced_new"] = True`. Remove all `TODO-UNIFY` markers in this file.
- [ ] **Step 4: Tests green** — targeted run then full suite; expect remaining failures confined to doctor/wiki.py areas (Task 3–4 scope).
- [ ] **Step 5: Commit** — `feat(synthesize): unified write guard + [[wiki:]] link validation (thresholds 0.60/0.70 constants)`

---

### Task 3: Reconcile `doctor.py` — two-tier lint

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/doctor.py`
- Test: `skill/llm-wiki/scripts/tests/test_doctor.py`

**Interfaces:**
- Consumes: Task 2's `extract_page_links`, `NEAR_DUP_LINT_THRESHOLD`, `normalize tokens` (use skill-effectiveness's `_norm_title_tokens`/`_jaccard`, made module-level `_normalize_tokens(text) -> set` / `_jaccard(a, b) -> float`); doctor's existing `add(severity, code, detail, fix)` helper and `run(wiki) -> dict`.
- Produces: findings codes exactly `unlisted_page`, `section_index_unlinked`, `orphan_page`, `dead_end_page`, `walled_garden`, `near_duplicate_subject`, `stale_page_ttl`, `malformed_frontmatter`; `run(wiki)` returns `{"ok", "findings", "counts", "repair_plan"}` (repair_plan populated in Task 7, key present now as `[]`).

- [ ] **Step 1: Write the failing tests** — union both sides' test classes; in skill-effectiveness's `TopologyFindingsTest`: rename `orphan_page` assertions to `unlisted_page`; flip `test_dead_end_page_flagged` to expect `dead_end_page` when the page has NO `[[wiki:]]` links even though it cites claims (v2 semantics), plus assert the detail contains "cites claims"; keep a second dead-end case (no links at all) with the other hint. Add:

```python
class GraphLintTest(unittest.TestCase):
    def setUp(self):
        self.wiki = fresh()
        self.c = add_accepted_claim(self.wiki, "alpha", "has", 1)

    def _page(self, rel, subject, title, body):
        os.makedirs(os.path.dirname(self.wiki.p(rel)), exist_ok=True)
        with open(self.wiki.p(rel), "w") as f:
            f.write("---\ntype: concept\nsubject: %s\ntitle: %s\n"
                    "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
                    % (subject, title, title, body))
        # register deps so verify stays coherent
        txn = Transaction(self.wiki, "test-page")
        txn.stage_write(rel, open(self.wiki.p(rel)).read())
        txn.commit(self.wiki.revision())

    def test_walled_garden_and_orphan(self):
        # three pages linking only among themselves + one hub-linked page
        self._page("wiki/concepts/a.md", "a subject", "A",
                   "[[wiki:concepts/b]]")
        self._page("wiki/concepts/b.md", "b subject", "B",
                   "[[wiki:concepts/c]]")
        self._page("wiki/concepts/c.md", "c subject", "C",
                   "[[wiki:concepts/a]]")
        self._page("wiki/concepts/d.md", "d subject", "D",
                   "[[wiki:concepts/a]]")
        codes = [f["code"] for f in doctor.run(self.wiki)["findings"]]
        self.assertIn("walled_garden", codes)   # {a,b,c} is a garden; d links in
        self.assertIn("orphan_page", codes)     # d has no inbound edges

    def test_stale_ttl_and_near_dup(self):
        self._page("wiki/concepts/old.md", "old subject", "Old", "[[c_x]]")
        with open(self.wiki.p("wiki/concepts/old.md")) as f:
            text = f.read()
        with open(self.wiki.p("wiki/concepts/old.md"), "w") as f:
            f.write(text.replace("created:", "stale_after: 2026-01-01\ncreated:"))
        self._page("wiki/concepts/alpha.md", "alpha topic", "Alpha Topic", "[[c_x]]")
        self._page("wiki/concepts/alpha-copy.md", "alpha topic copy", "Alpha Topic Copy", "[[c_x]]")
        codes = [f["code"] for f in doctor.run(self.wiki)["findings"]]
        self.assertIn("stale_page_ttl", codes)
        self.assertIn("near_duplicate_subject", codes)

    def test_malformed_frontmatter_is_error_not_crash(self):
        with open(self.wiki.p("wiki/concepts/bad.md"), "w") as f:
            f.write("---\nnot: yaml: at: all\n|\nunclosed\n")
        out = doctor.run(self.wiki)
        f = [x for x in out["findings"] if x["code"] == "malformed_frontmatter"]
        self.assertTrue(f)
        self.assertEqual(f[0]["severity"], "error")
```

- [ ] **Step 2: Run to verify fail** — discover test_doctor → FAIL.
- [ ] **Step 3: Implement** — replace both sides' topology code with: `_index_topology_checks(wiki, add)` (skill-effectiveness's logic, code renamed `unlisted_page`) and `_page_graph_checks(wiki, add, pages=None)` building inbound/outbound maps from `extract_page_links(body)` ∪ page-dep tokens (`wiki:<kind>/<slug>` in frontmatter deps), emitting the 5 graph codes per spec semantics (orphan only when >1 valid page; walled garden = undirected component ≥ 3 excluding largest; near-dup on title+subject tokens ≥ 0.70 for unlinked pairs, deterministic pair order; TTL via `stale_after` frontmatter, ISO date < today). Malformed pages → `malformed_frontmatter` error finding, skipped by graph checks. Iterate `sorted(...)` everywhere. `run()` gains `"repair_plan": []`. Remove TODO-UNIFY here.
- [ ] **Step 4: doctor tests green** — full doctor file green; other areas may still be red.
- [ ] **Step 5: Commit** — `feat(doctor): two-tier lint — index fabric (unlisted_page) + page graph (orphan/dead-end/walled-garden/near-dup/TTL), malformed=error`

---

### Task 4: Reconcile `wiki.py` CLI — union flags, suite green

**Files:**
- Modify: `skill/llm-wiki/scripts/wiki.py`, `skill/llm-wiki/SKILL.md`, `skill/llm-wiki/references/COMMANDS.md` (conflict hunks only)

**Interfaces:**
- Consumes: Tasks 2–3 behaviors.
- Produces: `write-page --allow-new` (exists), `ingest --kind` `choices=["session"]` with the v2 guard ("--kind only applies to --file/--text ingest", wiki.py:88 on v2), `doctor --scope <substring>` and `doctor --fix-indexes` flags (fix-indexes no-ops with a warning until Task 7: `"--fix-indexes: not yet implemented"` to stderr, exit 0). Full suite + gates GREEN at end of this task — this is the phase gate.

- [ ] **Step 1: Wire flags** per Interfaces (merge both argument blocks; `cmd_doctor` passes `scope=args.scope`; `--fix-indexes` accepted but inert).
- [ ] **Step 2: Resolve SKILL.md / COMMANDS.md conflicts minimally** — both sides' bullets kept where additive; no new sections (full docs pass is Task 9). Verify `python3 -m unittest discover -s skill/llm-wiki/scripts/tests -p test_skill_package* 2>&1 | tail -1` OK (word-cap test).
- [ ] **Step 3: Full gate** —
```bash
python3 -m unittest discover -s skill/llm-wiki/scripts/tests 2>&1 | tail -1   # OK (skips=11 acceptable)
python3 evals/deterministic/run_gates.py 2>&1 | grep -c PASS                   # 10 (v2's Gate #10 arrives via merge)
```
If Gate #10 is not wired into run_gates by the merge (v2 wired it in `evals/benchmark/` separately), run `python3 evals/benchmark/run_benchmark.py` and record pass — wiring choice goes to Task 9 docs, not here.
- [ ] **Step 4: Commit** — `feat(cli): union flags (--allow-new, ingest --kind session, doctor --scope/--fix-indexes inert) — suites green`

---

### Task 5: KI-1 — corpus fingerprint + resume invalidation

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/compile.py` (umbrella at :467)
- Test: `skill/llm-wiki/scripts/tests/test_compile.py`

**Interfaces:**
- Consumes: `compile_umbrella(wiki, resume=False)`; `hashing.canonical_json` (existing).
- Produces: `compute_corpus_fingerprint(wiki) -> str` (SHA-256 hex[:16] over sorted `(relpath, sha256(content))` of every file under `raw/`, `claims/`, and `.state/manifest.json`); fingerprint stored in each staged run's state at creation; on resume mismatch → that run's staged state discarded (equivalent to fresh staging) and the returned dict includes `"stale_invalidated": [run_ids]`.

- [ ] **Step 1: Failing test**

```python
class ResumeFingerprintTest(unittest.TestCase):
    def test_mutation_invalidates_resume(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "ki1-src", b"v1")
        rid = wc_compile.stage_candidates(wiki, r["source_id"], 1,
                                          [candidate_from(r)], report(r["source_id"], 1))
        fp1 = wc_compile.compute_corpus_fingerprint(wiki)
        sources.ingest(wiki, "text", "ki1-src-2", b"v2")   # corpus changes
        fp2 = wc_compile.compute_corpus_fingerprint(wiki)
        self.assertNotEqual(fp1, fp2)
        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertIn(rid, out.get("stale_invalidated", []))
```

(Reuse this file's existing `candidate_from`/`report` helper names — adapt to the file's actual helpers.)
- [ ] **Step 2: Verify fail** → AttributeError `compute_corpus_fingerprint`.
- [ ] **Step 3: Implement** per Interfaces; fingerprint check runs at the top of the resume branch in `compile_umbrella` (compile.py:467+), before the per-run phase advance; store `corpus_fingerprint` in each run's staged metadata at creation (same place run receipts are written).
- [ ] **Step 4: Green** — targeted + full suite + gates.
- [ ] **Step 5: Commit** — `fix(compile): KI-1 — content-hash corpus fingerprint invalidates stale resume staging`

---

### Task 6: O(N²) → shared inverted token index

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/synthesize.py`, `skill/llm-wiki/scripts/wikicore/doctor.py`
- Test: `skill/llm-wiki/scripts/tests/test_synthesize.py`

**Interfaces:**
- Consumes: `_normalize_tokens` / `_jaccard` (Task 3 made module-level in doctor; MOVE them to synthesize and import into doctor — single home).
- Produces: `build_token_index(entries: dict[str, set]) -> dict[str, list[str]]` and `candidate_pairs(index: dict[str, list[str]]) -> set[tuple[str, str]]` in synthesize; `similar_pages` and doctor's near-dup scan use them (only token-sharing pairs compared; normalized-title map for exact-title).

- [ ] **Step 1: Failing equivalence test**

```python
class InvertedIndexEquivalenceTest(unittest.TestCase):
    def test_indexed_equals_naive(self):
        import random
        rng = random.Random(42)
        entries = {"p%d" % i: set(rng.sample(
            ["attr", "window", "retry", "order", "cache", "token", "graph",
             "page", "link", "stale"], rng.randint(1, 4)))
            for i in range(60)}
        naive = {(a, b) for i, a in enumerate(sorted(entries))
                 for b in sorted(entries)[i+1:]
                 if entries[a] & entries[b]}
        idx = synthesize.build_token_index(entries)
        self.assertEqual(naive, synthesize.candidate_pairs(idx))

    def test_similar_pages_matches_naive_scan(self):
        wiki = fresh()
        # 30 pages with overlapping subjects; assert similar_pages("x") equals
        # a manual all-pages scan at threshold 0.60
        for i in range(30):
            add_accepted_claim(wiki, "subject %02d" % i, "p", i)
            write_test_page(wiki, "subject %02d" % i)   # helper from this file
        want = naive_all_pages_scan(wiki, "subject 07", 0.60)  # local helper
        got = {(t["section"], t["slug"])
               for t in synthesize.similar_pages(wiki, "concepts", "subject 07", "Subject 07")}
        self.assertEqual(want, got)
```

- [ ] **Step 2: Verify fail** → AttributeError `build_token_index`.
- [ ] **Step 3: Implement** the two functions; rewire `similar_pages` (fast-path: skip candidates sharing zero tokens unless exact-title) and doctor's `near_duplicate_subject` pair generation to `candidate_pairs`. Semantics unchanged — only the pair enumeration.
- [ ] **Step 4: Green** — targeted + full suite + gates; also `python3 - <<'EOF'` smoke: build 300 pages in a temp wiki via the test helpers pattern, time `doctor.run` — record wall time in the report (informational, no threshold).
- [ ] **Step 5: Commit** — `perf(lint): shared inverted token index — lossless sub-quadratic pair enumeration`

---

### Task 7: Lint → repair — `rebuild_section_indexes` + `--fix-indexes` + `repair_plan`

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/pages.py`, `skill/llm-wiki/scripts/wikicore/doctor.py`, `skill/llm-wiki/scripts/wiki.py`
- Test: `skill/llm-wiki/scripts/tests/test_pages.py`, `test_doctor.py`

**Interfaces:**
- Consumes: `build_pages`'s section-index + overview generation block (pages.py:266-299); `Transaction`, `deps.replace`.
- Produces: `pages.rebuild_section_indexes(wiki) -> list[str]` (regenerates every `wiki/<kind>/index.md` + `wiki/overview.md` through the SAME rendering code as build_pages — extract the block into a shared function both call; returns written rel paths; runs in one staged transaction); `doctor --fix-indexes` calls it, then re-runs lint and reports `{"repaired": ["unlisted_page: n", ...], "repair_plan": [...]}`; `repair_plan` (also emitted without the flag) = list of `{"code", "path", "action"}` for graph findings, action strings per spec ("add `[[wiki:<ref>]]` to a related page's See-also and deps").

- [ ] **Step 1: Failing tests**

```python
class FixIndexesTest(unittest.TestCase):
    def test_fix_repairs_unlisted_and_is_idempotent(self):
        wiki = fresh()
        add_accepted_claim(wiki, "alpha", "has", 1)
        pages.build_pages(wiki) if hasattr(pages, "build_pages") else None
        # hand-plant a stray page (not via write_page)
        stray = wiki.p("wiki/concepts/stray-page.md")
        os.makedirs(os.path.dirname(stray), exist_ok=True)
        open(stray, "w").write("---\ntype: concept\nsubject: stray page\n"
                               "title: Stray\ncreated: 2026-09-15T00:00:00Z\n"
                               "deps: []\n---\n\n# Stray\n\n[[c_x]]\n")
        before = [f["code"] for f in doctor.run(wiki)["findings"]]
        self.assertIn("unlisted_page", before)
        fixed = pages.rebuild_section_indexes(wiki)
        self.assertIn("wiki/concepts/index.md", fixed)
        after = [f["code"] for f in doctor.run(wiki)["findings"]]
        self.assertNotIn("unlisted_page", after)
        # idempotent: run twice → byte-identical
        first = open(wiki.p("wiki/concepts/index.md"), "rb").read()
        pages.rebuild_section_indexes(wiki)
        self.assertEqual(first, open(wiki.p("wiki/concepts/index.md"), "rb").read())

    def test_repair_plan_lists_editorial_actions(self):
        wiki = fresh()
        add_accepted_claim(wiki, "alpha", "has", 1)
        write_lone_page(wiki)  # helper: a page with no wiki links
        plan = doctor.run(wiki)["repair_plan"]
        self.assertTrue(any(p["code"] in ("orphan_page", "dead_end_page")
                            for p in plan))
        self.assertTrue(all("[[wiki:" in p["action"] for p in plan))
```

- [ ] **Step 2: Verify fail** → AttributeError `rebuild_section_indexes`.
- [ ] **Step 3: Implement** — extract + share the index/overview rendering between `build_pages` and `rebuild_section_indexes`; wire `cmd_doctor`'s inert `--fix-indexes` to real behavior (call rebuild, re-lint, emit repaired counts); build `repair_plan` from graph findings in `run()`.
- [ ] **Step 4: Green** — targeted + full suite + gates.
- [ ] **Step 5: Commit** — `feat(doctor): lint→repair — rebuild_section_indexes, --fix-indexes, machine-readable repair_plan`

---

### Task 8: Ingest triage manifest

**Files:**
- Modify: `skill/llm-wiki/scripts/wiki.py`, `skill/llm-wiki/scripts/wikicore/sources.py`
- Test: `skill/llm-wiki/scripts/tests/test_sources.py`

**Interfaces:**
- Consumes: v2's `ingest --kind` (choices `["session"]`).
- Produces: `sources.triage(dirpath: str) -> dict` — deterministic proposals `{"files": [{"file": abs, "kind": "session"|None, "priority": 1-3, "rationale": str}]}`; heuristics: `.jsonl`/`.json` with `"role"/"message"` keys or filename matching `*session*|*transcript*|*chat*` → kind `session`, priority 1; `.md/.txt` → priority 2; size > 500 KB → priority 3 with rationale; CLI `triage --dir P [--out manifest.json]` (prints JSON) and `ingest --triage manifest.json` (batch: ingests each file, applying `--kind` when set; one failure logs and continues; summary JSON lists per-file source_ids).

- [ ] **Step 1: Failing tests**

```python
class TriageTest(unittest.TestCase):
    def test_proposal_and_roundtrip(self):
        import json as _json
        wiki = fresh()
        d = tempfile.mkdtemp()
        open(os.path.join(d, "agent-session.jsonl"), "w").write(
            '{"role":"user","message":"hi"}\n')
        open(os.path.join(d, "notes.md"), "w").write("# Notes\n")
        manifest = sources.triage(d)
        kinds = {f["file"].split("/")[-1]: f["kind"] for f in manifest["files"]}
        self.assertEqual(kinds["agent-session.jsonl"], "session")
        self.assertIsNone(kinds["notes.md"])
        # batch apply
        out = sources.ingest_triage(wiki, manifest)
        self.assertEqual(len(out["ingested"]), 2)
        self.assertTrue(all("source_id" in r for r in out["ingested"]))
```

- [ ] **Step 2: Verify fail** → AttributeError `triage`.
- [ ] **Step 3: Implement** `triage` + `ingest_triage(wiki, manifest) -> {"ingested": [...], "failed": [...]}`; CLI wiring (`triage` subcommand; `ingest --triage` path handled in `cmd_ingest` before `--file`).
- [ ] **Step 4: Green** — targeted + full suite + gates.
- [ ] **Step 5: Commit** — `feat(ingest): triage manifest — deterministic kind/priority proposals + batch --triage ingest`

---

### Task 9: Docs wiring

**Files:**
- Modify: `skill/llm-wiki/SKILL.md`, `references/COMMANDS.md`, `references/EVALUATION.md`, `skill/wiki-write/SKILL.md`

**Interfaces:**
- Consumes: every implemented behavior (Tasks 2–8); v2's 6-lesson content (arrived via merge in COMMANDS.md/SKILL.md conflicts resolved in Task 4).
- Produces: docs consistent with behavior; word-cap test green; `EVALUATION.md` documents 5 test layers + both eval tiers + matrix; Gate #10 wired into `evals/deterministic/run_gates.py` ONLY if trivially delegable (call `run_benchmark.py`'s `main()`; if coupling is invasive, document it as the release runbook step instead and say so in the task report).

- [ ] **Step 1: Write the doc updates** — COMMANDS.md: 6 lessons merged per-command (classify-first at ingest incl. `triage`; extend-first at write incl. gate thresholds + `--allow-new`; two-pass compile; See-also dual-linking with `[[wiki:]]` + deps token; navigation ladder; human-only `verified`); doctor section gains all 7 codes + `--scope`/`--fix-indexes` + repair_plan; EVALUATION.md gains the five-layer table (condensed from spec §6) + matrix section; wiki-write gains See-also guidance. SKILL.md: one triage router row if word budget allows (must stay < 600).
- [ ] **Step 2: Verify** — `grep -q "allow-new" skill/llm-wiki/references/COMMANDS.md`; `grep -q "walled_garden" skill/llm-wiki/references/COMMANDS.md`; word-cap test OK; `python3 skill/llm-wiki/scripts/wiki.py triage --help` works; full suite + gates green.
- [ ] **Step 3: Commit** — `docs: unified skill wiring — 6 lessons + 7 lint codes + triage + 5-layer testing + matrix`

---

### Task 10: Smoke harness

**Files:**
- Create: `evals/smoke/run_smoke.py`, `evals/smoke/__init__.py` (empty)

**Interfaces:**
- Consumes: `install.sh` modes (default omp, `--agents`, `--cellockai`), engine CLI.
- Produces: `run_smoke.py [--mode omp|agents|cellockai|all]` — per available mode: install → resolve installed engine path → temp project `init` → `ingest --file evals/golden-corpus/docs/adr-007-retries.md` → `verify` ok:true → `doctor` no error-severity findings → `query-prepare --question "retries"` exits 0 → `install.sh --uninstall` → assert no leftovers under the mode's skills dir for llm-wiki entries. Missing runtime/dir → recorded `"skipped"` with reason, exit 0; a failed executed step → exit 1. Runtime budget: whole run < 10 s per mode (subprocess engine calls, no LLM).

- [ ] **Step 1: Write the harness** (single file, argparse, subprocess, tempdirs, JSON summary to stdout; last line `SMOKE OK`/`SMOKE FAIL`).
- [ ] **Step 2: Run it** — `python3 evals/smoke/run_smoke.py --mode all` → modes present on this machine pass (omp, agents), cellockai likely `skipped` (no `cellock` CLI) — that IS the pass shape.
- [ ] **Step 3: Commit** — `test(smoke): install-to-first-query smoke per install mode (<10s, explicit skips)`

---

### Task 11: Perf baselines + budget asserts

**Files:**
- Create: `evals/perf/baselines.json`, `evals/perf/check_budgets.py`, `evals/perf/__init__.py` (empty)

**Interfaces:**
- Consumes: bench driver wall-times (`evals/benchmarks/` step records), matrix timing hooks (Task 13).
- Produces: `baselines.json` = `{"lint_sweep_300_pages_s": <measured>, "similar_pages_300_s": <measured>, "pack_tokens_avg": 263.8, "build_wall_s": <measured>}` measured on HEAD at commit time (script `check_budgets.py --record` regenerates); `check_budgets.py` (no args) re-measures and fails ONLY on >10× regression over any baseline; wired as the CI cheap tier (documented in EVALUATION.md runbook — no CI config exists in-repo, so the wiring is: gates script calls it last if `evals/perf/baselines.json` exists).

- [ ] **Step 1: Write both files** — measurement reuses the bench driver to build the standard wiki + Task 6's 300-page smoke pattern for lint timing.
- [ ] **Step 2: Record + verify** — `python3 evals/perf/check_budgets.py --record` then `python3 evals/perf/check_budgets.py` → exit 0.
- [ ] **Step 3: Commit** — `bench(perf): baselines.json + >10x budget gate (cheap CI tier)`

---

### Task 12: E2E harness — agent runtimes

**Files:**
- Create: `evals/e2e/__init__.py` (empty), `evals/e2e/run_e2e.py`, `evals/e2e/scenarios.json`, `evals/e2e/assertions.py`

**Interfaces:**
- Consumes: installed skill (omp native, `--agents`), runtimes `omp -p --no-session` and `claude -p`; `~/.cellockai` (probe-only).
- Produces: `run_e2e.py [--runtime omp|claude|cellock|all] [--scenario S1..S6|all]` — per scenario: copy golden corpus to temp project, send the scripted instruction, then run deterministic assertions via `assertions.py` (pure filesystem/CLI checks, no LLM): S1 artifacts + verify ok + citation resolves; S2 near-dup → assert NO second page created AND agent output mentions extend/allow-new; S3 triage manifest exists + sources ingested with session kind; S4 correction → current answer 7d with 30d superseded; S5 refusal tokens in output; S6 no `grep`/`rg` in transcript command list for the query scenario. Output JSONL per run (runtime, scenario, pass, commands_used, tokens_estimate) appended to `evals/e2e/results.jsonl`. Runtime absent → explicit `skipped` row. Token budget per scenario: fail if transcript > 20k tokens.

`scenarios.json` shape:
```json
{"S1": {"instruction": "In {project}: run wiki-init, ingest docs/adr-007-retries.md, compile it, then answer 'How many retries are configured?' citing claim ids.",
        "assert": "s1"},
 "S2": {"instruction": "In {project} (already built): write a new concept page 'Retry Policy Notes' — handle any refusal the engine gives you.",
        "assert": "s2"}}
```
(S3–S6 analogous; instructions reference the engine path absolutely.)

- [ ] **Step 1: Write the three files** with all 6 scenarios + assertion functions (each `assert_sN(project_root, transcript_path) -> {"pass": bool, "detail": str}`).
- [ ] **Step 2: Run** — `python3 evals/e2e/run_e2e.py --runtime omp` → ≥ S1, S2, S5 green (S4 needs the correction fixture — include it in the temp project builder); claude runtime recorded; cellock skipped-with-reason.
- [ ] **Step 3: Commit** — `test(e2e): agent-runtime scenarios S1-S6 — omp/claude drivers, artifact assertions, explicit skips`

---

### Task 13: Matrix core — generator + runner + E1 + E6

**Files:**
- Create: `evals/matrix/__init__.py` (empty), `evals/matrix/generator.py`, `evals/matrix/runner.py`, `evals/matrix/test_matrix.py`

**Interfaces:**
- Consumes: `evals/benchmarks/driver.py` (Engine, build_wiki), `git archive` engine extraction (proven pattern).
- Produces: `generator.synthetic_corpus(n: int, seed: int = 42, fault: str|None = None) -> dirpath` — N markdown sources across `docs/`+`notes/`+`config/` with seeded subjects (60 distinct topic words, Zipf-ish reuse), planted claims manifest for gold questions, and fault injection when `fault ∈ {"near_dup_flood", "orphan_farm", "walled_garden", "stale_ttl_farm"}`; `runner.run(engine_refs: dict[str, str], scenarios: list, experiments: list) -> matrix dict` where engine_refs maps label → wiki.py path (runner gains `--extract <commit>` to `git archive` a commit into /tmp); E1 = per-engine × golden + synthetic(100): the existing bench metric set; E6 = run E1 twice, assert identical metric sections (raises on drift). Output `evals/matrix/results/matrix-<date>.json`.

- [ ] **Step 1: Write failing tests** — generator determinism (same seed → identical tree hashes), fault modes produce their planted faults (doctor detects each class on the built wiki); runner E6 determinism on one tiny engine pair.
- [ ] **Step 2: Implement** generator + runner.
- [ ] **Step 3: Execute E1 + E6** — engines: base `bb7a51f`, `skill-v2` (f42e7bc), HEAD. Commit results JSON.
- [ ] **Step 4: Commit** — `bench(matrix): seeded generator, version-ladder runner (E1), determinism gate (E6) + first results`

---

### Task 14: Matrix experiments E2/E4/E3 + A/B re-run + phase results

**Files:**
- Modify: `evals/matrix/runner.py`
- Create: `evals/matrix/results/` (final artifacts), `docs/superpowers/plans/…` sibling report note not needed — results + compare.md only.

**Interfaces:**
- Consumes: Task 13 runner + generator; `evals/benchmarks/bench.py` + `compare.py`.
- Produces: E2 (unified HEAD × N=100/300/1000: per-phase wall times, lint sweep seconds, pack tokens, query metrics — line lists per N); E4 (per fault fixture: lint recall = detected/planted, guard block rate on near_dup_flood, repair-plan coverage, false-positive count on clean corpus); E3 (k=10 maintenance cycles on synthetic(300): scripted ingest→compile→correction→recompile→churn→lint→repair→snapshot; metrics per cycle: staleness caught/missed, findings before/after repair, ops count); plus the A/B re-run `python3 evals/benchmarks/bench.py --engine <base> --label base2` + HEAD + `compare.py` — committed as `compare-unified.md`. Every results file embeds `experiment`, `engine_refs`, `date`.

- [ ] **Step 1: Implement E2/E4/E3** in runner.py (E3 cycle script is data: a list of engine CLI calls + mutations, executed through the driver, asserted via doctor/query outputs).
- [ ] **Step 2: Execute** all experiments on HEAD + base where required; E3 ≥ 5 cycles minimum (10 target); commit results.
- [ ] **Step 3: A/B re-run** and commit `evals/benchmarks/results/compare-unified.md` with Run metadata header (base bb7a51f vs unified HEAD).
- [ ] **Step 4: Full final gate** — unit suite, gates, smoke, perf budgets, e2e (omp), matrix determinism — all green; `git status` clean.
- [ ] **Step 5: Commit** — `bench(matrix): E2 scale sweep, E4 failure drills, E3 maintenance sim + unified A/B results`

---

## Self-Review (completed during planning)

- **Spec coverage:** §1→T1 · §2→T2/T3/T4 · §3.1→T7 · §3.2→T5 · §3.3→T6 · §3.4→T8 · §4→T9 · §5→T14 (A/B) + T11 · §6→T10/T11/T12 + layer 1-2 throughout · §7→T13/T14 · success criteria 1-8 → T4/T7/T8 (1), T5-T8 tests (2), T10 (3), T11 (4), T12 (5), T13/T14 (6), T14 (7), T9 (8).
- **Placeholders:** scenarios S3–S6 instructions are compressed as "analogous" — Task 12's implementer writes them concretely from the spec's scenario list; flagged here deliberately as the one elastic spot, bounded by `scenarios.json`'s shown shape and `assertions.py`'s required function set.
- **Type consistency:** `compute_corpus_fingerprint(wiki) -> str` (T5) used nowhere else; `extract_page_links`/`NEAR_DUP_LINT_THRESHOLD` (T2) consumed by T3; `rebuild_section_indexes(wiki) -> list[str]` (T7) consumed by cmd_doctor same task; `triage(dirpath) -> dict`/`ingest_triage(wiki, manifest)` (T8) consumed by cmd_ingest; `build_token_index`/`candidate_pairs` (T6) home in synthesize, imported by doctor.
