# Topic-Map Compile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a topic-discovery phase to `wiki.py compile` — `topics.jsonl` per run merged into a canonical `.llm-wiki/topics.json` registry that drives declared-kind page routing and single-claim page eligibility, plus `record-decision`, `topics`, and `set_topic_kind` commands.

**Architecture:** Claims remain canonical; `wiki/` pages remain derived. The agent writes `topics.jsonl` before `candidates.jsonl` during the extract phase; `reconcile_apply` merges rows into a schema-validated registry inside the same transaction. `page-targets` consults the registry before the legacy eligibility/routing rules.

**Spec:** `docs/superpowers/specs/2026-10-16-topic-map-compile-design.md` (commit `ad1987a`)

**Tech Stack:** Python 3.10+ stdlib, `jsonschema`, unittest (`scripts/tests/`, run with `python3 -m unittest` from `scripts/`).

## Global Constraints

- All code lives under `skill/llm-wiki/scripts/` (`wiki.py`, `wikicore/*.py`); tests under `skill/llm-wiki/scripts/tests/`.
- Canonical state changes ONLY inside `Transaction` (`txn.stage_write` / `txn.stage_state`); never write `.llm-wiki/` files directly.
- Topic kinds are SINGULAR in the registry (`concept|entity|procedure|question|change|decision|source`); page directories are PLURAL. Map via `topics.PAGE_DIR`.
- `disputed` claims are NOT in `synthesize.ELIGIBLE_STATUSES` (`accepted`, `provisional`) — never add them.
- `wiki/decisions/*`, `wiki/index.md`, `wiki/overview.md`, `*/index.md` are script-owned; `write_page` rejects them — never route a topic target there.
- Tests use the existing unittest pattern: `fresh()` → `init_wiki`, `ingest_text()`, `run_compile()`; `sys.path.insert` at top (see `tests/test_compile.py`).
- CLI errors raise `TxnError` (exit 3); stale revision raises `ConflictError` (exit 4).

---

### Task 1: `wikicore/topics.py` — registry module + move `slugify` to `text.py`

**Files:**
- Create: `skill/llm-wiki/scripts/wikicore/topics.py`
- Create: `skill/llm-wiki/schemas/topics.schema.json`
- Modify: `skill/llm-wiki/scripts/wikicore/text.py` (add `slugify`)
- Modify: `skill/llm-wiki/scripts/wikicore/synthesize.py:124-133` (replace body with re-export)
- Test: `skill/llm-wiki/scripts/tests/test_topics.py`

**Interfaces:**
- Consumes: `Transaction.stage_state`, `review.add_item`, `text.slugify`, `synthesize._load_kind_rules`/`_route_kind` (lazy, backfill only).
- Produces: `load(wiki)`, `get(wiki, subject)`, `get_kind(wiki, subject)`, `merge_rows(txn, rows, declared_by)`, `ensure_source_topic(txn, subject, title, declared_by)`, `auto_register(txn, subjects, declared_by)`, `unclaimed(wiki)`, `parse_topics_file(path)`, `backfill(wiki)`, constants `KINDS`, `PAGE_DIR`.

`slugify` must move (not copy) to `text.py` — `topics.py` and `synthesize.py` share it; `synthesize` keeps a re-export so existing callers/tests are unaffected (no second convention).

- [ ] **Step 1: Write the failing tests**

`skill/llm-wiki/scripts/tests/test_topics.py`:

```python
import json, os, sys, tempfile, unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import claims, sources, topics
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import Transaction, TxnError


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "topics-test")
    return wiki


def _commit(wiki, fn):
    txn = Transaction(wiki, "test")
    fn(txn)
    return txn.commit(wiki.revision())


class TestTopicRows(unittest.TestCase):
    def test_parse_rows_validates(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "topics.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"lake.s3","kind":"entity","title":"S3"}\n')
            f.write('{"subject":"lake.dl","kind":"concepts","title":"Data lake"}\n')
        rows, kinds = topics.parse_topics_file(p)
        self.assertEqual(rows[0]["kind"], "entity")
        self.assertEqual(rows[1]["kind"], "concept")  # plural normalized

    def test_parse_rows_rejects_bad_kind(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "t.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a.b","kind":"bogus","title":"T"}\n')
        with self.assertRaises(TxnError):
            topics.parse_topics_file(p)

    def test_parse_rows_rejects_missing_field_with_line(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "t.jsonl")
        with open(p, "w") as f:
            f.write('{"subject":"a.b","title":"no kind"}\n')
        with self.assertRaises(TxnError) as cm:
            topics.parse_topics_file(p)
        self.assertIn("line 1", str(cm.exception))


class TestMerge(unittest.TestCase):
    def test_same_kind_merges_aliases_first_title_wins(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3", "aliases": ["AWS S3"]},
            {"subject": "lake.s3", "kind": "entity", "title": "Amazon S3", "aliases": ["S3"]},
        ], {"source_id": "src_a", "version": 1}))
        e = topics.get(wiki, "lake.s3")
        self.assertEqual(e["title"], "S3")
        self.assertEqual(sorted(e["aliases"]), ["AWS S3", "S3"])

    def test_kind_conflict_keeps_first_kind(self):
        wiki = fresh()
        out = {}
        def go(t):
            out.update(topics.merge_rows(t, [
                {"subject": "lake.delta", "kind": "entity", "title": "Delta"},
                {"subject": "lake.delta", "kind": "concept", "title": "Delta"},
            ], {"source_id": "src_a", "version": 1}))
        _commit(wiki, go)
        e = topics.get(wiki, "lake.delta")
        self.assertEqual(e["kind"], "entity")
        self.assertEqual(e["status"], "conflict")
        self.assertEqual(e["alternate_kinds"], ["concept"])
        self.assertEqual(out["kind_conflicts"], ["lake.delta"])

    def test_auto_register_creates_concept(self):
        wiki = fresh()
        out = {}
        def go(t):
            out.update(topics.auto_register(t, ["misc.thing"],
                                            {"source_id": "src_a", "version": 1}))
        _commit(wiki, go)
        e = topics.get(wiki, "misc.thing")
        self.assertEqual(e["kind"], "concept")
        self.assertTrue(e["auto_registered"])
        self.assertEqual(out["auto_registered"], ["misc.thing"])

    def test_decision_kind_never_conflicts_with_merge(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "policy.pg", "kind": "decision", "title": "Use PG"},
        ], {"source_id": "src_a", "version": 1}))
        self.assertEqual(topics.get(wiki, "policy.pg")["kind"], "decision")


class TestRegistryFile(unittest.TestCase):
    def test_registry_json_shape_and_version(self):
        wiki = fresh()
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3"},
        ], {"source_id": "src_a", "version": 1}))
        reg = topics.load(wiki)
        self.assertEqual(reg["version"], 1)
        self.assertIn("lake.s3", reg["topics"])
        self.assertEqual(reg["topics"]["lake.s3"]["declared_by"],
                         [{"source_id": "src_a", "version": 1}])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics -v`
