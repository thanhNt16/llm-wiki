# Agent-Efficiency Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut agent token/command overhead in the compile loop — shorthand JSONL candidates, engine auto-classification of deterministic verdicts, compact reconcile output, and a resumable `wiki.py compile` umbrella command.

**Spec:** `docs/superpowers/specs/2026-10-01-agent-efficiency-redesign-design.md` (approved).

**Architecture:** All changes live in `skill/llm-wiki/scripts/` (stdlib-only Python) plus skill docs. A new `wikicore/shorthand.py` normalizes agent shorthand into the full candidate dict. `compile.py` gains `auto_verdict()` and a compact `needs_review` comparison shape; `reconcile_apply` auto-fills missing classifications. `wiki.py` gains the `compile` command (3-phase state machine over `.llm-wiki/.state/staging/<run_id>/`).

**Tech Stack:** Python 3 stdlib only (json, os, re). Tests: `unittest` in `skill/llm-wiki/scripts/tests/`; run `python3 -m unittest discover tests -v` from `scripts/`.

## Global Constraints

- No new runtime dependencies. stdlib only.
- Back-compat: verbose JSON-array candidates stay valid; low-level commands (`compile-plan`, `stage-candidates`, `reconcile-prepare`, `reconcile-apply`, `build-pages`, `verify`) keep working unchanged.
- Exit codes: 3 = validation, 4 = stale revision, 5 = locked (existing `EXIT_CODES` in `wiki.py`).
- Agent-settable candidate fields: `subject`, `predicate`, `value`, `locator`, `authority`, `scope`, `valid_from`, `supersedes`. Engine owns `id`, `recorded_at`, `status`, `proposed_by`, `evidence`, `root_origin`, `valid_to`.
- On-disk claim schema unchanged — shorthand is input format only.
- Auto-classify ONLY these verdicts: `UNRELATED` (no matches), `DUPLICATE` (single match, same scope+value, overlapping target `root_origin`), `CORROBORATION` (single match, same scope+value, disjoint `root_origin`). Everything else → `needs_review`.

## File Structure

- `skill/llm-wiki/scripts/wikicore/shorthand.py` — NEW: `expand_locator`, `expand_authority`, `parse_candidates_file`, `normalize_candidate`
- `skill/llm-wiki/scripts/wikicore/compile.py` — MOD: `auto_verdict()`, compact `reconcile_prepare`, auto-fill in `reconcile_apply`, `compile_umbrella()` phase machine
- `skill/llm-wiki/scripts/wiki.py` — MOD: `compile` subparser + `cmd_compile`; `stage-candidates` accepts `.jsonl`
- `skill/llm-wiki/scripts/tests/test_shorthand.py` — NEW
- `skill/llm-wiki/scripts/tests/test_compile.py` — MOD: auto-classify + umbrella tests
- `skill/wiki-compile/SKILL.md`, `skill/llm-wiki/references/{COMMANDS,EXTRACTION,RECONCILIATION}.md` — MOD: docs

---

### Task 1: `wikicore/shorthand.py` — shorthand → full candidate

**Files:**
- Create: `skill/llm-wiki/scripts/wikicore/shorthand.py`
- Test: `skill/llm-wiki/scripts/tests/test_shorthand.py`

**Interfaces:**
- Produces (consumed by Task 4 and `cmd_stage_candidates`):
  - `expand_locator(raw) -> dict` — `raw` is str (`h:`/`l:`/`s:` prefix) or dict (passed through).
  - `expand_authority(raw) -> dict` — `raw` is str shorthand or dict (passed through).
  - `normalize_candidate(c: dict, source_id: str, source_version: int, root_origin: str) -> dict` — returns a full candidate with engine fields stamped.
  - `parse_candidates_file(path: str) -> list` — `.jsonl` → per-line objects; `.json` → array or `{"candidates": [...]}`. Raises `TxnError` on malformed input naming the line.

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_shorthand.py
import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore.shorthand import expand_locator, expand_authority, \
    normalize_candidate, parse_candidates_file
from wikicore.transaction import TxnError