Expected: FAIL — `ModuleNotFoundError: wikicore.topics`

- [ ] **Step 3: Move `slugify` to `text.py` and write `topics.py`**

In `wikicore/text.py` append:

```python
def slugify(subject: str) -> str:
    """Subject -> slug: lowercase, non-alphanumeric runs -> '-', <=60 chars cut
    at a '-' boundary (PRD §17)."""
    slug = re.sub(r"[^a-z0-9]+", "-", subject.lower()).strip("-")
    if len(slug) > 60:
        cut = slug[:60]
        if "-" in cut:
            cut = cut[: cut.rfind("-")]
        slug = cut.strip("-")
    return slug or "untitled"
```

In `wikicore/synthesize.py` replace the `def slugify` body (lines 124–133) with a re-export:

```python
from .text import slugify  # noqa: F401  (canonical home: text.py)
```

Create `schemas/topics.schema.json`:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "TopicRegistry",
  "type": "object",
  "required": ["version", "topics"],
  "properties": {
    "version": {"type": "integer", "minimum": 0},
    "topics": {
      "type": "object",
      "additionalProperties": {
        "type": "object",
        "required": ["kind", "title", "status"],
        "properties": {
          "kind": {"enum": ["concept", "entity", "procedure", "question", "change", "decision", "source"]},
          "title": {"type": "string", "minLength": 1},
          "status": {"enum": ["resolved", "conflict"]},
          "aliases": {"type": "array", "items": {"type": "string"}},
          "alternate_kinds": {"type": "array", "items": {"type": "string"}},
          "auto_registered": {"type": "boolean"},
          "declared_by": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["source_id", "version"],
              "properties": {"source_id": {"type": "string"}, "version": {"type": "integer"}}
            }
          }
        }
      }
    }
  }
}
```

Create `wikicore/topics.py`:

```python
"""Canonical topic registry (.llm-wiki/topics.json).

Declared by the agent during the compile extract phase (topics.jsonl),
merged deterministically at apply time inside the claim transaction.
Drives declared-kind routing and single-claim page eligibility in
synthesize.page_targets. Never hand-edit.
"""
import json
import os

from .text import slugify  # noqa: F401  (re-exported for page-targets)
from .transaction import Transaction, TxnError

TOPICS_FILE = "topics.json"

KINDS = ("concept", "entity", "procedure", "question", "change",
         "decision", "source")
_PLURAL = {"concepts": "concept", "entities": "entity",
           "procedures": "procedure", "questions": "question",
           "changes": "change", "decisions": "decision",
           "sources": "source"}
PAGE_DIR = {"concept": "concepts", "entity": "entities",
            "procedure": "procedures", "question": "questions",
            "change": "changes", "source": "sources"}


def _norm_kind(k: str) -> str:
    k = str(k).strip().lower()
    if k in _PLURAL:
        return _PLURAL[k]
    return k


def _entry(kind, title, declared_by):
    return {"kind": kind, "title": title, "status": "resolved",
            "aliases": [], "alternate_kinds": [],
            "declared_by": [dict(declared_by)]}


def load(wiki) -> dict:
    if not os.path.isfile(wiki.p(TOPICS_FILE)):
        return {"version": 0, "topics": {}}
    with open(wiki.p(TOPICS_FILE)) as f:
        return json.load(f)


def get(wiki, subject):
    return load(wiki)["topics"].get(subject)


def get_kind(wiki, subject):
    e = get(wiki, subject)
    if e and e.get("status") == "resolved" and e.get("kind") in PAGE_DIR:
        return PAGE_DIR[e["kind"]]
    return None


def _load_for_txn(wiki, txn):
    """Registry incl. this txn's staged overlay."""
    data = txn._state_overlay.get(TOPICS_FILE)
    if data is None:
        data = load(wiki)
    return data


def _stage(txn, reg):
    txn.stage_state(TOPICS_FILE, lambda data: data.update(reg))


def _title_for(subject):
    return subject.rsplit(".", 1)[-1].replace("-", " ").title()


def _merge_one(reg, row, declared_by, out):
    subj = str(row["subject"]).strip()
    kind = _norm_kind(row["kind"])
    ent = reg["topics"].get(subj)
    if ent is None:
        ent = _entry(kind, str(row["title"]).strip(), declared_by)
        reg["topics"][subj] = ent
        out["registered"] += 1
        return
    db = dict(declared_by)
    if db not in ent.setdefault("declared_by", []):
        ent["declared_by"].append(db)
    if ent["kind"] != kind:
        if kind not in ent.setdefault("alternate_kinds", []):
            ent["alternate_kinds"].append(kind)
        ent["status"] = "conflict"
        if subj not in out["kind_conflicts"]:
            out["kind_conflicts"].append(subj)
        return
    for a in row.get("aliases") or []:
        a = str(a).strip()
        if a and a not in ent.setdefault("aliases", []):
            ent["aliases"].append(a)
    new_title = str(row.get("title") or "").strip()
    if new_title and new_title != ent["title"]:
        out["warnings"].append("topic %r: title %r kept over %r"
                               % (subj, ent["title"], new_title))


def merge_rows(txn: Transaction, rows: list, declared_by: dict) -> dict:
    """Merge declared topics.jsonl rows into the registry (staged)."""
    from . import review
    reg = _load_for_txn(txn.wiki, txn)
    reg.setdefault("topics", {})
    out = {"declared": len(rows), "registered": 0,
           "kind_conflicts": [], "warnings": []}
    for row in rows:
        subj = str(row["subject"]).strip()
        before = len(out["kind_conflicts"])
        _merge_one(reg, row, declared_by, out)
        if len(out["kind_conflicts"]) > before:
            review.add_item(
                txn, "topic_kind_conflict",
                "subject %r declared with conflicting kinds" % subj,
                [dict(declared_by)], [subj],
                "wrong kind routes the page to the wrong directory",
                proposed=reg["topics"][subj]["kind"])
    if rows:
        reg["version"] = int(reg.get("version", 0)) + 1
        _stage(txn, reg)
    return out


def ensure_source_topic(txn: Transaction, subject: str, title: str,
                        declared_by: dict) -> None:
    reg = _load_for_txn(txn.wiki, txn)
    reg.setdefault("topics", {})
    if subject not in reg["topics"]:
        ent = _entry("source", title, declared_by)
        ent["auto_registered"] = True
        reg["topics"][subject] = ent
        reg["version"] = int(reg.get("version", 0)) + 1
        _stage(txn, reg)