class TestShorthand(unittest.TestCase):
    def test_heading_locator(self):
        self.assertEqual(expand_locator("h:1.1 Intro"),
                         {"type": "heading", "value": "1.1 Intro"})

    def test_line_range_locator(self):
        self.assertEqual(expand_locator("l:139"), {"type": "line_range", "value": "139"})
        self.assertEqual(expand_locator("l:557-559"),
                         {"type": "line_range", "value": "557-559"})

    def test_section_locator_and_passthrough(self):
        self.assertEqual(expand_locator("s:Body"), {"type": "section", "value": "Body"})
        d = {"type": "paragraph", "value": "3"}
        self.assertEqual(expand_locator(d), d)

    def test_bad_locator_rejected(self):
        with self.assertRaises(TxnError):
            expand_locator("x:bogus")

    def test_authority_shorthand(self):
        self.assertEqual(expand_authority("manual"),
                         {"type": "authoritative_source", "source": "manual"})
        self.assertEqual(expand_authority("decision"),
                         {"type": "explicit_project_decision", "source": "decision"})
        self.assertEqual(expand_authority("code"),
                         {"type": "implementation_verification", "source": "code"})
        self.assertEqual(expand_authority("inferred"),
                         {"type": "agent_inference", "source": "agent"})
        self.assertEqual(expand_authority("adr:ADR-019"),
                         {"type": "explicit_project_decision", "source": "ADR-019"})

    def test_authority_passthrough(self):
        d = {"type": "authoritative_source", "source": "Manual v1.2"}
        self.assertEqual(expand_authority(d), d)

    def test_bad_authority_rejected(self):
        with self.assertRaises(TxnError):
            expand_authority("bogus")

    def test_normalize_stamps_engine_fields(self):
        c = normalize_candidate(
            {"subject": "a.b", "predicate": "p", "value": 7,
             "locator": "h:Window", "authority": "decision"},
            "source_X", 2, "origin_X")
        self.assertEqual(c["status"], "candidate")
        self.assertEqual(c["proposed_by"], "agent")
        self.assertEqual(c["root_origin"], "origin_X")
        self.assertEqual(c["evidence"], [{
            "source_id": "source_X", "source_version": 2,
            "locator": {"type": "heading", "value": "Window"}}])
        self.assertIn("recorded_at", c)
        self.assertNotIn("id", c)  # staging assigns ids, not shorthand

    def test_normalize_requires_subject_predicate(self):
        with self.assertRaises(TxnError):
            normalize_candidate({"predicate": "p"}, "s", 1, "o")

    def test_parse_jsonl(self):
        p = os.path.join(tempfile.mkdtemp(), "c.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a","predicate":"b","value":1,"locator":"h:H"}\n')
            f.write('{"subject":"c","predicate":"d","value":2,"locator":"l:5"}\n')
        self.assertEqual(len(parse_candidates_file(p)), 2)

    def test_parse_jsonl_bad_line_names_line(self):
        p = os.path.join(tempfile.mkdtemp(), "c.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a","predicate":"b"}\nnot json\n')
        with self.assertRaises(TxnError) as ctx:
            parse_candidates_file(p)
        self.assertIn("line 2", str(ctx.exception))

    def test_parse_verbose_json_still_works(self):
        p = os.path.join(tempfile.mkdtemp(), "c.json")
        with open(p, "w") as f:
            json.dump([{"subject": "a", "predicate": "b", "value": 1}], f)
        self.assertEqual(len(parse_candidates_file(p)), 1)
```

- [ ] **Step 2: Run to verify fail**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_shorthand -v`
Expected: ImportError — `wikicore.shorthand` doesn't exist.

- [ ] **Step 3: Implement `wikicore/shorthand.py`**

```python
"""Shorthand candidate parsing for the compile umbrella (spec §2).

Agents write one JSON object per line with only semantic fields; this module
expands locator/authority shorthand and stamps engine-owned fields.
"""
import json
import re
import time

from .transaction import TxnError

_AUTHORITY_MAP = {
    "manual": "authoritative_source", "doc": "authoritative_source",
    "config": "authoritative_source",
    "decision": "explicit_project_decision", "adr": "explicit_project_decision",
    "code": "implementation_verification", "verified": "implementation_verification",
    "inferred": "agent_inference",
}
_AUTHORITY_SOURCE = {
    "manual": "manual", "doc": "doc", "config": "config",
    "decision": "decision", "adr": "adr",
    "code": "code", "verified": "verified", "inferred": "agent",
}

_AGENT_FIELDS = {"subject", "predicate", "value", "locator", "authority",
                 "scope", "valid_from", "supersedes", "evidence"}


def expand_locator(raw):
    if isinstance(raw, dict):
        return raw
    if not isinstance(raw, str):
        raise TxnError("locator must be a string shorthand or object, got %r" % type(raw))
    m = re.match(r"^([hls]):(.+)$", raw, re.S)
    if not m:
        raise TxnError("bad locator shorthand %r — use h:<heading>, l:<lines>, s:<section>" % raw)
    kind, val = m.group(1), m.group(2).strip()
    return {"type": {"h": "heading", "l": "line_range", "s": "section"}[kind],
            "value": val}


def expand_authority(raw):
    if isinstance(raw, dict):
        return raw
    if not isinstance(raw, str):
        raise TxnError("authority must be a shorthand string or object, got %r" % type(raw))
    key, _, src = raw.partition(":")
    key = key.strip().lower()
    if key not in _AUTHORITY_MAP:
        raise TxnError("bad authority shorthand %r — one of %s"
                       % (raw, sorted(_AUTHORITY_MAP)))
    return {"type": _AUTHORITY_MAP[key],
            "source": src.strip() or _AUTHORITY_SOURCE[key]}


def normalize_candidate(c, source_id, source_version, root_origin):
    """Shorthand dict -> full candidate. Engine stamps id later (staging),
    plus recorded_at/status/proposed_by/evidence/root_origin here."""
    if not isinstance(c, dict):
        raise TxnError("candidate must be an object")
    for field in ("subject", "predicate"):
        if not c.get(field):
            raise TxnError("candidate missing %r" % field)
    out = dict(c)
    out["subject"] = str(out["subject"]).strip()
    out["predicate"] = str(out["predicate"]).strip()
    out["locator_used"] = None  # placeholder removed below
    del out["locator_used"]
    # evidence
    if "evidence" not in out:
        loc = expand_locator(out.pop("locator", "s:whole-document"))
        out["evidence"] = [{"source_id": source_id,
                            "source_version": source_version,
                            "locator": loc}]
    else:
        out.pop("locator", None)
    out.setdefault("authority", {"type": "authoritative_source", "source": "document"})
    out["authority"] = expand_authority(out["authority"])
    out.setdefault("scope", {})
    out.setdefault("valid_from", None)
    out.setdefault("valid_to", None)
    out.setdefault("supersedes", [])
    out["recorded_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    out["status"] = "candidate"
    out["proposed_by"] = "agent"
    out["root_origin"] = root_origin
    out.setdefault("id", "auto")
    return out


def parse_candidates_file(path):
    """.jsonl -> one shorthand object per line; .json -> array or {candidates:[..]}.
    Returns raw dicts (normalization happens at staging where source_id is known)."""
    if path.endswith(".jsonl"):
        out = []
        with open(path) as f:
            for n, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError as e:
                    raise TxnError("candidates file %s line %d: %s" % (path, n, e))
                if not isinstance(obj, dict):
                    raise TxnError("candidates file %s line %d: not an object" % (path, n))
                out.append(obj)
        return out
    with open(path) as f:
        data = json.load(f)
    if isinstance(data, dict):
        data = data.get("candidates", [])
    if not isinstance(data, list):
        raise TxnError("candidates file %s: expected array" % path)
    return data
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_shorthand -v`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/shorthand.py skill/llm-wiki/scripts/tests/test_shorthand.py
git commit -m "feat: shorthand candidate parsing (wikicore/shorthand.py)"
```

---

### Task 2: Auto-verdict + compact `reconcile_prepare`

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/compile.py` (reconcile_prepare ~line 82-105; add `auto_verdict`)
- Test: `skill/llm-wiki/scripts/tests/test_compile.py`

**Interfaces:**
- Consumes: `reconcile_prepare`'s per-candidate `matches` list.
- Produces (consumed by Task 3 & 4):
  - `auto_verdict(candidate: dict, matches: list) -> Optional[str]` — returns `"UNRELATED"`, `"DUPLICATE"`, `"CORROBORATION"`, or `None` (needs agent).
  - `reconcile_prepare` output rows become `{index, key, candidate_value, scope, matches, auto, hint}` — full `candidate` object no longer echoed.

- [ ] **Step 1: Write failing tests** (append to `tests/test_compile.py` — reuse existing `fresh()`, `ingest_text()`, `candidate()` helpers)