def auto_register(txn: Transaction, subjects: list, declared_by: dict) -> dict:
    """Register undeclared candidate subjects as kind=concept."""
    reg = _load_for_txn(txn.wiki, txn)
    reg.setdefault("topics", {})
    added = []
    for s in subjects:
        if s in reg["topics"]:
            continue
        ent = _entry("concept", _title_for(s), declared_by)
        ent["auto_registered"] = True
        reg["topics"][s] = ent
        added.append(s)
    if added:
        reg["version"] = int(reg.get("version", 0)) + 1
        _stage(txn, reg)
    return {"auto_registered": sorted(added)}


def unclaimed(wiki) -> list:
    """Declared registry subjects with zero claims."""
    from . import claims
    have = {c["subject"] for c in claims.list_claims(wiki)}
    return sorted(s for s, e in load(wiki)["topics"].items()
                  if e.get("kind") not in ("source", "decision")
                  and s not in have)


def parse_topics_file(path: str):
    """Parse topics.jsonl -> (rows, declared_subjects). TxnError w/ line nos."""
    rows = []
    declared = set()
    with open(path, encoding="utf-8") as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError as e:
                raise TxnError("topics.jsonl line %d: invalid JSON: %s" % (i, e))
            if not isinstance(row, dict):
                raise TxnError("topics.jsonl line %d: row must be an object" % i)
            for field in ("subject", "kind", "title"):
                if not str(row.get(field) or "").strip():
                    raise TxnError("topics.jsonl line %d: missing %r"
                                   % (i, field))
            k = _norm_kind(row["kind"])
            if k not in KINDS or k == "source":
                raise TxnError("topics.jsonl line %d: bad kind %r (valid: %s)"
                               % (i, row["kind"],
                                  ", ".join(x for x in KINDS if x != "source")))
            row["kind"] = k
            rows.append(row)
            declared.add(row["subject"].strip())
    return rows, declared


def backfill(wiki) -> dict:
    """Register every existing claim subject (idempotent; never overwrites)."""
    from . import claims, synthesize
    reg = load(wiki)
    reg.setdefault("topics", {})
    warnings = []
    rules, default_kind = synthesize._load_kind_rules(wiki, warnings)
    groups = {}
    for c in claims.list_claims(wiki):
        groups.setdefault(c["subject"], []).append(c)
    added = 0
    declared_by = {"source_id": "backfill", "version": 0}
    for subj in sorted(groups):
        if subj in reg["topics"]:
            continue
        plural, _ = synthesize._route_kind(subj, groups[subj], rules,
                                           default_kind)
        k = _PLURAL.get(plural, "concept")
        ent = _entry(k, _title_for(subj), declared_by)
        ent["auto_registered"] = True
        reg["topics"][subj] = ent
        added += 1
    if added:
        reg["version"] = int(reg.get("version", 0)) + 1
        txn = Transaction(wiki, "wiki-topics-backfill")
        txn.stage_state(TOPICS_FILE, lambda data: data.update(reg))
        txn.changes = {"topics_registered": added}
        txn.commit(wiki.revision())
    return {"topics_registered": added, "warnings": warnings}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics -v`
Expected: PASS (all tests)

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/topics.py skill/llm-wiki/scripts/wikicore/text.py \
  skill/llm-wiki/scripts/wikicore/synthesize.py skill/llm-wiki/schemas/topics.schema.json \
  skill/llm-wiki/scripts/tests/test_topics.py
git commit -m "feat(topics): canonical topic registry with merge, auto-register, backfill"
```

---

### Task 2: `deps.mark_stale` helper

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/deps.py` (append)
- Test: `skill/llm-wiki/scripts/tests/test_deps.py`

**Interfaces:**
- Consumes: `Transaction.stage_state`.
- Produces: `mark_stale(txn, artifacts)` — used by `set_topic_kind` (Task 5).

- [ ] **Step 1: Write the failing test**

Append to `tests/test_deps.py`:

```python
def test_mark_stale_adds_artifacts(self):
    wiki = fresh()
    txn = Transaction(wiki, "test")
    deps.mark_stale(txn, ["wiki/concepts/old-dir.md"])
    txn.commit(wiki.revision())
    self.assertTrue(deps.is_stale(wiki, "wiki/concepts/old-dir.md"))
```

(Use the file's existing imports/`fresh` helper; if `test_deps.py` lacks them, copy the `fresh()`/`sys.path` preamble from `tests/test_topics.py` and import `Transaction`.)

- [ ] **Step 2: Run to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_deps -v`
Expected: FAIL — `AttributeError: module 'wikicore.deps' has no attribute 'mark_stale'`

- [ ] **Step 3: Implement**

Append to `wikicore/deps.py`:

```python
def mark_stale(txn: Transaction, artifacts: list) -> None:
    """Mark specific derived artifacts stale without dependency lookup."""
    if not artifacts:
        return
    def _upd(data):
        stale = data.setdefault("stale", [])
        for a in artifacts:
            if a not in stale:
                stale.append(a)
    txn.stage_state(DEPS_FILE, _upd)
```

- [ ] **Step 4: Run to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_deps -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/deps.py skill/llm-wiki/scripts/tests/test_deps.py
git commit -m "feat(deps): mark_stale helper for explicit artifact invalidation"
```

---

### Task 3: `compile` — `topics_path`, parse+validate, merge at apply, receipt block

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/compile.py:106-109,232-262,341-365,394-548`
- Test: `skill/llm-wiki/scripts/tests/test_topics.py` (append `TestCompileTopics`)

**Interfaces:**
- Consumes: `topics.parse_topics_file`, `topics.merge_rows`, `topics.ensure_source_topic`, `topics.auto_register`, `topics.unclaimed` (Task 1).
- Produces: `reconcile_apply(wiki, run_id, classifications, base_revision, topics_rows=None, declared_by=None)`; receipt gains `receipt["topics"]` dict; `pending[]` entries gain `topics_path`; umbrella done-receipt aggregates `topics` totals + `unclaimed` + `no_topics_declared` warning.

Key placement: merge runs INSIDE `reconcile_apply` after `txn = Transaction(...)` (line 261) so registry + claims commit atomically.

- [ ] **Step 1: Write the failing tests**

Append to `tests/test_topics.py` (add `from wikicore import compile as wc_compile` to imports):