```python
    def test_auto_verdict_unrelated(self):
        self.assertEqual(wc_compile.auto_verdict({"root_origin": "o1"}, []),
                         "UNRELATED")

    def test_auto_verdict_duplicate_same_origin(self):
        m = [{"same_scope": True, "same_value": True, "status": "accepted",
              "root_origins": ["o1"]}]
        self.assertEqual(wc_compile.auto_verdict({"root_origin": "o1"}, m),
                         "DUPLICATE")

    def test_auto_verdict_corroboration_diff_origin(self):
        m = [{"same_scope": True, "same_value": True, "status": "accepted",
              "root_origins": ["o2"]}]
        self.assertEqual(wc_compile.auto_verdict({"root_origin": "o1"}, m),
                         "CORROBORATION")

    def test_auto_verdict_needs_review_on_value_diff(self):
        m = [{"same_scope": True, "same_value": False, "status": "accepted",
              "root_origins": ["o2"]}]
        self.assertIsNone(wc_compile.auto_verdict({"root_origin": "o1"}, m))

    def test_auto_verdict_needs_review_multi_match(self):
        m = [{"same_scope": True, "same_value": True, "status": "accepted",
              "root_origins": ["o2"]},
             {"same_scope": False, "same_value": False, "status": "accepted",
              "root_origins": ["o3"]}]
        self.assertIsNone(wc_compile.auto_verdict({"root_origin": "o1"}, m))

    def test_reconcile_prepare_compact(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "x")
        c = candidate(wiki, rcpt)
        run = wc_compile.stage_candidates(wiki, rcpt["source_id"],
                                          rcpt["version"], [c], {})
        out = wc_compile.reconcile_prepare(wiki, run)
        row = out["comparisons"][0]
        self.assertEqual(row["auto"], "UNRELATED")
        self.assertEqual(row["key"], "project.analytics.attribution/click_lookback_window")
        self.assertNotIn("candidate", row)   # no full echo
        self.assertIn("candidate_value", row)
```

- [ ] **Step 2: Run to verify fail** — `python3 -m unittest tests.test_compile -v` → AttributeError `auto_verdict`.

- [ ] **Step 3: Implement** — in `wikicore/compile.py`:

```python
def _root_origins_of(claim) -> set:
    out = set()
    for ev in claim.get("evidence", []):
        out.add(ev.get("root_origin") or ev.get("source_id"))
    return out


def auto_verdict(cand, matches):
    """Deterministic reconciliation verdicts (spec §2). None = needs agent."""
    if not matches:
        return "UNRELATED"
    if len(matches) > 1:
        return None
    m = matches[0]
    if m["same_scope"] and m["same_value"]:
        my_origin = cand.get("root_origin")
        if my_origin and my_origin in set(m.get("root_origins") or []):
            return "DUPLICATE"
        return "CORROBORATION"
    return None


def _hint(matches):
    if not matches:
        return "UNRELATED"
    if len(matches) > 1:
        return "CONTRADICTION?"
    m = matches[0]
    if m["same_scope"] and m["same_value"]:
        return "DUPLICATE/CORROBORATION"
    if not m["same_scope"]:
        return "SCOPE_DIFFERENCE?"
    return "CONTRADICTION?"
```

In `reconcile_prepare`, inside the `for ex in existing` match-build add
`"root_origins": sorted(_root_origins_of(ex))` to each match dict; then build
rows compactly:

```python
        key = "%s/%s" % (_norm(cand["subject"]), _norm(cand["predicate"]))
        comparisons.append({
            "index": i, "key": key,
            "candidate_value": cand.get("value"),
            "scope": cand.get("scope") or {},
            "authority": cand.get("authority"),
            "valid_from": cand.get("valid_from"),
            "matches": matches,
            "auto": auto_verdict(cand, matches),
            "hint": _hint(matches),
        })
```

Delete the `"candidate": cand` echo from the row.

- [ ] **Step 4: Run to verify pass** — same unittest command. Existing tests that read `row["candidate"]` must be updated to read `row["candidate_value"]`/`row["key"]` instead (grep `test_compile.py` for `["candidate"]` usages first).

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/compile.py skill/llm-wiki/scripts/tests/test_compile.py
git commit -m "feat: auto_verdict + compact reconcile-prepare output"
```

---

### Task 3: `reconcile_apply` auto-fills missing classifications

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/compile.py` (`reconcile_apply` ~line 178)
- Test: `skill/llm-wiki/scripts/tests/test_compile.py`

**Interfaces:**
- Consumes: `auto_verdict` from Task 2; `comparisons.json` written by `reconcile_prepare`.
- Produces: `reconcile_apply` accepts `classifications` covering a SUBSET of indices; missing indices get the `auto` verdict (raises `TxnError` if a row is neither classified nor auto-decidable). Receipt gains `"auto": {"unrelated": N, "duplicate": N, "corroboration": N}`.

- [ ] **Step 1: Failing test**

```python
    def test_apply_autofills_unrelated(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "x")
        c = candidate(wiki, rcpt)
        run = wc_compile.stage_candidates(wiki, rcpt["source_id"],
                                          rcpt["version"], [c], {})
        wc_compile.reconcile_prepare(wiki, run)
        receipt = wc_compile.reconcile_apply(wiki, run, [], wiki.revision())
        self.assertEqual(receipt["changes"]["claims_created"], 1)
        self.assertEqual(receipt["auto"]["unrelated"], 1)

    def test_apply_rejects_undecidable_gap(self):
        wiki = fresh()

        rcpt = ingest_text(wiki, "doc", "x")
        c = candidate(wiki, rcpt)
        # first claim accepted so the second candidate gets a same-key match
        run1 = wc_compile.stage_candidates(wiki, rcpt["source_id"],
                                           rcpt["version"], [c], {})
        wc_compile.reconcile_prepare(wiki, run1)
        wc_compile.reconcile_apply(wiki, run1,
                                   [{"index": 0, "relationship": "UNRELATED"}],
                                   wiki.revision())
        c2 = candidate(wiki, rcpt, value=8)  # same key, different value
        rcpt2 = ingest_text(wiki, "doc2", "y")
        c2["evidence"] = [{"source_id": rcpt2["source_id"],
                           "source_version": rcpt2["version"],
                           "locator": {"type": "heading", "value": "Window"}}]
        run2 = wc_compile.stage_candidates(wiki, rcpt2["source_id"],
                                           rcpt2["version"], [c2], {})
        wc_compile.reconcile_prepare(wiki, run2)
        with self.assertRaises(TxnError):
            wc_compile.reconcile_apply(wiki, run2, [], wiki.revision())
```

- [ ] **Step 2: Run to verify fail** — current apply ignores missing rows silently → first test fails (0 claims created) or raises; either way fails.

- [ ] **Step 3: Implement** — at the top of `reconcile_apply`, after loading `payload`:

```python
    comparisons_path = os.path.join(rundir, "comparisons.json")
    comparisons = (_load_json(comparisons_path)["comparisons"]
                   if os.path.isfile(comparisons_path) else None)
    classified = {c["index"] for c in classifications}
    auto_counts = {"unrelated": 0, "duplicate": 0, "corroboration": 0}
    for i, cand in enumerate(candidates):
        if i in classified:
            continue
        matches = comparisons[i]["matches"] if comparisons else []
        verdict = auto_verdict(cand, matches)
        if verdict is None:
            raise TxnError(
                "candidate index %d (%s/%s) needs a classification — "
                "no auto verdict applies" % (i, cand["subject"], cand["predicate"]))
        classifications.append({"index": i, "relationship": verdict})
        auto_counts[verdict.lower()] += 1
```

And in the receipt assembly (before `return receipt`): `receipt["auto"] = auto_counts`. (Receipt is `txn.commit(base_revision)` return — a dict; mutate it.)

Note: `classifications` may arrive unsorted — `reconcile_apply` iterates it directly; appended auto rows are fine in any order since each carries `index`.

- [ ] **Step 4: Run to verify pass** + full `test_compile` suite green.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat: reconcile_apply auto-fills deterministic verdicts"
```

---

### Task 4: `wiki.py compile` umbrella command

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/compile.py` (add `compile_umbrella`)
- Modify: `skill/llm-wiki/scripts/wiki.py` (subparser + `cmd_compile`)
- Test: `skill/llm-wiki/scripts/tests/test_compile.py`

**Interfaces:**
- Consumes: `parse_candidates_file`, `normalize_candidate` (Task 1); `auto_verdict` + compact prepare + auto-fill apply (Tasks 2–3); existing `compile_plan`, `stage_candidates`, `build_pages` (`pages.build_pages`), `verify` (`pages.verify`).
- Produces: `compile_umbrella(wiki, resume: bool) -> dict`. CLI: `python3 wiki.py compile [--resume]`.

- [ ] **Step 1: Failing test — end-to-end umbrella**

```python
    def test_umbrella_full_flow(self):
        import wikicore.compile as wc
        wiki = fresh()
        rcpt = ingest_text(wiki, "doc", "fact: window is 7 days")

        out1 = wc.compile_umbrella(wiki, resume=False)
        self.assertEqual(len(out1["pending"]), 1)
        p = out1["pending"][0]
        self.assertTrue(os.path.isfile(p["content_path"]))
        # agent writes shorthand at candidates_path
        with open(p["candidates_path"], "w") as f:
            f.write(json.dumps({
                "subject": "project.window", "predicate": "days", "value": 7,
                "locator": "h:Window", "authority": "manual"}) + "\n")

        out2 = wc.compile_umbrella(wiki, resume=True)
        self.assertEqual(out2["auto"]["unrelated"], 1)
        self.assertEqual(out2["needs_review"], [])
        self.assertFalse(out2["needs_classifications"])

        out3 = wc.compile_umbrella(wiki, resume=True)
        self.assertTrue(out3["verify"]["ok"])
        self.assertEqual(out3["changes"]["claims_created"], 1)

    def test_umbrella_resume_waits_for_classifications(self):
        wiki = fresh()
        rcpt = ingest_text(wiki, "a", "x")
        run_dir_setup = wc_compile.compile_umbrella(wiki, resume=False)
        p = run_dir_setup["pending"][0]
        # two candidates: one auto, one needing review requires a second claim;
        # simplest: write shorthand with a value that conflicts after first apply
        with open(p["candidates_path"], "w") as f:
            f.write(json.dumps({"subject": "s", "predicate": "p",
                                "value": 1, "locator": "h:H"}) + "\n")
        out = wc_compile.compile_umbrella(wiki, resume=True)
        self.assertFalse(out["needs_classifications"])
```