```python
class TestCompileTopics(unittest.TestCase):
    def _run_umbrella(self, wiki, topics_rows=None, cands=None):
        """Full umbrella: compile -> write files -> resume."""
        r = sources.ingest(wiki, "text", "doc-t", b"# Doc\nS3 is object storage.")
        plan = wc_compile.compile_umbrella(wiki)
        meta = plan["pending"][0]
        if topics_rows is not None:
            with open(meta["topics_path"], "w") as f:
                for row in topics_rows:
                    f.write(json.dumps(row) + "\n")
        with open(meta["candidates_path"], "w") as f:
            for c in (cands or []):
                f.write(json.dumps(c) + "\n")
        res = wc_compile.compile_umbrella(wiki, resume=True)
        while res["phase"] == "classify":
            cls = [{"index": row["index"], "relationship": "CONTRADICTION",
                    "target_claim_id": None, "authority": {"type": "authoritative_source"}}
                   for row in res["needs_review"]]
            with open(meta["classifications_path"], "w") as f:
                json.dump(cls, f)
            res = wc_compile.compile_umbrella(wiki, resume=True)
        return r, res

    def test_pending_entry_has_topics_path(self):
        wiki = fresh()
        sources.ingest(wiki, "text", "doc-t", b"hello")
        plan = wc_compile.compile_umbrella(wiki)
        self.assertIn("topics_path", plan["pending"][0])
        self.assertTrue(plan["pending"][0]["topics_path"].endswith("topics.jsonl"))

    def test_declared_topics_registered_and_in_receipt(self):
        wiki = fresh()
        cand = {"subject": "lake.s3", "predicate": "type", "value": "object store",
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "lake.s3", "kind": "entity", "title": "S3"}],
            cands=[cand])
        t = topics.get(wiki, "lake.s3")
        self.assertEqual(t["kind"], "entity")
        self.assertEqual(res["topics"]["registered"], 1)

    def test_source_topic_auto_registered(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki, topics_rows=[], cands=[])
        reg = topics.load(wiki)["topics"]
        self.assertTrue(any(e["kind"] == "source" for e in reg.values()))

    def test_undeclared_subject_auto_registered_with_warning(self):
        wiki = fresh()
        cand = {"subject": "misc.thing", "predicate": "p", "value": 1,
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki, topics_rows=[], cands=[cand])
        e = topics.get(wiki, "misc.thing")
        self.assertTrue(e["auto_registered"])
        self.assertIn("misc.thing", res["topics"]["auto_registered"])

    def test_missing_topics_file_warns_not_fails(self):
        wiki = fresh()
        cand = {"subject": "x.y", "predicate": "p", "value": 1,
                "locator": "h:Doc", "authority": "doc"}
        _, res = self._run_umbrella(wiki, topics_rows=None, cands=[cand])
        self.assertEqual(res["phase"], "done")
        self.assertTrue(any("no_topics_declared" in w
                            for w in res["topics"]["warnings"]))

    def test_unclaimed_topic_listed(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "lake.presto", "kind": "entity", "title": "P"}],
            cands=[])
        self.assertIn("lake.presto", res["topics"]["unclaimed"])

    def test_decision_topic_in_receipt_not_page(self):
        wiki = fresh()
        _, res = self._run_umbrella(wiki,
            topics_rows=[{"subject": "policy.pg", "kind": "decision", "title": "PG"}],
            cands=[])
        self.assertIn("policy.pg", res["topics"]["decision_topics"])
        self.assertEqual(topics.get(wiki, "policy.pg")["kind"], "decision")
```

- [ ] **Step 2: Run to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics.TestCompileTopics -v`
Expected: FAIL — `KeyError: 'topics_path'`

- [ ] **Step 3: Implement**

In `wikicore/compile.py`, three edits:

**(a)** `compile_umbrella` phase-1 `meta` dict (line ~431): add

```python
                "topics_path": os.path.join(d, "topics.jsonl"),
```

(between `content_path` and `candidates_path`).

**(b)** `reconcile_apply` signature + merge. Change signature to:

```python
def reconcile_apply(wiki, run_id: str, classifications: list, base_revision: int,
                    topics_rows=None, declared_by=None) -> dict:
```

After `txn = Transaction(wiki, "wiki-compile")` (line 261) insert:

```python
    from . import topics as topics_mod

    topic_out = {"declared": 0, "registered": 0, "auto_registered": [],
                 "kind_conflicts": [], "decision_topics": [], "warnings": []}
    if declared_by:
        if topics_rows:
            topic_out.update(topics_mod.merge_rows(txn, topics_rows, declared_by))
        topics_mod.ensure_source_topic(
            txn, "source.%s" % re.sub(r"[^a-z0-9]+", "-", str(
                declared_by.get("title") or source_id).lower()).strip("-") or "source",
            str(declared_by.get("title") or source_id), declared_by)
        topic_out["decision_topics"] = sorted(
            str(r["subject"]).strip() for r in (topics_rows or [])
            if topics_mod._norm_kind(r["kind"]) == "decision")
```

Wait — simpler: compute decision subjects from declared rows directly. After the claim-classification loop and before `txn.stage_write("sources/%s/extraction.json" ...)` insert:

```python
    if declared_by:
        declared_subjects = {str(r["subject"]).strip() for r in (topics_rows or [])}
        undeclared = sorted({c["subject"].strip() for c in candidates} - declared_subjects)
        reg = topics_mod.load(wiki)
        reg.setdefault("topics", {})
        overlay = txn._state_overlay.get("topics.json", reg)
        undeclared = [s for s in undeclared if s not in overlay["topics"]]
        if undeclared:
            topic_out.update(topics_mod.auto_register(txn, undeclared, declared_by))
            for s in undeclared:
                topic_out["warnings"].append(
                    "subject %r not declared in topics.jsonl; auto-registered as concept" % s)
```

After `receipt = txn.commit(base_revision)` add `receipt["topics"] = topic_out` (before `receipt["auto"]` assignment is fine — attach both after commit; order irrelevant).

Add `import re` at top of file if absent (check — file already imports json/os; add `import re` to the import block).

**(c)** `compile_umbrella` resume path. After `meta = _load_json(meta_p)` (line ~462) add topic parsing before the apply call sites. Restructure: right after `state = _staging_state(wiki, run_id)` insert:

```python
        topics_p = os.path.join(d, "topics.jsonl")
        topics_rows = None
        topics_missing = False
        if os.path.isfile(topics_p):
            from . import topics as topics_mod
            topics_rows, _declared = topics_mod.parse_topics_file(topics_p)
        else:
            topics_missing = True
```

Then change `_apply` (nested at line ~448) signature to `_apply(run_id, classifications, topics_rows=None, topics_missing=False)` and inside it:

```python
    def _apply(run_id, classifications, topics_rows=None, topics_missing=False):
        meta2 = _load_json(os.path.join(
            wiki.p(".state", "staging", run_id), "pending.json"))
        manifest = sources_mod.get_manifest(wiki, meta2["source_id"]) or {}
        declared_by = {"source_id": meta2["source_id"],
                       "version": meta2["source_version"],
                       "title": manifest.get("title") or meta2["source_id"]}
        receipt = reconcile_apply(wiki, run_id, classifications,
                                  wiki.revision(),
                                  topics_rows=topics_rows,
                                  declared_by=declared_by)
        if topics_missing:
            receipt.setdefault("topics", {}).setdefault("warnings", []).append(
                "no_topics_declared (%s)" % run_id)
        receipt["topics"]["unclaimed"] = topics_mod.unclaimed(wiki)
        ...  # existing receipt.json write + applied.append + auto_totals loop