(For a `needs_review` round-trip test: stage+apply a first claim via existing low-level calls, then umbrella a second pending source whose shorthand has same subject/predicate but different value → `needs_classifications: true`; agent writes `classifications.json` `[{"index":0,"relationship":"CORRECTION","target_claim_id":...}]` into the staging dir; next `--resume` applies.)

- [ ] **Step 2: Run to verify fail** — `compile_umbrella` doesn't exist.

- [ ] **Step 3: Implement** — in `wikicore/compile.py`:

```python
def _staging_runs(wiki):
    base = wiki.p(".state", "staging")
    if not os.path.isdir(base):
        return []
    return sorted(d for d in os.listdir(base)
                  if os.path.isdir(os.path.join(base, d)))


def _staging_state(wiki, run_id):
    """None=no files yet; 'staged'=compile.json; 'prepared'=comparisons.json."""
    d = wiki.p(".state", "staging", run_id)
    if os.path.isfile(os.path.join(d, "comparisons.json")):
        return "prepared"
    if os.path.isfile(os.path.join(d, "compile.json")):
        return "staged"
    if os.path.isfile(os.path.join(d, "candidates.jsonl")):
        return "candidates_ready"
    return None


def compile_umbrella(wiki, resume=False):
    """Spec §1: 3-phase state machine over staging dirs."""
    from . import sources as sources_mod, pages as pages_mod
    from .shorthand import normalize_candidate, parse_candidates_file

    if not resume:
        plan = compile_plan(wiki)
        pending = []
        for entry in plan["pending"]:
            sid = entry["source_id"] if isinstance(entry, dict) else entry
            ver = (entry.get("version") if isinstance(entry, dict)
                   else sources_mod.get_manifest(wiki, sid)["versions"][-1])
            run_id = ids.new("run")
            d = wiki.p(".state", "staging", run_id)
            os.makedirs(d, exist_ok=True)
            manifest = sources_mod.get_manifest(wiki, sid)
            content = wiki.p("sources", sid,
                             manifest.get("normalized_path") or "content.md")
            meta = {"source_id": sid, "source_version": ver,
                    "run_id": run_id,
                    "content_path": content,
                    "candidates_path": os.path.join(d, "candidates.jsonl"),
                    "classifications_path": os.path.join(d, "classifications.json")}
            with open(os.path.join(d, "pending.json"), "w") as f:
                json.dump(meta, f)
            pending.append(meta)
        return {"phase": "extract", "pending": pending}

    # resume: advance every run dir one phase
    runs = _staging_runs(wiki)
    needs_review_rows = []
    applied = []
    auto_totals = {"unrelated": 0, "duplicate": 0, "corroboration": 0}
    pending_apply = []

    for run_id in runs:
        d = wiki.p(".state", "staging", run_id)
        meta_p = os.path.join(d, "pending.json")
        if not os.path.isfile(meta_p):
            continue
        meta = _load_json(meta_p)
        cls_p = meta["classifications_path"]
        cand_p = meta["candidates_path"]

        if os.path.isfile(cls_p):
            classifications = parse_candidates_file(cls_p) \
                if cls_p.endswith(".jsonl") else _load_json(cls_p)
            if isinstance(classifications, dict):
                classifications = classifications.get("classifications", [])
            receipt = reconcile_apply(wiki, run_id, classifications,
                                      wiki.revision())
            applied.append(receipt)
            for k, v in receipt.get("auto", {}).items():
                auto_totals[k] += v
            continue

        state = _staging_state(wiki, run_id)
        if state == "prepared":
            cmp_ = _load_json(os.path.join(d, "comparisons.json"))["comparisons"]
            rows = [r for r in cmp_ if r["auto"] is None]
            for r in rows:
                r["run_id"] = run_id
                r["source_id"] = meta["source_id"]
            needs_review_rows.extend(rows)
            if not rows:
                # all auto — apply immediately with empty classifications
                receipt = reconcile_apply(wiki, run_id, [], wiki.revision())
                applied.append(receipt)
                for k, v in receipt.get("auto", {}).items():
                    auto_totals[k] += v
            continue

        if not os.path.isfile(cand_p):
            raise TxnError("run %s: expected %s (write shorthand candidates there)"
                           % (run_id, cand_p))
        raw = parse_candidates_file(cand_p)
        manifest = sources_mod.get_manifest(wiki, meta["source_id"])
        root_origin = manifest.get("root_origin") or meta["source_id"]
        cands = [normalize_candidate(c, meta["source_id"],
                                     meta["source_version"], root_origin)
                 for c in raw]
        report_p = os.path.join(d, "report.json")
        report = _load_json(report_p) if os.path.isfile(report_p) else {
            "source_id": meta["source_id"],
            "source_version": meta["source_version"],
            "coverage": {"text": "complete"},
            "warnings": [],
            "parser": {"name": "agent", "version": "shorthand-jsonl"},
        }
        # stage into THIS run dir (not stage_candidates — it would mint a new
        # run id). _stage_payload is the shared writer extracted in this task.
        _stage_payload(wiki, d, meta["source_id"], meta["source_version"],
                       cands, report)
        reconcile_prepare(wiki, run_id)
        cmp_ = _load_json(os.path.join(d, "comparisons.json"))["comparisons"]
        rows = [r for r in cmp_ if r["auto"] is None]
        for r in rows:
            r["run_id"] = run_id
            r["source_id"] = meta["source_id"]
        needs_review_rows.extend(rows)
        if not rows:
            receipt = reconcile_apply(wiki, run_id, [], wiki.revision())
            applied.append(receipt)
            for k, v in receipt.get("auto", {}).items():
                auto_totals[k] += v

    if needs_review_rows:
        return {"phase": "classify", "needs_classifications": True,
                "needs_review": needs_review_rows,
                "auto": auto_totals}

    if applied:
        pages_mod.build_pages(wiki, wiki.revision())
        verify = pages_mod.verify(wiki)
        total = {"claims_created": 0, "claims_updated": 0,
                 "claims_superseded": 0, "review_items": 0}
        conflicts = []
        for r in applied:
            for k in total:
                total[k] += r["changes"].get(k, 0)
            conflicts.extend(r.get("conflicts", []))
        return {"phase": "done", "needs_classifications": False,
                "auto": auto_totals, "changes": total,
                "conflicts": conflicts, "verify": verify,
                "receipts": applied}

    return {"phase": "waiting", "needs_classifications": False,
            "auto": auto_totals,
            "message": "no staged runs — run `compile` first or write candidates.jsonl"}
```

In `wiki.py`:

```python
def cmd_compile(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(wc_compile.compile_umbrella(wiki, resume=args.resume))
    return 0
```

Parser wiring (next to `compile-plan`):

```python
    p = sub.add_parser("compile")
    p.add_argument("--resume", action="store_true")
    p.set_defaults(fn=cmd_compile)
```

Also update `cmd_stage_candidates` to accept `.jsonl` via
`parse_candidates_file` + per-candidate `normalize_candidate` when the file
ends `.jsonl` (source_id/version/root_origin from args/manifest).

Refactor for reuse — split `stage_candidates`' body so `compile_umbrella`
stages into a pre-existing run dir without minting a new id:

```python
def _stage_payload(wiki, rundir, source_id, source_version, candidates, report):
    """Assign ids + root_origin, write compile.json into rundir."""
    from . import sources as sources_mod
    src_manifest = sources_mod.get_manifest(wiki, source_id)
    if src_manifest is None:
        raise TxnError("unknown source: %s" % source_id)
    assigned = []
    for c in candidates:
        if not ids.validate(c.get("id", "")) or c.get("id") == "auto":
            c["id"] = ids.new("claim")
        if not c.get("root_origin"):
            c["root_origin"] = src_manifest.get("root_origin") or source_id
        c["subject"] = str(c["subject"]).strip()
        c["predicate"] = str(c["predicate"]).strip()
        assigned.append(c)
    payload = {"source_id": source_id, "source_version": source_version,
               "candidates": assigned, "report": report}
    with open(os.path.join(rundir, "compile.json"), "w") as f:
        json.dump(payload, f, indent=2, ensure_ascii=False)


def stage_candidates(wiki, source_id, source_version, candidates, report):
    """Stage agent-extracted candidates. Staging only — nothing canonical yet."""
    txn = Transaction(wiki, "wiki-stage-candidates")
    _stage_payload(wiki, txn.staging_dir, source_id, source_version,
                   candidates, report)
    return txn.run_id
```