```

Update both `_apply` call sites to pass `topics_rows, topics_missing`. The replay path (cached `receipt.json`, lines ~471-480) stays as-is — receipts already carry `topics`.

Finally, in the done-receipt aggregation (lines ~530-544) add after the `conflicts` collection loop:

```python
        topics_total = {"declared": 0, "registered": 0, "auto_registered": [],
                        "kind_conflicts": [], "decision_topics": [],
                        "unclaimed": [], "warnings": []}
        for r in applied:
            t = r.get("topics") or {}
            topics_total["declared"] += t.get("declared", 0)
            topics_total["registered"] += t.get("registered", 0)
            for k in ("auto_registered", "kind_conflicts", "decision_topics",
                      "unclaimed", "warnings"):
                for v in t.get(k) or []:
                    if v not in topics_total[k]:
                        topics_total[k].append(v)
```

and add `"topics": topics_total` to the returned `{"phase": "done", ...}` dict.

- [ ] **Step 4: Run to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics tests.test_compile -v`
Expected: PASS (new + existing compile tests)

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/compile.py skill/llm-wiki/scripts/tests/test_topics.py
git commit -m "feat(compile): topics.jsonl extract phase, registry merge at apply, topics receipt"
```

---

### Task 4: `page-targets` — declared-kind routing + single-claim eligibility

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/synthesize.py:169-246`
- Test: `skill/llm-wiki/scripts/tests/test_topics.py` (append `TestPageTargets`)

**Interfaces:**
- Consumes: `topics.get`, `topics.PAGE_DIR` (Task 1).
- Produces: `page_targets` target dicts may carry `reason: "declared"`; `waiting[]` entries may carry `reason: "declared_no_claims"`.

- [ ] **Step 1: Write the failing tests**

```python
class TestPageTargets(unittest.TestCase):
    def _claim(self, wiki, r, subject):
        """Stage+apply one accepted claim via direct txn."""
        cand = {"id": "auto", "subject": subject, "predicate": "p", "value": 1,
                "scope": {}, "valid_from": None, "valid_to": None,
                "recorded_at": "2026-10-01T00:00:00Z", "status": "candidate",
                "proposed_by": "agent",
                "authority": {"type": "authoritative_source", "source": "doc"},
                "evidence": [{"source_id": r["source_id"],
                              "source_version": r["version"],
                              "locator": {"type": "heading", "value": "H"}}],
                "supersedes": []}
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"],
                                             [cand], {"source_id": r["source_id"],
                                                      "source_version": r["version"],
                                                      "coverage": {"text": "complete"},
                                                      "warnings": [],
                                                      "parser": {"name": "t", "version": "1"}})
        wc_compile.reconcile_prepare(wiki, run_id)
        wc_compile.reconcile_apply(wiki, run_id,
            [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
              "valid_from": None, "valid_to": None,
              "authority": {"type": "authoritative_source"}}], wiki.revision())

    def test_declared_single_claim_target_with_declared_kind(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"S3 stores objects.")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.s3", "kind": "entity", "title": "S3"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        self._claim(wiki, r, "lake.s3")
        res = synthesize.page_targets(wiki)
        tg = [t for t in res["targets"] if t["subject"] == "lake.s3"]
        self.assertEqual(len(tg), 1)
        self.assertEqual(tg[0]["artifact"], "wiki/entities/lake-s3.md")
        self.assertEqual(tg[0]["reason"], "declared")

    def test_undeclared_single_claim_stays_waiting(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        self._claim(wiki, r, "misc.thing")  # auto-registered NOT applied here
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "misc.thing" for t in res["targets"]))

    def test_conflict_subject_falls_back_to_rules(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.d", "kind": "entity", "title": "D"},
            {"subject": "lake.d", "kind": "concept", "title": "D"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        self._claim(wiki, r, "lake.d")
        res = synthesize.page_targets(wiki)
        self.assertFalse(any(t["subject"] == "lake.d" for t in res["targets"]))
        self.assertTrue(any(w["subject"] == "lake.d" for w in res["waiting"]))
```

(Add `from wikicore import synthesize` to test imports.)

- [ ] **Step 2: Run to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics.TestPageTargets -v`
Expected: FAIL — no targets emitted / artifact not entities dir

- [ ] **Step 3: Implement**

In `synthesize.page_targets`, inside the `for subject in sorted(groups)` loop, before the existing eligibility check, load the registry once above the loop:

```python
    topic_map = topics.load(wiki)["topics"]
```

(import `topics` at module top: `from . import claims, deps, topics`).

Then replace the eligibility block (lines 186-203) with:

```python
        n = len(cs)
        origins = sum(claims.independence(wiki, c) for c in cs)
        tentry = topic_map.get(subject)
        declared_kind = (topics.PAGE_DIR.get(tentry["kind"])
                         if tentry and tentry.get("status") == "resolved"
                         else None)
        if declared_kind is not None:
            reason = "declared"
        elif n >= min_claims and origins >= min_sources:
            reason = "corroborated"
        elif any((c.get("authority") or {}).get("type") == AUTHORITATIVE for c in cs):
            reason = "authoritative"
        else:
            waiting.append({
                "subject": subject,
                "claim_count": n,
                "distinct_origins": origins,
                "reason": "below_threshold",
            })
            continue
        if kind is not None:
            k, matched = kind, None
        elif declared_kind is not None:
            k, matched = declared_kind, None
        else:
            k, matched = _route_kind(subject, cs, rules, default_kind)
```

Additionally, `decision`-kind topics must never emit targets even if a claim group exists — `declared_kind` is `None` for `decision` (not in `PAGE_DIR`), which already yields that behavior; no extra code needed.

- [ ] **Step 4: Run to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics tests.test_synthesize -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/synthesize.py skill/llm-wiki/scripts/tests/test_topics.py
git commit -m "feat(synthesize): declared-kind routing and single-claim page eligibility"
```

---