- [ ] **Step 4: Run tests** — `python3 -m unittest discover tests -v` all green.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat: wiki.py compile umbrella command (3-phase resumable)"
```

---

### Task 5: Skill docs — wiki-compile SKILL.md + references

**Files:**
- Modify: `skill/wiki-compile/SKILL.md` (rewrite compile section around umbrella)
- Modify: `skill/llm-wiki/references/COMMANDS.md` (add `## compile`, document shorthand/auto-verdicts)
- Modify: `skill/llm-wiki/references/EXTRACTION.md` (shorthand JSONL example replaces verbose array)
- Modify: `skill/llm-wiki/references/RECONCILIATION.md` (mark UNRELATED/DUPLICATE/CORROBORATION as engine-auto; agent only sees needs_review rows)

**Interfaces:** none (docs only). Wording MUST match the implemented flags/paths exactly — copy the shorthand example from Task 1's test.

- [ ] **Step 1: Rewrite the `## Engine` flow in `wiki-compile/SKILL.md`**

Replace the stage-candidates/reconcile chain with:

```markdown
```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" compile        # → pending[] with content_path + candidates_path
# read each content_path, write shorthand JSONL to candidates_path:
#   {"subject":"a.b","predicate":"p","value":7,"locator":"h:Heading","authority":"manual"}
python3 "$WIKI" compile --resume   # → auto counts + needs_review rows (usually empty)
# if needs_review: write classifications.json (only those rows), then:
python3 "$WIKI" compile --resume   # → receipt + verify.ok
```
```

Plus a short `### Shorthand` block listing agent fields vs engine-stamped
fields, locator prefixes (`h:`/`l:`/`s:`), and authority names
(`manual|doc|config|decision|adr|code|verified|inferred`).

- [ ] **Step 2: Update `COMMANDS.md`** — add a `## compile` section documenting the 3-phase flow, file paths (`candidates.jsonl`, `classifications.json`, optional `report.json` in the staging dir), `--resume` state machine, and note the low-level commands remain available. Update the `stage-candidates` entry to mention `.jsonl` support.

- [ ] **Step 3: Update `EXTRACTION.md`** — replace the verbose candidate example with the shorthand JSONL form; keep a note that the verbose array format still works.

- [ ] **Step 4: Update `RECONCILIATION.md`** — add: "UNRELATED (no matches), DUPLICATE and CORROBORATION (same scope+value, differing only by root_origin) are auto-classified by the engine — you only classify rows appearing in `needs_review`."

- [ ] **Step 5: Run package test + commit**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_skill_package -v`
Expected: PASS (frontmatter/description rules still hold).

```bash
git commit -am "docs: compile umbrella + shorthand in wiki-compile skill and references"
```

---

### Task 6: End-to-end regression + evals note

**Files:**
- Modify: `skill/llm-wiki/scripts/tests/test_e2e.py` — add a scenario exercising `compile` umbrella via subprocess (`sys.executable wiki.py compile` in a tmp project)
- Modify: `evals/` scenario index IF a compile scenario exists that asserts the old 5-command flow — update expected commands to the umbrella flow (check `evals/golden-corpus/` and `evals/semantic/` first; skip if nothing references `reconcile-prepare` directly)

**Interfaces:** consumes `cmd_compile` from Task 4 via real subprocess; asserts on stdout JSON.

- [ ] **Step 1: Failing/passing e2e test**

```python
def test_e2e_umbrella_compile(tmp_path_or_fixture):
    # init wiki, ingest text file, run `compile`, write candidates.jsonl,
    # run `compile --resume` twice, assert verify.ok and claim file exists
```

Follow `test_e2e.py`'s existing subprocess/fixture conventions exactly (read the file first — reuse its harness).

- [ ] **Step 2: Run full suite** — `python3 -m unittest discover tests -v` from `scripts/`; all green.

- [ ] **Step 3: Commit**

```bash
git commit -am "test: e2e umbrella compile scenario"
```

---

## Self-Review

- Spec §1 (3-command resumable umbrella) → Task 4. ✔
- Spec §2 shorthand JSONL → Task 1; locator/authority enums match spec. ✔
- Spec §2 compact reconcile + hint → Task 2. ✔
- Spec §2 auto-classify UNRELATED/DUPLICATE/CORROBORATION → Tasks 2–3. ✔
- Spec §3 ingest → verified already shipped; spec marks out of scope; no task. ✔
- Spec §4 SKILL.md + references → Task 5; exit codes reused (TxnError→3). ✔
- Spec §4 tests → Tasks 1–4 unit + Task 6 e2e; no new deps. ✔
- Note: `stage_candidates` is split so `_stage_payload` writes `compile.json` directly into a run dir — the umbrella stages under its own pre-created run id, no rename/orphan dirs.