### Task 5: `record-decision` command + `decision` input validation

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/claims.py` (append `create_decision`)
- Modify: `skill/llm-wiki/scripts/wiki.py` (parser + `cmd_record_decision`)
- Modify: `skill/llm-wiki/scripts/wikicore/transaction.py:250-263` (schema-check `topics.json`)
- Test: `skill/llm-wiki/scripts/tests/test_topics.py` (append `TestRecordDecision`)

**Interfaces:**
- Consumes: `ids.new("decision")`, `claims.load_claim`, `sources.get_manifest`, `DECISION_SCHEMA`.
- Produces: `claims.create_decision(wiki, data)` → decision dict (commits internally, returns `{"id", "decision"}`); CLI `wiki.py record-decision --file decision.json`.

- [ ] **Step 1: Write the failing tests**

```python
class TestRecordDecision(unittest.TestCase):
    def _accepted_claim(self, wiki, r):
        cand = {"id": "auto", "subject": "policy.pg", "predicate": "db", "value": "postgres",
                "scope": {}, "valid_from": None, "valid_to": None,
                "recorded_at": "2026-10-01T00:00:00Z", "status": "candidate",
                "proposed_by": "agent",
                "authority": {"type": "explicit_project_decision", "source": "ADR"},
                "evidence": [{"source_id": r["source_id"], "source_version": r["version"],
                              "locator": {"type": "heading", "value": "H"}}],
                "supersedes": []}
        run_id = wc_compile.stage_candidates(wiki, r["source_id"], r["version"], [cand],
            {"source_id": r["source_id"], "source_version": r["version"],
             "coverage": {"text": "complete"}, "warnings": [],
             "parser": {"name": "t", "version": "1"}})
        wc_compile.reconcile_prepare(wiki, run_id)
        wc_compile.reconcile_apply(wiki, run_id,
            [{"index": 0, "relationship": "UNRELATED", "target_claim_id": None,
              "valid_from": None, "valid_to": None,
              "authority": {"type": "explicit_project_decision"}}], wiki.revision())
        return claims.list_claims(wiki)[0]["id"]

    def test_record_decision_happy_path(self):
        wiki = fresh()
        r = sources.ingest(wiki, "text", "adr", b"use postgres")
        cid = self._accepted_claim(wiki, r)
        out = claims.create_decision(wiki, {
            "title": "Use Postgres", "status": "accepted",
            "decided_on": "2026-10-01", "claims": [cid],
            "evidence": [{"source_id": r["source_id"], "source_version": r["version"],
                          "locator": {"type": "heading", "value": "H"}}],
            "body": "ADR-001"})
        self.assertTrue(out["id"].startswith("decision_"))
        d = claims.load_decision(wiki, out["id"])
        self.assertEqual(d["claims"], [cid])
        self.assertEqual(d["version"], 1)

    def test_record_decision_rejects_unknown_claim(self):
        wiki = fresh()
        with self.assertRaises(TxnError) as cm:
            claims.create_decision(wiki, {
                "title": "X", "status": "accepted", "decided_on": "2026-10-01",
                "claims": ["claim_0000000000000000000000000A"], "evidence": []})
        self.assertIn("unknown claim", str(cm.exception))

    def test_record_decision_rejects_stamped_id(self):
        wiki = fresh()
        with self.assertRaises(TxnError):
            claims.create_decision(wiki, {
                "id": "decision_0000000000000000000000000A", "title": "X",
                "status": "accepted", "decided_on": "2026-10-01",
                "claims": [], "evidence": []})
```

- [ ] **Step 2: Run to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics.TestRecordDecision -v`
Expected: FAIL — `AttributeError: ... no attribute 'create_decision'`

- [ ] **Step 3: Implement**

Append to `wikicore/claims.py`:

```python
def create_decision(wiki, data: dict) -> dict:
    """Validate + commit a decision record (record-decision command).
    Caller supplies title/status/decided_on/claims/evidence/body/scope;
    engine stamps id, recorded_at, version."""
    if not isinstance(data, dict):
        raise TxnError("decision input must be a JSON object")
    for banned in ("id", "recorded_at", "version"):
        if banned in data:
            raise TxnError("decision input must not include %r — engine-stamped" % banned)
    for cid in data.get("claims") or []:
        c = load_claim(wiki, cid)
        if c.get("status") in TERMINAL:
            raise TxnError("claim %s is %s; cannot support a decision" % (cid, c["status"]))
    from . import sources as sources_mod
    for ev in data.get("evidence") or []:
        sid = ev.get("source_id", "")
        if sources_mod.get_manifest(wiki, sid) is None:
            raise TxnError("unknown source: %s" % sid)
    decision = dict(data)
    decision["id"] = ids.new("decision")
    decision["recorded_at"] = _now()
    decision["version"] = 1
    txn = Transaction(wiki, "wiki-record-decision")
    save_decision(txn, decision)
    txn.changes = {"decisions_created": 1}
    receipt = txn.commit(wiki.revision())
    return {"id": decision["id"], "decision": decision, "receipt": receipt}
```

In `transaction.py::_check_object`, add before the `schema_file = None` fallback:

```python
    elif rel == "topics.json":
        schema_file = "topics.schema.json"
```

In `wiki.py` add command:

```python
def cmd_record_decision(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    data = _load_json_file(args.file)
    _emit(claims.create_decision(wiki, data))
    return 0
```

and parser (after `record-decision` placement near `review`):

```python
    p = sub.add_parser("record-decision")
    p.add_argument("--file", required=True, help="decision input JSON file")
    p.set_defaults(fn=cmd_record_decision)
```

- [ ] **Step 4: Run to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/claims.py skill/llm-wiki/scripts/wikicore/transaction.py \
  skill/llm-wiki/scripts/wiki.py skill/llm-wiki/scripts/tests/test_topics.py
git commit -m "feat(decisions): record-decision command mints canonical decision records"
```

---

### Task 6: `topics` command + `set_topic_kind` review action + doctor

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/review.py:15-18,63-161`
- Modify: `skill/llm-wiki/scripts/wikicore/doctor.py` (append checks inside `run`/`doctor` fn)
- Modify: `skill/llm-wiki/scripts/wiki.py` (`cmd_topics` + parser)
- Test: `skill/llm-wiki/scripts/tests/test_topics.py` (append `TestReviewAndDoctor`)

**Interfaces:**
- Consumes: `topics.get`/`load`, `deps.mark_stale` (Task 2), `synthesize.slugify` via `text.slugify`.
- Produces: `review act --action set_topic_kind --params '{"subject":..., "kind":...}'`; `wiki.py topics`, `wiki.py topics --backfill`; doctor findings `topic_kind_conflict` (warn), `topic_unclaimed` (info), `topic_page_kind_mismatch` (error), `topics_auto_registered` (info).

- [ ] **Step 1: Write the failing tests**

```python
class TestReviewAndDoctor(unittest.TestCase):
    def _conflict(self, wiki):
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.d", "kind": "entity", "title": "D"},
            {"subject": "lake.d", "kind": "concept", "title": "D"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        return [i for i in wiki.read_jsonl(".state/review-queue.jsonl")
                if i.get("kind") == "topic_kind_conflict"][0]

    def test_set_topic_kind_resolves_conflict(self):
        from wikicore import review
        wiki = fresh()
        item = self._conflict(wiki)
        receipt = review.act(wiki, item["id"], "set_topic_kind",
                             {"subject": "lake.d", "kind": "concept"},
                             wiki.revision())
        e = topics.get(wiki, "lake.d")
        self.assertEqual(e["kind"], "concept")
        self.assertEqual(e["status"], "resolved")
        self.assertEqual(receipt["item"]["status"], "resolved")

    def test_set_topic_kind_rejects_resolved_subject(self):
        from wikicore import review
        wiki = fresh()
        r = sources.ingest(wiki, "text", "doc", b"x")
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.e", "kind": "entity", "title": "E"},
        ], {"source_id": r["source_id"], "version": r["version"]}))
        # need a review item to act on — use the conflict path then re-resolve
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.e", "kind": "concept", "title": "E"},
        ], {"source_id": "src2", "version": 1}))
        item = [i for i in wiki.read_jsonl(".state/review-queue.jsonl")
                if i.get("kind") == "topic_kind_conflict"][0]
        review.act(wiki, item["id"], "set_topic_kind",
                   {"subject": "lake.e", "kind": "concept"}, wiki.revision())
        # acting again on same item now that status=resolved must fail
        with self.assertRaises(TxnError):
            review.act(wiki, item["id"], "set_topic_kind",
                       {"subject": "lake.e", "kind": "entity"}, wiki.revision())

    def test_doctor_flags_conflict_and_unclaimed(self):
        from wikicore import doctor
        wiki = fresh()
        self._conflict(wiki)
        _commit(wiki, lambda t: topics.merge_rows(t, [
            {"subject": "lake.lonely", "kind": "entity", "title": "L"},
        ], {"source_id": "srcx", "version": 1}))
        res = doctor.run(wiki) if hasattr(doctor, "run") else doctor.doctor(wiki)
        codes = {f["code"] for f in res["findings"]}
        self.assertIn("topic_kind_conflict", codes)
        self.assertIn("topic_unclaimed", codes)
```

- [ ] **Step 2: Run to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics.TestReviewAndDoctor -v`
Expected: FAIL — `TxnError: unknown review action: 'set_topic_kind'`

- [ ] **Step 3: Implement**

In `review.py`: add `"set_topic_kind"` to `ACTIONS`. Restructure the `claim_id` guard (lines 71-75) to exempt it:

```python
    if action not in ("defer", "reopen", "set_topic_kind"):
        cid = params.get("claim_id")
        if not cid:
            raise TxnError("action %r requires claim_id" % action)
        claim = claims.load_claim(wiki, cid)
```

Add branch after `set_validity` (before `defer`):

```python
    elif action == "set_topic_kind":
        from . import deps as deps_mod, topics as topics_mod
        subj = str(params.get("subject") or "").strip()
        new_kind = str(params.get("kind") or "").strip().lower()
        entry = topics_mod.get(wiki, subj)
        if entry is None or entry.get("status") != "conflict":
            raise TxnError("subject %r is not a declared topic in conflict" % subj)
        if new_kind not in entry.get("alternate_kinds", []) + [entry["kind"]]:
            raise TxnError("kind %r was not declared for %r (declared: %s)"
                           % (new_kind, subj, ", ".join(entry.get("alternate_kinds", []))))
        old_dir = topics_mod.PAGE_DIR.get(entry["kind"])
        def _upd(reg):
            e = reg.setdefault("topics", {}).get(subj)
            if e is not None:
                e["kind"] = new_kind
                e["status"] = "resolved"
        txn.stage_state(topics_mod.TOPICS_FILE, _upd)
        from .text import slugify
        old_artifact = "wiki/%s/%s.md" % (old_dir, slugify(subj)) if old_dir else None
        if old_artifact and old_dir != topics_mod.PAGE_DIR.get(new_kind) \
                and os.path.isfile(wiki.p(old_artifact)):
            deps_mod.mark_stale(txn, [old_artifact])
```

In `doctor.py`: locate the function returning `{"ok": ..., "findings": [...]}` (main entry). Inside its semantic-debt section (near the `low extraction coverage`/`disputes` checks, after `known_review_ids` block), add:

```python
    # --- topic registry debt ----------------------------------------------
    try:
        from . import topics as topics_mod
        treg = topics_mod.load(wiki)["topics"]
    except Exception:
        treg = {}
    conflicts_n = sum(1 for e in treg.values() if e.get("status") == "conflict")
    if conflicts_n:
        add("warn", "topic_kind_conflict",
            "%d topic(s) have unresolved kind conflicts" % conflicts_n,
            "resolve via review act set_topic_kind")
    unclaimed = topics_mod.unclaimed(wiki) if treg else []
    if unclaimed:
        add("info", "topic_unclaimed",
            "%d declared topic(s) have no claims: %s" % (len(unclaimed), ", ".join(unclaimed[:5])),
            "extract claims for them or accept thin coverage")
    auto_n = sum(1 for e in treg.values() if e.get("auto_registered"))
    if auto_n:
        add("info", "topics_auto_registered",
            "%d topic(s) auto-registered as concept" % auto_n,
            "declare kinds explicitly in topics.jsonl")
    from .text import slugify
    for subj, e in treg.items():
        want = topics_mod.PAGE_DIR.get(e.get("kind"))
        if not want or e.get("status") != "resolved":
            continue
        slug = slugify(subj)
        wikip = wiki.p("wiki")
        if not os.path.isdir(wikip):
            continue
        for other in os.listdir(wikip):
            other_p = os.path.join(wikip, other, slug + ".md")
            if other != want and os.path.isfile(other_p) and \
                    other in synthesize.PAGE_KINDS:
                add("error", "topic_page_kind_mismatch",
                    "topic %r is %s but page exists at wiki/%s/%s.md" % (subj, e["kind"], other, slug),
                    "re-write page at wiki/%s/%s.md or fix the topic kind" % (want, slug))
```

(Ensure `import os` present — it is. `add` and `synthesize` names follow the module's existing conventions; `synthesize` is already imported in doctor.py — verify, add `from . import synthesize` if absent.)

In `wiki.py` add:

```python
def cmd_topics(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    from wikicore import topics as topics_mod
    if args.backfill:
        _emit(topics_mod.backfill(wiki))
        return 0
    reg = topics_mod.load(wiki)
    claim_counts = {}
    for c in claims.list_claims(wiki):
        claim_counts[c["subject"]] = claim_counts.get(c["subject"], 0) + 1
    out = [{"subject": s, "kind": e["kind"], "status": e["status"],
            "title": e["title"], "claim_count": claim_counts.get(s, 0),
            "declared_by": len(e.get("declared_by", [])),
            "auto_registered": bool(e.get("auto_registered"))}
           for s, e in sorted(reg["topics"].items())]
    _emit({"version": reg.get("version", 0), "topics": out})
    return 0
```

parser:

```python
    p = sub.add_parser("topics")
    p.add_argument("--backfill", action="store_true",
                   help="register existing claim subjects (idempotent)")
    p.set_defaults(fn=cmd_topics)
```

- [ ] **Step 4: Run to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_topics tests.test_review tests.test_doctor -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/review.py skill/llm-wiki/scripts/wikicore/doctor.py \
  skill/llm-wiki/scripts/wiki.py skill/llm-wiki/scripts/tests/test_topics.py
git commit -m "feat(topics): topics census, set_topic_kind review action, doctor findings"
```

---

### Task 7: Skill + reference docs, README, full suite, smoke run

**Files:**
- Modify: `skill/wiki-compile/SKILL.md`
- Modify: `skill/llm-wiki/references/EXTRACTION.md`
- Modify: `skill/llm-wiki/references/COMMANDS.md`
- Modify: `skill/llm-wiki/references/ARCHITECTURE.md`
- Modify: `skill/wiki-write/references/PAGES.md`
- Modify: `skill/llm-wiki/SKILL.md`
- Modify: `skill/llm-wiki/scripts/wikicore/store.py:21-28` (README canonical list)

- [ ] **Step 1: `store.py` README canonical list**

Change line 23-24 to mention `topics.json`:

```
Evidence-backed project memory. Canonical data lives in `sources/`, `claims/`,
`decisions/`, `topics.json`; everything under `wiki/`, `context/`, `views/` is derived and
```

- [ ] **Step 2: `wiki-compile/SKILL.md` — two-file extract contract**

After line 23 (`python3 "$WIKI" compile --resume` block), update the engine example:

```bash
python3 "$WIKI" compile        # → pending[] with content_path + topics_path + candidates_path
# per pending entry:
#  1) write topics.jsonl (one topic/line) BEFORE candidates:
#     {"subject":"lake.s3","kind":"entity","title":"Amazon S3","aliases":["S3"]}
#     kinds: concept|entity|procedure|question|change|decision
#  2) write candidates.jsonl shorthand as before
python3 "$WIKI" compile --resume   # → auto counts + needs_review rows
```

And in the post-apply steps add, before synthesis:

```
0. Topics — the receipt's `topics` block lists `decision_topics[]` (record each
   with `wiki.py record-decision --file decision.json`), `unclaimed[]`
   (declared but no claims — extract more or accept), `auto_registered[]`,
   `kind_conflicts[]` (resolve via `review act set_topic_kind`), and
   `no_topics_declared` warnings.
```

- [ ] **Step 3: `EXTRACTION.md` — topics.jsonl format**

Insert a new section before "## Shorthand fields":

```markdown
## Topic declaration (topics.jsonl)

Before writing candidates, write one topic row per subject the source
mentions to `topics_path` — the topology comes first, claims attach to it:

{"subject":"lake.data-lake","kind":"concept","title":"Data lake"}
{"subject":"lake.s3","kind":"entity","title":"Amazon S3","aliases":["S3"]}
{"subject":"policy.use-postgres","kind":"decision","title":"Use Postgres","decided_on":"2026-10-01"}

Kinds: concept | entity | procedure | question | change | decision
(plurals normalized; `source` is reserved — the engine registers each
compiled source itself). Rows need `subject`, `kind`, `title`; `aliases`
optional. Declared resolved topics get a page with a single eligible claim;
subjects you extract but never declare auto-register as `concept` with a
receipt warning. A `decision` row produces no page — it lists in
`decision_topics[]` for `record-decision` after apply.
```

- [ ] **Step 4: `COMMANDS.md` — three additions**

After the `subjects` section add:

```markdown
## topics

```bash
wiki.py topics            # registry census: subject, kind, status, claim_count
wiki.py topics --backfill # register existing claim subjects (idempotent migration)
```

## record-decision

```bash
wiki.py record-decision --file decision.json
```

Input: `{title, status, decided_on, claims[], evidence[], body?, scope?}` —
no `id`/`recorded_at`/`version` (engine stamps them). Every `claims[]` id
must exist and be non-terminal; every `evidence[].source_id` must resolve.
```

In the `compile` section's pending-entry description, mention `topics_path`
and that `topics.jsonl` is written before `candidates.jsonl`. In the `review`
section's action list add `set_topic_kind` with params
`{"subject": "...", "kind": "..."}` resolving `topic_kind_conflict` items.

- [ ] **Step 5: `ARCHITECTURE.md` + `PAGES.md` + `SKILL.md`**

`ARCHITECTURE.md` — under "Storage rule": change canonical list to
"`sources/ claims/ decisions/ topics.json notes/ plus .state/`" and add one
line: "The topic registry records agent-declared subject→kind topology;
`page-targets` routes declared resolved subjects to `wiki/<kind>/` with a
single eligible claim."

`PAGES.md` — under "concept / entity page": add "Declared topics
(`reason: "declared"` targets) can be authored from a single claim; the
same citation rules apply."

`llm-wiki/SKILL.md` command router — add row:

```
| List declared topics / register existing subjects | references/COMMANDS.md#topics | `wiki.py topics [--backfill]` |
```

- [ ] **Step 6: Full suite + smoke run**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest discover tests -v`
Expected: all PASS

Smoke — full lifecycle on a temp wiki:

```bash
cd /tmp && rm -rf wikitest && mkdir wikitest && cd wikitest
WIKI=/Users/harry/Desktop/personal/llm-wiki/skill/llm-wiki/scripts/wiki.py
python3 $WIKI init --name smoketest
python3 $WIKI ingest --text "# Data engineering\nA data lake stores raw data on S3. A lakehouse uses Delta Lake on top. A data warehouse like BigQuery is curated." --text-ref lake-doc
python3 $WIKI compile  # capture pending paths
# write topics.jsonl + candidates.jsonl per skill contract, then:
python3 $WIKI compile --resume
python3 $WIKI compile --resume
python3 $WIKI topics          # census shows declared kinds
python3 $WIKI page-targets    # entities/s3 etc appear with reason "declared"
python3 $WIKI record-decision --file /tmp/dec.json  # if a decision topic was declared
python3 $WIKI verify          # "ok": true (after write-page for targets)
python3 $WIKI doctor
```

Expected: `page-targets` emits `wiki/entities/s3.md` etc. with `"reason": "declared"`; `verify` ok after pages written.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "docs(skills): topic-map compile contract across wiki-compile/extraction/commands/architecture"
```

---

## Self-review notes

- Coverage: spec §1 (topics.jsonl+topics_path) → Task 3; §2 (registry+merge table) → Tasks 1+3; §3 (page-targets) → Task 4; §4 (record-decision) → Task 5; §5 (topics cmd, set_topic_kind, doctor, backfill) → Tasks 1+6; §6 files → all; §7 docs → Task 7.
- `topics._load_for_txn` reads `txn._state_overlay` — a private field, but it is the established compose mechanism used by `deps.invalidate` (deps.py:51).
- `set_topic_kind` re-acting on a resolved subject raises `TxnError` — the test covers this.
