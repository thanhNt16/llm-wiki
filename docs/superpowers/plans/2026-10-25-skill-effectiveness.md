# Skill-Effectiveness Phase Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the landscape lessons (navigation-topology lint, extend-don't-create guard) and build a deterministic before/after benchmark harness plus a two-arm semantic comparison that proves the skillset improved.

**Architecture:** Engine changes are additive checks in `wikicore/doctor.py` and `wikicore/synthesize.py` (no schema/layout changes). The benchmark harness lives in `evals/benchmarks/`, drives the engine **CLI via subprocess** (so identical harness code measures the base engine `bb7a51f` and HEAD), and computes metrics by reading CLI JSON output and the built wiki directory. A two-arm semantic protocol (grep vs wiki) is executed by subagents and scored mechanically.

**Tech Stack:** Python ≥3.9 stdlib only (unittest, json, subprocess, re, tempfile). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-25-skill-effectiveness-design.md`

## Global Constraints

- Python ≥3.9, stdlib only — no third-party imports anywhere.
- Existing suites must stay green after every task: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests` (346 tests) and `python3 evals/deterministic/run_gates.py` (9 gates, all PASS).
- New doctor findings are `warning` severity — they must NOT flip `doctor` `ok` or `verify` (only `error` severity does, per existing behavior).
- Guard threshold: normalized-token Jaccard ≥ 0.6 OR normalized-title equality; guard applies to NEW pages only (refresh path keeps the existing subject-immutability check); `--allow-new` overrides and records `forced_new: true` in frontmatter.
- Benchmark harness never imports `wikicore` internals — subprocess CLI only, so the same code runs against any engine checkout.
- Exit-code contract: TxnError → exit 3 (existing behavior in `wiki.py main`).
- Commit style: conventional-ish prefixes (`feat:`, `test:`, `bench:`, `docs:`), one logical change per commit, in the worktree `/Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness` on branch `skill-effectiveness`.

---

### Task 1: Doctor navigation-topology lint

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/doctor.py` (add `_topology_findings`, call from `run` before the findings sort at the end)
- Test: `skill/llm-wiki/scripts/tests/test_doctor.py` (append)

**Interfaces:**
- Consumes: `doctor.run(wiki) -> {"ok": bool, "findings": [...], "counts": {...}}`; page layout `wiki/<section>/<slug>.md` for sections `("concepts","entities","procedures","questions","sources","changes")`; section indexes at `wiki/<section>/index.md`; `wiki/overview.md`.
- Produces: new finding codes `orphan_page` (warning), `dead_end_page` (warning), `section_index_unlinked` (warning). Later tasks and the bench harness rely on these exact code strings.

- [ ] **Step 1: Write the failing tests**

Append to `test_doctor.py` (reuses existing `fresh()` and `add_accepted_claim()` helpers at top of that file):

```python
def _write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


PAGE_TMPL = """---
type: concept
subject: %s
title: %s
created: 2026-09-15T00:00:00Z
---

# %s

See [[%s]].
[related](../concepts/index.md)
"""


class TopologyFindingsTest(unittest.TestCase):
    def setUp(self):
        self.wiki = fresh()
        self.c = add_accepted_claim(self.wiki, "subject alpha", "has", 1)
        # a healthy section: index lists the page, page links out
        _write(self.wiki.p("wiki", "concepts", "subject-alpha.md"),
               PAGE_TMPL % ("subject alpha", "Subject Alpha", "Subject Alpha",
                            self.c["id"]))
        _write(self.wiki.p("wiki", "concepts", "index.md"),
               "# Index\n\n| Page | Title |\n|---|---|\n"
               "| [subject-alpha](subject-alpha.md) | Subject Alpha |\n")
        _write(self.wiki.p("wiki", "overview.md"),
               "# Overview\n\n- [concepts](concepts/index.md) (1)\n")

    def _codes(self):
        out = doctor.run(self.wiki)
        return [f["code"] for f in out["findings"]], out

    def test_healthy_section_has_no_topology_findings(self):
        codes, out = self._codes()
        self.assertNotIn("orphan_page", codes)
        self.assertNotIn("dead_end_page", codes)
        self.assertNotIn("section_index_unlinked", codes)
        self.assertTrue(out["ok"])  # warnings never fail doctor

    def test_orphan_page_flagged(self):
        _write(self.wiki.p("wiki", "concepts", "stray-page.md"),
               PAGE_TMPL % ("stray page", "Stray", "Stray", self.c["id"]))
        codes, _ = self._codes()
        self.assertIn("orphan_page", codes)
        detail = [f for f in doctor.run(self.wiki)["findings"]
                  if f["code"] == "orphan_page"][0]["detail"]
        self.assertIn("stray-page.md", detail)

    def test_dead_end_page_flagged(self):
        _write(self.wiki.p("wiki", "concepts", "subject-alpha.md"),
               "---\ntype: concept\nsubject: subject alpha\n"
               "title: Subject Alpha\ncreated: 2026-09-15T00:00:00Z\n---\n\n"
               "# Subject Alpha\n\nNo links at all here.\n")
        codes, _ = self._codes()
        self.assertIn("dead_end_page", codes)

    def test_section_index_unlinked_flagged(self):
        _write(self.wiki.p("wiki", "overview.md"), "# Overview\n\n- nothing\n")
        codes, _ = self._codes()
        self.assertIn("section_index_unlinked", codes)

    def test_indexes_and_overview_exempt(self):
        codes, _ = self._codes()
        # index.md and overview.md must never be flagged as orphan/dead-end
        details = " ".join(f["detail"] for f in doctor.run(self.wiki)["findings"])
        self.assertNotIn("index.md is not listed", details.replace("concepts/index", "X"))
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness && python3 -m unittest skill.llm-wiki.scripts.tests.test_doctor 2>&1 | tail -5` — if module path fails, use `python3 -m unittest discover -s skill/llm-wiki/scripts/tests -p test_doctor.py -v 2>&1 | tail -15`
Expected: FAIL — new codes absent (AssertionError `orphan_page not in []`).

- [ ] **Step 3: Implement `_topology_findings` in doctor.py**

Add below the existing checks (before `findings.sort(...)` at the end of `run`):

```python
_MD_LINK_RE = re.compile(r"\[[^\]]*\]\(([^)#\s]+\.md)\)")
_WIKI_LINK_RE = re.compile(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]")

_PAGE_SECTIONS = ("concepts", "entities", "procedures", "questions",
                  "sources", "changes")


def _read(rel, wiki):
    path = wiki.p(rel)
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return None


def _split_front(text):
    if text.startswith("---"):
        end = text.find("\n---", 3)
        if end != -1:
            return text[end + 4:]
    return text


def _topology_findings(wiki, add):
    """Navigation-fabric lint: every content page must be reachable from its
    section index, link onward (claim or md link), and every section index
    must be linked from overview.md. Index/overview/changes pages are exempt
    structural nodes, not content pages."""
    overview = _read("wiki/overview.md", wiki) or ""
    for section in _PAGE_SECTIONS:
        sec_dir = wiki.p("wiki", section)
        if not os.path.isdir(sec_dir):
            continue
        index_rel = "wiki/%s/index.md" % section
        index_body = _read(index_rel, wiki) or ""
        index_links = set(_MD_LINK_RE.findall(index_body))
        linked_from_overview = ("%s/index.md" % section) in overview
        if os.path.isfile(wiki.p(index_rel)) and not linked_from_overview:
            add("warning", "section_index_unlinked",
                "wiki/%s/index.md exists but overview.md does not link it"
                % section,
                "rebuild pages (build-pages regenerates overview) or add the "
                "section link to overview.md")
        for name in sorted(os.listdir(sec_dir)):
            if not name.endswith(".md") or name == "index.md":
                continue
            rel = "wiki/%s/%s" % (section, name)
            if ("./" + name) not in ("./" + name) and name not in index_links \
                    and ("./%s" % name) not in index_links:
                add("warning", "orphan_page",
                    "wiki/%s/%s is not listed in its section index" %
                    (section, name),
                    "rebuild pages, or write the page via write-page so the "
                    "index regenerates")
            body = _split_front(_read(rel, wiki) or "")
            has_out = bool(_WIKI_LINK_RE.search(body)) or \
                bool(_MD_LINK_RE.search(body))
            if not has_out:
                add("warning", "dead_end_page",
                    "wiki/%s/%s links to no claim and no other page" %
                    (section, name),
                    "cite the claims it summarizes with [[claim_id]] links")
```

Call it inside `run` right before the final `findings.sort(...)` line:

```python
    # --- navigation topology (lesson: orphan/dead-end pages get no
    # readership; the overview -> section index -> page ladder is the
    # progressive-disclosure fabric) ---
    _topology_findings(wiki, add)
```

Note on the odd `if ("./" + name) not in ...` guard in `orphan_page`: simplify it to the clean form — the correct check is:

```python
            if name not in index_links:
                add("warning", "orphan_page", ...)
```

where `index_links` collects link targets from the index body via `_MD_LINK_RE` (targets are exactly like `subject-alpha.md`). Use the clean form; the `dead_end_page` body check ignores frontmatter.

- [ ] **Step 4: Run the tests — verify pass**

Run: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests -p test_doctor.py 2>&1 | tail -3`
Expected: `OK`.

- [ ] **Step 5: Full regression**

Run: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests 2>&1 | grep -E "^(OK|FAILED|Ran)"` and `python3 evals/deterministic/run_gates.py 2>&1 | grep -c PASS`
Expected: `OK (skipped=11)` and `9`.

- [ ] **Step 6: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/doctor.py skill/llm-wiki/scripts/tests/test_doctor.py
git commit -m "feat(doctor): navigation-topology lint — orphan_page, dead_end_page, section_index_unlinked"
```

---

### Task 2: Extend-don't-create guard (near-duplicate pages)

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/synthesize.py` (add `_norm_title_tokens`, `_jaccard`, `similar_pages`; wire into `page_targets` and `write_page`)
- Modify: `skill/llm-wiki/scripts/wiki.py` (add `--allow-new` to write-page parser; thread through `cmd_write_page`)
- Test: `skill/llm-wiki/scripts/tests/test_synthesize.py` (append)

**Interfaces:**
- Consumes: `page_targets(wiki, kind=None, stale_only=False, min_claims=2, min_sources=2) -> {"targets": [...], "waiting": [...], "warnings": [...]}`; `write_page(wiki, artifact, markdown_text, dep_list, base_revision) -> dict` (raises `TxnError` → CLI exit 3); page frontmatter has `title` and `subject`; pages live at `wiki/<kind>/<slugify(subject)>.md`.
- Produces: `similar_pages(wiki, kind, subject, title, exclude_slug=None) -> [{"slug": str, "title": str, "similarity": float}]` (empty when no near-duplicate); target dicts gain `similar_existing: [...]` (only when non-empty); `write_page(..., allow_new=False)` — raises `TxnError` naming the twin when creating a new page with a near-duplicate and `allow_new` is False, records `forced_new: true` in frontmatter when True; CLI flag `write-page --allow-new`.

- [ ] **Step 1: Write the failing tests**

Append to `test_synthesize.py`:

```python
class SimilarPagesTest(unittest.TestCase):
    def setUp(self):
        self.wiki = fresh()
        for i in (1, 2):
            r = sources.ingest(self.wiki, "text", "sim-src-%d" % i, b"b%d" % i)
            add_accepted_claim_raw(self.wiki, r, "attribution window",
                                   "length", "%dd" % (7 if i == 1 else 30))

    def _existing_page(self):
        md = ("---\ntype: concept\nsubject: attribution window\n"
              "title: Attribution Window\ncreated: 2026-09-15T00:00:00Z\n"
              "deps: []\n---\n\n# Attribution Window\n\n"
              "[[%s]]\n" % self.wiki and "")
        return md

    def test_similar_pages_finds_near_duplicate(self):
        # plant an existing page by writing one claim set + page manually
        cl = claims.list_claims(self.wiki, subject="attribution window")
        md = ("---\ntype: concept\nsubject: attribution window\n"
              "title: Attribution Window\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n# Attribution Window\n\n"
              + "\n".join("[[%s]]" % c["id"] for c in cl))
        out = synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                                    md, None, self.wiki.revision())
        self.assertTrue(out)
        sim = synthesize.similar_pages(self.wiki, "concepts",
                                       "attribution windows", "Attribution Windows")
        self.assertTrue(any(s["similarity"] >= 0.6 for s in sim))
        sim_far = synthesize.similar_pages(self.wiki, "concepts",
                                           "retry policy", "Retry Policy")
        self.assertEqual(sim_far, [])

    def test_write_page_refuses_near_duplicate_new_page(self):
        cl = claims.list_claims(self.wiki, subject="attribution window")
        md = ("---\ntype: concept\nsubject: attribution window\n"
              "title: Attribution Window\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n# Attribution Window\n\n"
              + "\n".join("[[%s]]" % c["id"] for c in cl))
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              md, None, self.wiki.revision())
        with self.assertRaises(TxnError) as ctx:
            synthesize.write_page(
                self.wiki, "wiki/concepts/attribution-window-backup.md",
                md.replace("subject: attribution window",
                           "subject: attribution window backup")
                  .replace("title: Attribution Window",
                           "title: Attribution Window Backup")
                  .replace("attribution-window.md", ""),
                None, self.wiki.revision())
        self.assertIn("attribution-window.md", str(ctx.exception))

    def test_write_page_allow_new_overrides(self):
        cl = claims.list_claims(self.wiki, subject="attribution window")
        md = ("---\ntype: concept\nsubject: attribution window\n"
              "title: Attribution Window\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n# Attribution Window\n\n"
              + "\n".join("[[%s]]" % c["id"] for c in cl))
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              md, None, self.wiki.revision())
        out = synthesize.write_page(
            self.wiki, "wiki/concepts/attribution-window-backup.md",
            md.replace("subject: attribution window",
                       "subject: attribution window backup")
              .replace("title: Attribution Window",
                       "title: Attribution Window Backup"),
            None, self.wiki.revision(), allow_new=True)
        self.assertTrue(out)
        text = open(self.wiki.p("wiki/concepts/attribution-window-backup.md")).read()
        self.assertIn("forced_new: true", text)

    def test_page_targets_emit_similar_existing(self):
        cl = claims.list_claims(self.wiki, subject="attribution window")
        md = ("---\ntype: concept\nsubject: attribution window\n"
              "title: Attribution Window\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n# Attribution Window\n\n"
              + "\n".join("[[%s]]" % c["id"] for c in cl))
        synthesize.write_page(self.wiki, "wiki/concepts/attribution-window.md",
                              md, None, self.wiki.revision())
        targets = synthesize.page_targets(self.wiki)
        t = [x for x in targets["targets"] if x["subject"] == "attribution window"]
        self.assertTrue(t)
        self.assertIn("similar_existing", t[0])
```

Note: `add_accepted_claim_raw` may not exist in test_synthesize.py — reuse the file's existing claim-creation helper (`add_accepted_claim` or equivalent; check the top of test_synthesize.py and adapt the call to its actual name/signature). If the helper ingests its own source, use it directly instead of the loop in `setUp`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests -p test_synthesize.py 2>&1 | tail -5`
Expected: FAIL — `module 'synthesize' has no attribute 'similar_pages'`.

- [ ] **Step 3: Implement the guard in synthesize.py**

Add after `slugify`:

```python
_TITLE_STOPWORDS = {"the", "a", "an", "of", "and", "or", "to", "for", "in",
                    "on", "is", "are"}


def _norm_title_tokens(text: str) -> set:
    toks = re.split(r"[^0-9a-z]+", text.lower())
    return {t for t in toks if t and t not in _TITLE_STOPWORDS}


def _jaccard(a: set, b: set) -> float:
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def similar_pages(wiki, kind: str, subject: str, title: str,
                  exclude_slug: Optional[str] = None) -> list:
    """Near-duplicate pages for a proposed new page: token-Jaccard >= 0.6 on
    title/subject vs every existing page's title/subject in ALL sections
    (cross-section twins are the worst kind). Lesson: 'extend, don't create' —
    the default failure mode is near-duplicate pages."""
    want = _norm_title_tokens(title) | _norm_title_tokens(subject)
    out = []
    for sec in PAGE_KINDS:
        sec_dir = wiki.p("wiki", sec)
        if not os.path.isdir(sec_dir):
            continue
        for name in sorted(os.listdir(sec_dir)):
            if not name.endswith(".md") or name == "index.md":
                continue
            slug = name[:-3]
            if exclude_slug is not None and slug == exclude_slug and sec == kind:
                continue
            path = wiki.p("wiki", sec, name)
            try:
                with open(path, encoding="utf-8") as f:
                    text = f.read()
            except OSError:
                continue
            front, _ = _split_front(text)
            have = _norm_title_tokens(str(front.get("title") or "")) | \
                _norm_title_tokens(str(front.get("subject") or ""))
            sim = _jaccard(want, have)
            if sim >= 0.6 or (str(front.get("title") or "").strip().lower()
                              == (title or "").strip().lower()):
                out.append({"section": sec, "slug": slug,
                            "title": front.get("title") or "",
                            "similarity": round(sim, 3)})
    return out
```

Wire into `page_targets`: after the `targets.append({...})` dict is built (inside the loop over `groups`), add:

```python
        sim = similar_pages(wiki, k, subject,
                            t_title if (t_title := subject.title()) else subject,
                            exclude_slug=None)
        if t.get("exists"):
            sim = []  # guard is for NEW pages only
        if sim:
            t["similar_existing"] = sim
            t["routing"] = "extend-first"
```

Place this as its own loop after the slug-collision block (so `t["slug"]` is final):

```python
    for t in targets:
        if t["exists"]:
            continue
        sim = similar_pages(wiki, t["kind"], t["subject"], t["subject"].title())
        if sim:
            t["similar_existing"] = sim
            t["routing"] = "extend-first"
```

Wire into `write_page`: extend the signature to
`def write_page(wiki, artifact, markdown_text, dep_list, base_revision, allow_new=False):`
and after the new-page `created` validation block (the `else:` branch that requires an ISO `created`), add:

```python
        if not allow_new:
            twins = similar_pages(wiki, parts[1], subject,
                                  str(front.get("title") or ""))
            if twins:
                names = ", ".join("wiki/%s/%s.md" % (tw["section"], tw["slug"])
                                  for tw in twins[:3])
                raise TxnError(
                    "near-duplicate page: %s already covers this topic "
                    "(similarity >= 0.6). Extend it instead of creating a new "
                    "page, or pass --allow-new to force creation: %s"
                    % (names, names.split(", ")[0]))
```

And when `allow_new` is True and the page is new, set `front["forced_new"] = True` before the write (next to where `front["deps"]` is assigned).

CLI wiring in `wiki.py`: add to the write-page parser
`p.add_argument("--allow-new", action="store_true", dest="allow_new", help="allow creating a page that near-duplicates an existing one (records forced_new: true)")`
and pass `allow_new=args.allow_new` in the `synthesize.write_page(...)` call in `cmd_write_page`.

- [ ] **Step 4: Run the tests — verify pass**

Run: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests -p test_synthesize.py 2>&1 | tail -3`
Expected: `OK`.

- [ ] **Step 5: Full regression**

Run: full unittest discover + `python3 evals/deterministic/run_gates.py 2>&1 | grep -c PASS`
Expected: `OK (skipped=11)`, `9`.

- [ ] **Step 6: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/synthesize.py skill/llm-wiki/scripts/wiki.py skill/llm-wiki/scripts/tests/test_synthesize.py
git commit -m "feat(pages): extend-don't-create guard — similar_pages, similar_existing on targets, write-page refusal with --allow-new"
```

---

### Task 3: Benchmark fixture + engine driver

**Files:**
- Create: `evals/benchmarks/__init__.py` (empty)
- Create: `evals/benchmarks/fixture.py` (planted claims + gold questions + stray/near-dup probes)
- Create: `evals/benchmarks/driver.py` (subprocess engine wrapper)

**Interfaces:**
- Consumes: engine CLI commands (all present at base `bb7a51f` and HEAD): `init --name`, `ingest --file`, `stage-candidates --file --source-id --source-version --report`, `reconcile-prepare --run`, `reconcile-apply --run --classifications --base-revision`, `build-pages`, `page-targets`, `write-page --text --artifact --deps`, `query-prepare --question`, `context-pack --task --budget`, `verify`, `doctor`. CLI JSON goes to stdout (one JSON object per command via `_emit`).
- Produces: `fixture.CANDIDATE_BATCHES -> list[{"source_rel": str, "candidates": [candidate-dict...], "classifications": [classification-dict...]}]` (reuse the `candidate()`/`report()` dict shapes from `evals/deterministic/run_gates.py`); `fixture.GOLD_QUESTIONS -> list[{"id": str, "question": str, "expect": [{"subject","predicate","value"}...]}]`; `driver.Engine(path: str)` with `.run(args, input_text=None, cwd=None) -> (exit_code, stdout, stderr)` and `.json(args, ...) -> parsed-stdout-or-None`; `driver.build_wiki(engine, corpus_dir, wiki_root) -> dict` performing init→ingest→stage→reconcile→build-pages→verify and returning per-step exit codes and the reconcile run ids.

- [ ] **Step 1: Write fixture.py**

```python
"""Planted claims + gold questions for the deterministic benchmark.

Mirrors the golden-corpus planted cases (attribution window correction 30d->7d,
retry conflict 3 vs 5, staging scope) plus three deliberate probes:
- near-duplicate page attempt ("Attribution Window" then "Attribution Window
  Backup") — exercises the extend-don't-create guard (base: created; HEAD:
  refused with exit 3)
- single-origin subject ("feature flag rollout") — lands on page-targets
  waiting list (two-source rule)
- stray page written directly to the filesystem — orphan_page lint target.
"""
import json
import os

CORPUS_DOCS = ["docs/adr-007-retries.md", "docs/adr-019-attribution.md",
               "notes/memo-2026-09-01-window-change.md",
               "config/analytics.prod.json", "config/analytics.sandbox.json",
               "notes/meeting-2026-09-10.md"]


def _cand(subject, predicate, value, authority="documented",
          scope=None, valid_from=None, supersedes=()):
    return {
        "id": "auto", "subject": subject, "predicate": predicate,
        "value": value, "scope": scope or {}, "valid_from": valid_from,
        "valid_to": None, "recorded_at": "2026-09-15T00:00:00Z",
        "status": "candidate", "proposed_by": "bench",
        "authority": {"type": authority},
        "evidence": [{"source_id": "FILLED_BY_DRIVER", "source_version": 1,
                      "locator": {"type": "heading", "value": "H"}}],
        "supersedes": list(supersedes),
    }


def _cls(rel="UNRELATED", target=None, valid_from=None):
    return [{"index": 0, "relationship": rel, "target_claim_id": target,
             "valid_from": valid_from, "valid_to": None,
             "authority": {"type": "documented"}}]


# batches keyed by source; driver fills evidence source ids after ingest
BATCHES = [
    {"source_rel": "docs/adr-019-attribution.md", "claims": [
        _cand("attribution", "window-days", 30, valid_from="2026-01-01"),
        _cand("attribution", "scope", "prod"),
    ]},
    {"source_rel": "notes/memo-2026-09-01-window-change.md", "claims": [
        _cand("attribution", "window-days", 7, valid_from="2026-09-01",
              supersedes=["AUTO:attribution|window-days|30"]),
    ]},
    {"source_rel": "config/analytics.prod.json", "claims": [
        _cand("attribution", "scope", "prod"),
    ]},
    {"source_rel": "config/analytics.sandbox.json", "claims": [
        _cand("attribution", "scope", "sandbox", scope={"env": "sandbox"}),
    ]},
    {"source_rel": "docs/adr-007-retries.md", "claims": [
        _cand("retry", "max-attempts", 3),
        _cand("retry", "backoff", "exponential"),
    ]},
    {"source_rel": "notes/meeting-2026-09-10.md", "claims": [
        _cand("retry", "max-attempts", 5),
    ]},
    {"source_rel": "docs/adr-001-postgres.md", "claims": [
        _cand("order service", "database", "postgres"),
        _cand("feature flag rollout", "status", "in-progress"),
    ]},
]

# supersede resolution: the memo claim supersedes the adr-019 30d claim;
# driver marks this by ordering batches and using CORRECTS relationship on
# the second batch's window-days claim against the first batch's claim id.

GOLD_QUESTIONS = [
    {"id": "q1-order-db", "question": "Which database does the order service use?",
     "expect": [{"subject": "order service", "predicate": "database",
                 "value": "postgres"}]},
    {"id": "q2-window-now", "question": "What is the current attribution window?",
     "expect": [{"subject": "attribution", "predicate": "window-days",
                 "value": 7}]},
    {"id": "q3-window-old", "question": "What was the attribution window before September 2026?",
     "expect": [{"subject": "attribution", "predicate": "window-days",
                 "value": 30}], "as_of": "2026-08-15"},
    {"id": "q4-retries", "question": "How many retry attempts are configured?",
     "expect": [{"subject": "retry", "predicate": "max-attempts", "value": 3},
                {"subject": "retry", "predicate": "max-attempts", "value": 5}]},
    {"id": "q5-sandbox", "question": "Does staging use the same attribution window as prod?",
     "expect": [{"subject": "attribution", "predicate": "scope",
                 "value": "sandbox"}]},
    {"id": "q6-unanswerable", "question": "What is our 2028 pricing plan?",
     "expect": []},
]

NEAR_DUP_SUBJECT = "attribution window backup"
NEAR_DUP_TITLE = "Attribution Window Backup"
STRAY_PAGE_REL = "wiki/concepts/stray-injected-page.md"
STRAY_PAGE_TEXT = ("---\ntype: concept\nsubject: stray injected page\n"
                   "title: Stray Injected Page\n"
                   "created: 2026-09-15T00:00:00Z\n---\n\n"
                   "# Stray Injected Page\n\nNo links.\n")
```

- [ ] **Step 2: Write driver.py**

```python
"""Subprocess wrapper around a wiki.py engine checkout.

The bench harness NEVER imports wikicore from the engine under test — it
drives the CLI so the identical harness measures any engine revision (base
bb7a51f or HEAD) fairly."""
import json
import subprocess
import sys


class Engine:
    def __init__(self, wiki_py: str):
        self.wiki_py = wiki_py

    def run(self, args, cwd, input_text=None):
        proc = subprocess.run(
            [sys.executable, self.wiki_py] + [str(a) for a in args],
            cwd=str(cwd), capture_output=True, text=True,
            input=input_text, timeout=300)
        return proc.returncode, proc.stdout, proc.stderr

    def json(self, args, cwd, input_text=None):
        code, out, err = self.run(args, cwd, input_text)
        if code != 0:
            return code, None, err
        try:
            return code, json.loads(out), err
        except ValueError:
            return code, None, "non-JSON stdout: %s" % out[:200]


def build_wiki(engine: Engine, corpus_dir: str, wiki_root: str) -> dict:
    """init -> ingest corpus docs -> stage planted claims -> reconcile ->
    build-pages. Returns {"steps": [{cmd, exit}...], "ok": bool}."""
    import json as _json
    import os
    import shutil
    steps = []

    def step(name, args, ok_codes=(0,)):
        code, out, err = engine.run(args, wiki_root)
        steps.append({"step": name, "exit": code,
                      "err": err.strip()[-200:] if code not in ok_codes else ""})
        return code, out

    code, _ = step("init", ["init", "--name", "bench"])
    if code != 0:
        return {"steps": steps, "ok": False}

    import tempfile
    claim_id_by_key = {}
    for batch in _json.loads(_json.dumps(__import__("fixture").BATCHES)):
        pass  # placeholder — replaced in Step 3b below

    return {"steps": steps, "ok": all(s["exit"] == 0 for s in steps)}
```

The `build_wiki` body above is a skeleton — complete it as follows (replace the placeholder loop):

```python
    from fixture import BATCHES, CORPUS_DOCS
    src_ids = {}
    for rel in CORPUS_DOCS:
        code, out = step("ingest " + rel, ["ingest", "--file",
                                           os.path.join(corpus_dir, rel)])
        if code != 0:
            return {"steps": steps, "ok": False}
        src_ids[rel] = json.loads(out)["source_id"]

    supersede_targets = {}
    for batch in BATCHES:
        rel = batch["source_rel"]
        sid = src_ids[rel]
        cands = []
        for c in batch["claims"]:
            c = json.loads(json.dumps(c))
            for ev in c["evidence"]:
                ev["source_id"] = sid
            sup = [s for s in c["supersedes"] if not s.startswith("AUTO:")]
            autos = [s for s in c["supersedes"] if s.startswith("AUTO:")]
            if autos:
                key = autos[0][len("AUTO:"):]
                supersede_targets.setdefault(key, []).append((c, len(cands)))
            c["supersedes"] = sup
            cands.append(c)
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(cands, f)
            cand_path = f.name
        report = {"source_id": sid, "source_version": 1,
                  "coverage": {"text": "complete"}, "warnings": [],
                  "parser": {"name": "bench", "version": "1"}}
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(report, f)
            report_path = f.name
        code, out = step("stage " + rel,
                         ["stage-candidates", "--file", cand_path,
                          "--source-id", sid, "--source-version", "1",
                          "--report", report_path])
        if code != 0:
            return {"steps": steps, "ok": False}
        run_id = json.loads(out).get("run_id")
        # reconcile with UNRELATED (auto-accept) unless a supersede target exists
        cls = []
        for i, c in enumerate(cands):
            key = "%s|%s|%s" % (c["subject"], c["predicate"],
                                _short_value(c["value"]))
            if key in supersede_targets:
                continue  # handled in a later batch pass below
            cls.append({"index": i, "relationship": "UNRELATED",
                        "target_claim_id": None, "valid_from": None,
                        "valid_to": None,
                        "authority": {"type": c["authority"]["type"]}})
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(cls, f)
            cls_path = f.name
        code, _ = step("reconcile " + rel,
                       ["reconcile-apply", "--run", run_id,
                        "--classifications", cls_path])
        if code != 0:
            return {"steps": steps, "ok": False}
    step("build-pages", ["build-pages"])
    code, out = step("verify", ["verify"])
    ok = code == 0
    try:
        ok = ok and json.loads(out).get("ok", False)
    except ValueError:
        ok = False
    return {"steps": steps, "ok": ok,
            "source_ids": src_ids}


def _short_value(v):
    return str(v)
```

Supersede handling: the `30 -> 7` correction must flow through `reconcile-apply` with `relationship=CORRECTS` and `target_claim_id` of the 30d claim. Because claim ids are assigned at staging, do a **two-pass build**: pass 1 stages+reconciles every batch UNRELATED *except* the memo's window-days claim; pass 2 looks up the created 30d claim id via `query-prepare --question "attribution window"` (or by reading `claims/claim_*.json` in wiki_root — simpler: glob `claims/claim_*.json`, find subject=attribution, predicate=window-days, value=30), then re-stages the memo batch with `supersedes=[that_id]` and classification `CORRECTS`. Implement this concretely — a helper `_apply_correction(engine, wiki_root, src_ids)` that globs the claims dir, finds the 30d claim, writes a one-candidate batch from the memo source with `supersedes` set, and reconciles with `{"index":0,"relationship":"CORRECTS","target_claim_id":<id>,"valid_from":"2026-09-01",...}`. Adjust `BATCHES` consumption so the memo's window-days claim is skipped in pass 1 (mark it in `fixture.py` with a key `"defer": "correction"` on that claim and filter on it).

- [ ] **Step 3: Smoke-test the driver manually**

Run (from repo root, HEAD engine):
```bash
cd /Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness && python3 - <<'EOF'
import shutil, tempfile, os, sys
sys.path.insert(0, "evals/benchmarks")
from driver import Engine, build_wiki
root = tempfile.mkdtemp(prefix="bench-smoke-")
res = build_wiki(Engine("skill/llm-wiki/scripts/wiki.py"),
                 "evals/golden-corpus", root)
print("ok:", res["ok"])
for s in res["steps"]:
    print(s["step"], s["exit"], s["err"][:100])
shutil.rmtree(root)
EOF
```
Expected: `ok: True`, every step exit 0.

- [ ] **Step 4: Commit**

```bash
git add evals/benchmarks/__init__.py evals/benchmarks/fixture.py evals/benchmarks/driver.py
git commit -m "bench: planted fixture + subprocess engine driver (corpus, correction, conflict, scope, probes)"
```

---

### Task 4: Metrics, bench.py, compare.py, smoke tests

**Files:**
- Create: `evals/benchmarks/metrics.py`
- Create: `evals/benchmarks/bench.py`
- Create: `evals/benchmarks/compare.py`
- Test: `evals/benchmarks/test_bench.py`

**Interfaces:**
- Consumes: Task 3's `Engine`, `build_wiki`, `fixture.GOLD_QUESTIONS`, `NEAR_DUP_*`, `STRAY_*`; `query-prepare` stdout JSON `{"matches": [{"subject","predicate","value",...}, ...]}` (top matches sorted by score); `context-pack` stdout JSON with `receipt.budget.{requested,estimated}` and `receipt.claims`; `page-targets` stdout `{"targets":[...],"waiting":[...]}`; `doctor` stdout `{"findings":[{"code","severity",...}]}`; wiki dir layout from Task 1.
- Produces: `metrics.collect(engine, wiki_root, corpus_dir) -> dict` (the metrics object); `bench.main(argv)` writing `bench-<label>.json`; `compare.main([before.json after.json])` writing markdown + JSON diff to stdout/files; metric keys (stable, used by the report): `retrieval.precision_at_5`, `retrieval.gold_recall_at_5`, `retrieval.mrr`, `retrieval.refusals_correct`, `context.pack_tokens_avg`, `context.budget_compliance`, `context.compression_ratio`, `topology.pages`, `topology.orphan_pages`, `topology.dead_end_pages`, `topology.section_indexes_linked`, `topology.near_dup_pairs`, `twosource.corroborated`, `twosource.waiting`, `hygiene.doctor_findings_by_code` (dict), `guard.near_dup_blocked` (bool), `guard.stray_orphan_detected_by_doctor` (bool).

- [ ] **Step 1: Write metrics.py**

```python
"""Metric collection — reads engine CLI JSON + the built wiki dir.
Runs identically against any engine revision."""
import glob
import json
import os
import re

from driver import Engine
from fixture import (GOLD_QUESTIONS, NEAR_DUP_SUBJECT, NEAR_DUP_TITLE,
                     STRAY_PAGE_REL, STRAY_PAGE_TEXT)

from synthesize_like import norm_tokens  # local copy — see note below


def _triples(matches):
    return {(m.get("subject"), m.get("predicate"), m.get("value"))
            for m in matches}


def retrieval(engine: Engine, wiki_root: str) -> dict:
    hits, gold_total, rr_sum, refusals_ok = 0, 0, 0.0, 0
    for q in GOLD_QUESTIONS:
        args = ["query-prepare", "--question", q["question"]]
        if q.get("as_of"):
            args += ["--as-of", q["as_of"]]
        code, out, _ = engine.json(args, wiki_root)
        matches = (out or {}).get("matches", [])[:5]
        if not q["expect"]:
            refusals_ok += 1 if not matches else 0
            continue
        gold = {(e["subject"], e["predicate"], e["value"]) for e in q["expect"]}
        gold_total += len(gold)
        top = _triples(matches)
        found = gold & top
        hits += len(found)
        for rank, m in enumerate(matches, 1):
            if (m.get("subject"), m.get("predicate"), m.get("value")) in gold:
                rr_sum += 1.0 / rank
                break
    n = sum(1 for q in GOLD_QUESTIONS if q["expect"])
    return {"precision_at_5": None if not n else round(hits / (5 * n), 3),
            "gold_recall_at_5": None if not gold_total
            else round(hits / gold_total, 3),
            "mrr": None if not n else round(rr_sum / n, 3),
            "refusals_correct": refusals_ok ==
            sum(1 for q in GOLD_QUESTIONS if not q["expect"])}


def context(engine: Engine, wiki_root: str, corpus_dir: str) -> dict:
    tokens, reqs, ests, compliant = [], 0, 0, 0
    for q in GOLD_QUESTIONS[:4]:
        code, out, _ = engine.json(
            ["context-pack", "--task", q["question"], "--budget", "6000"],
            wiki_root)
        if code != 0 or not out:
            continue
        receipt = out.get("receipt", {})
        est = receipt.get("budget", {}).get("estimated")
        req = receipt.get("budget", {}).get("requested", 6000)
        if est is None:
            continue
        tokens.append(est)
        reqs += 1
        ests += est
        compliant += 1 if est <= req else 0
    corpus_tokens = 0
    for path in glob.glob(os.path.join(corpus_dir, "**", "*"), recursive=True):
        if os.path.isfile(path) and path.endswith((".md", ".json", ".csv")):
            with open(path, encoding="utf-8", errors="ignore") as f:
                corpus_tokens += len(f.read().split())
    avg = (ests / reqs) if reqs else 0
    return {"pack_tokens_avg": round(avg, 1),
            "budget_compliance": None if not reqs else compliant == reqs,
            "compression_ratio": None if not avg
            else round(corpus_tokens / avg, 2)}


def _page_files(wiki_root):
    out = []
    for sec in ("concepts", "entities", "procedures", "questions",
                "sources", "changes"):
        d = os.path.join(wiki_root, ".llm-wiki", "wiki", sec)
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if name.endswith(".md") and name != "index.md":
                out.append((sec, name[:-3]))
    return out


def topology(engine: Engine, wiki_root: str, corpus_dir: str,
             near_dup_created: bool) -> dict:
    root = os.path.join(wiki_root, ".llm-wiki")
    pages = _page_files(wiki_root)
    orphans, dead_ends, indexes_linked = 0, 0, 0
    for sec in ("concepts", "entities", "procedures", "questions",
                "sources", "changes"):
        idx = os.path.join(root, "wiki", sec, "index.md")
        if os.path.isfile(idx):
            body = open(idx, encoding="utf-8").read()
            for _sec, slug in pages:
                pass  # membership counted below
    # membership + dead ends per page
    for sec, slug in pages:
        idx = os.path.join(root, "wiki", sec, "index.md")
        listed = False
        if os.path.isfile(idx):
            listed = ("%s.md" % slug) in open(idx, encoding="utf-8").read()
        orphans += 0 if listed else 1
        body_path = os.path.join(root, "wiki", sec, slug + ".md")
        text = open(body_path, encoding="utf-8").read()
        has_out = re.search(r"\[\[[a-z]+_[0-9A-Za-z]+\]\]", text) or \
            re.search(r"\]\([^)]+\.md\)", text)
        dead_ends += 0 if has_out else 1
    overview = os.path.join(root, "wiki", "overview.md")
    ov = open(overview, encoding="utf-8").read() if os.path.isfile(overview) else ""
    for sec in ("concepts", "entities", "procedures", "questions",
                "sources", "changes"):
        if os.path.isfile(os.path.join(root, "wiki", sec, "index.md")):
            indexes_linked += 1 if ("%s/index.md" % sec) in ov else 0
    # near-dup pairs by title tokens (harness-side, engine-independent)
    from fixture import BATCHES  # noqa: F401  (kept for symmetry)
    titles = {}
    for sec, slug in pages:
        text = open(os.path.join(root, "wiki", sec, slug + ".md"),
                    encoding="utf-8").read()
        m = re.search(r"^title:\s*(.+)$", text, re.M)
        titles[(sec, slug)] = m.group(1).strip() if m else slug
    def toks(t):
        ws = {"the", "a", "an", "of", "and", "or", "to", "for", "in", "backup"}
        s = {w for w in re.split(r"[^0-9a-z]+", t.lower()) if w and w not in ws}
        return s
    keys = list(titles)
    near_pairs = 0
    for i in range(len(keys)):
        for j in range(i + 1, len(keys)):
            a, b = toks(titles[keys[i]]), toks(titles[keys[j]])
            if a and b and len(a & b) / len(a | b) >= 0.6:
                near_pairs += 1
    return {"pages": len(pages), "orphan_pages": orphans,
            "dead_end_pages": dead_ends,
            "section_indexes_linked": indexes_linked,
            "near_dup_pairs": near_pairs}


def hygiene_and_guard(engine: Engine, wiki_root: str,
                      stray_written: bool) -> dict:
    code, out, _ = engine.json(["doctor"], wiki_root)
    findings = (out or {}).get("findings", [])
    by_code = {}
    for f in findings:
        by_code[f["code"]] = by_code.get(f["code"], 0) + 1
    return {"doctor_findings_by_code": by_code,
            "stray_orphan_detected_by_doctor": by_code.get("orphan_page", 0) > 0}
```

Note: delete the `from synthesize_like import norm_tokens` line — it is unused; the `toks()` local function covers normalization. Also delete the vestigial first `for sec ... for _sec, slug ... pass` loop in `topology` (leftover sketch) — the real membership logic is the second loop.

- [ ] **Step 2: Write bench.py**

```python
#!/usr/bin/env python3
"""Deterministic before/after benchmark.

Usage: python3 bench.py --engine <path/to/wiki.py> --label <name> \
                        --corpus <golden-corpus-dir> [--out dir]
Builds the planted wiki via the CLI, injects the stray page, attempts the
near-duplicate write, collects metrics, writes bench-<label>.json.
"""
import argparse
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import driver  # noqa: E402
import fixture  # noqa: E402
import metrics  # noqa: E402


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", required=True, help="path to wiki.py under test")
    ap.add_argument("--label", required=True)
    ap.add_argument("--corpus", default=os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "golden-corpus"))
    ap.add_argument("--out", default=os.path.dirname(os.path.abspath(__file__)))
    args = ap.parse_args(argv)

    engine = driver.Engine(os.path.abspath(args.engine))
    root = tempfile.mkdtemp(prefix="llmwiki-bench-")
    corpus = os.path.abspath(args.corpus)
    try:
        build = driver.build_wiki(engine, corpus, root)
        if not build.get("ok"):
            print(json.dumps({"label": args.label, "error": "build failed",
                              "steps": build.get("steps", [])}, indent=2))
            return 1

        # probe 1: stray page (filesystem-level, engine-independent)
        stray_path = os.path.join(root, ".llm-wiki", fixture.STRAY_PAGE_REL)
        os.makedirs(os.path.dirname(stray_path), exist_ok=True)
        with open(stray_path, "w") as f:
            f.write(fixture.STRAY_PAGE_TEXT)

        # probe 2: near-duplicate page attempt via the engine
        claims_out = None
        code, qp, _ = engine.json(
            ["query-prepare", "--question", "attribution window"], root)
        claims_out = qp
        md = ("---\ntype: concept\nsubject: %s\ntitle: %s\n"
              "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n"
              "[[c_placeholder]]\n"
              % (fixture.NEAR_DUP_SUBJECT, fixture.NEAR_DUP_TITLE,
                 fixture.NEAR_DUP_TITLE))
        # link a real claim from the built wiki so deps validate
        import glob as _g
        cands = _g.glob(os.path.join(root, ".llm-wiki", "claims", "claim_*.json"))
        link = ""
        for cp in cands:
            with open(cp) as f:
                c = json.load(f)
            if c.get("subject") == "attribution":
                link = "[[%s]]" % c["id"]
                break
        md = md.replace("[[c_placeholder]]", link or "")
        from synthesize_slug import slugify  # NOT imported — see note
        return 0  # replaced below
    finally:
        pass
```

The tail of `bench.py` above is sketch — replace everything after the `md = md.replace(...)` line with this concrete continuation (and delete the bogus `from synthesize_slug import` line):

```python
        slug = fixture.NEAR_DUP_SUBJECT.replace(" ", "-")
        code, out, err = engine.run(
            ["write-page", "--text", md,
             "--artifact", "wiki/concepts/%s.md" % slug], root)
        near_dup_blocked = (code == 3)

        ret = metrics.retrieval(engine, root)
        ctx = metrics.context(engine, root, corpus)
        topo = metrics.topology(engine, root, corpus, near_dup_blocked)
        hyg = metrics.hygiene_and_guard(engine, root, stray_written=True)

        result = {
            "label": args.label,
            "engine": os.path.abspath(args.engine),
            "build_steps": build["steps"],
            "guard": {"near_dup_blocked": near_dup_blocked,
                      **hyg.pop("guard", {})},
            "retrieval": ret,
            "context": ctx,
            "topology": topo,
            "hygiene": hyg,
        }
        out_path = os.path.join(args.out, "bench-%s.json" % args.label)
        with open(out_path, "w") as f:
            json.dump(result, f, indent=2, sort_keys=True)
        print(out_path)
        shutil.rmtree(root, ignore_errors=True)
        return 0
    finally:
        shutil.rmtree(root, ignore_errors=True)
```

- [ ] **Step 3: Write compare.py**

```python
#!/usr/bin/env python3
"""Side-by-side diff of two bench JSON files. Exit 0 always (report tool)."""
import json
import sys

BETTER_UP = {"gold_recall_at_5", "precision_at_5", "mrr", "compression_ratio",
             "section_indexes_linked", "corroborated"}
BETTER_DOWN = {"pack_tokens_avg", "orphan_pages", "dead_end_pages",
               "near_dup_pairs", "waiting"}


def flat(d, prefix=""):
    out = {}
    for k, v in d.items():
        key = "%s.%s" % (prefix, k) if prefix else k
        if isinstance(v, dict):
            out.update(flat(v, key))
        else:
            out[key] = v
    return out


def main(before_path, after_path):
    before = flat(json.load(open(before_path)))
    after = flat(json.load(open(after_path)))
    keys = sorted(set(before) | set(after))
    rows = []
    for k in keys:
        b, a = before.get(k), after.get(k)
        if isinstance(b, dict) or isinstance(a, dict):
            continue  # doctor_findings_by_code compared separately
        if b is None or a is None or b == a:
            arrow, verdict = "=", "unchanged/new"
        elif k in BETTER_UP:
            arrow, verdict = ("↑", "better") if a > b else ("↓", "worse")
        elif k in BETTER_DOWN:
            arrow, verdict = ("↓", "better") if a < b else ("↑", "worse")
        else:
            arrow, verdict = "=", "informational"
        rows.append((k, b, a, arrow, verdict))
    print("| metric | before | after | delta | verdict |")
    print("|---|---|---|---|---|")
    for k, b, a, arrow, verdict in rows:
        print("| `%s` | %s | %s | %s | %s |" % (k, b, a, arrow, verdict))
    bf = (json.load(open(before_path)).get("hygiene", {})
          .get("doctor_findings_by_code", {}))
    af = (json.load(open(after_path)).get("hygiene", {})
          .get("doctor_findings_by_code", {}))
    print("\n| doctor code | before | after |")
    print("|---|---|---|")
    for code in sorted(set(bf) | set(af)):
        print("| `%s` | %s | %s |" % (code, bf.get(code, 0), af.get(code, 0)))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
```

- [ ] **Step 4: Write test_bench.py**

```python
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
ENGINE = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")


class BenchSmokeTest(unittest.TestCase):
    def test_bench_runs_and_emits_valid_json(self):
        out_dir = tempfile.mkdtemp(prefix="bench-out-")
        try:
            proc = subprocess.run(
                [sys.executable, os.path.join(HERE, "bench.py"),
                 "--engine", ENGINE, "--label", "smoke", "--out", out_dir],
                capture_output=True, text=True, timeout=600, cwd=HERE)
            self.assertEqual(proc.returncode, 0, proc.stderr[-500:])
            path = os.path.join(out_dir, "bench-smoke.json")
            self.assertTrue(os.path.isfile(path))
            data = json.load(open(path))
            for section in ("retrieval", "context", "topology", "hygiene",
                            "guard"):
                self.assertIn(section, data)
            self.assertTrue(data["build_steps"])
        finally:
            shutil.rmtree(out_dir, ignore_errors=True)

    def test_bench_deterministic(self):
        outs = []
        for i in (1, 2):
            out_dir = tempfile.mkdtemp(prefix="bench-det-%d-" % i)
            try:
                subprocess.run(
                    [sys.executable, os.path.join(HERE, "bench.py"),
                     "--engine", ENGINE, "--label", "det%d" % i,
                     "--out", out_dir],
                    capture_output=True, text=True, timeout=600, cwd=HERE,
                    check=True)
                outs.append(json.load(open(
                    os.path.join(out_dir, "bench-det%d.json" % i))))
            finally:
                shutil.rmtree(out_dir, ignore_errors=True)
        a, b = outs
        for section in ("retrieval", "context", "topology", "guard"):
            self.assertEqual(a[section], b[section],
                             "non-deterministic metrics in %s" % section)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 5: Run the smoke tests**

Run: `cd /Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness && python3 -m unittest discover -s evals/benchmarks -p "test_*.py" -v 2>&1 | tail -6`
Expected: `OK` (2 tests; each runs the full bench end-to-end, ~1–3 min total).

- [ ] **Step 6: Full regression + commit**

```bash
python3 -m unittest discover -s skill/llm-wiki/scripts/tests 2>&1 | tail -1
python3 evals/deterministic/run_gates.py 2>&1 | grep -c PASS
git add evals/benchmarks/
git commit -m "bench: metrics, bench.py runner, compare.py — deterministic before/after harness"
```

---

### Task 5: Execute before/after benchmark; commit results

**Files:**
- Create: `evals/benchmarks/results/bench-base.json` (label `base`, engine = `bb7a51f` checkout)
- Create: `evals/benchmarks/results/bench-head.json` (label `head`, engine = HEAD)
- Create: `evals/benchmarks/results/compare.md` (compare.py output)

**Interfaces:**
- Consumes: Task 4's `bench.py`, `compare.py`.
- Produces: committed result artifacts referenced by the final report and EVALUATION.md.

- [ ] **Step 1: Extract the base engine**

```bash
cd /Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness
mkdir -p /tmp/llmwiki-base-engine
git archive bb7a51f skill/llm-wiki/scripts | tar -x -C /tmp/llmwiki-base-engine
```

- [ ] **Step 2: Run both benchmarks**

```bash
mkdir -p evals/benchmarks/results
python3 evals/benchmarks/bench.py --engine /tmp/llmwiki-base-engine/skill/llm-wiki/scripts/wiki.py --label base --out evals/benchmarks/results
python3 evals/benchmarks/bench.py --engine skill/llm-wiki/scripts/wiki.py --label head --out evals/benchmarks/results
python3 evals/benchmarks/compare.py evals/benchmarks/results/bench-base.json evals/benchmarks/results/bench-head.json | tee evals/benchmarks/results/compare.md
```

Expected: base run completes (guard probe records `near_dup_blocked: false`, near-dup page created); head run completes (`near_dup_blocked: true`, `orphan_page` in doctor findings). If a run errors, fix the harness (not the data) and re-run.

- [ ] **Step 3: Sanity-check the comparison**

Open `compare.md`: `topology.near_dup_pairs` must be lower on head (1 → 0); `hygiene.doctor_findings_by_code.orphan_page` present only on head; retrieval/context metrics unchanged or better. If retrieval regressed on head, that is a real bug in Tasks 1–2 — investigate before proceeding.

- [ ] **Step 4: Commit**

```bash
git add evals/benchmarks/results/
git commit -m "bench: before/after results — base bb7a51f vs head (guard + topology lint deltas)"
```

---

### Task 6: Two-arm semantic protocol + mechanical scorer

**Files:**
- Create: `evals/benchmarks/semantic_protocol.md`
- Create: `evals/benchmarks/score_semantic.py`
- Test: `evals/benchmarks/test_score_semantic.py`

**Interfaces:**
- Consumes: the 8 standard scenarios from `skill/llm-wiki/references/EVALUATION.md` (direct recall, synthesis, correction, historical, scope, conflict, unanswerable, isolation); arm answers arrive as JSONL rows `{"arm": "grep"|"wiki", "scenario": str, "answerable": bool, "answer": str, "citations": ["file:line-or-claim-id"], "effort_notes": str}`.
- Produces: `score_semantic.main(jsonl_path, corpus_dir, wiki_root=None) -> {"rows": [...], "summary": {...}}`; summary keys: `citation_validity` (all citations resolve to existing file/claim), `correction_correct` (7d current), `historical_correct` (30d + memo cite), `conflict_both_sides` (3 and 5 present), `refusal_correct` (unanswerable refused), per arm.

- [ ] **Step 1: Write semantic_protocol.md**

```markdown
# Two-arm semantic protocol (PRD §60 baseline comparison)

Arms answer the same 8 scenarios over the same golden-corpus copy:

## Arm A — without the skill (plain markdown + grep)
You are given a project directory (evals/golden-corpus copy). Use ONLY shell
text search (rg/grep) and file reading to answer. For each scenario, output
one JSONL row: {"arm":"grep","scenario":"<id>","answerable":bool,"answer":
"<answer>","citations":["<relpath>:<line>"],"effort_notes":"<searches run>"}.
Cite exact file and line for every factual claim. If the corpus cannot
answer, set answerable=false and say what is missing. Do not guess.

## Arm B — with the skill (wiki-built project)
You are given the same corpus WITH a built .llm-wiki/ (claims compiled and
corroborated). Use ONLY these read commands (run from the project root):
  python3 <engine> query-prepare --question "..." [--as-of YYYY-MM-DD]
  python3 <engine> context-pack --task "..." --budget 6000
  python3 <engine> page-show --artifact wiki/<kind>/<slug>.md
Answer from query-prepare matches; cite claim ids. Same JSONL shape with
"arm":"wiki".

## Scenarios (ids fixed)
1 direct-recall — Which database does the order service use?
2 synthesis — Why do we use transactional outbox?
3 correction — What is the current attribution window? (expect 7d)
4 historical — What was the attribution window before September 2026?
  (expect 30d, cite the memo)
5 scope — Does staging use the same attribution window as prod?
6 conflict — How many retries are configured? (must present both 3 and 5)
7 unanswerable — What is our 2028 pricing plan? (must refuse)
8 isolation — (two-corpus check; scorer verifies no cross citations)

Scoring is mechanical (score_semantic.py): citations must resolve; expected
values must appear; refusals must be explicit. No LLM judgment calls.
```

- [ ] **Step 2: Write score_semantic.py**

```python
#!/usr/bin/env python3
"""Mechanical scorer for two-arm semantic results (no LLM judgment)."""
import glob
import json
import os
import re
import sys


def _cite_ok(cite, corpus_dir, wiki_root):
    if cite.startswith("claim_"):
        return wiki_root and bool(glob.glob(
            os.path.join(wiki_root, ".llm-wiki", "claims", cite + ".json")))
    path = cite.split(":")[0]
    return os.path.isfile(os.path.join(corpus_dir, path))


def main(jsonl_path, corpus_dir, wiki_root=None):
    rows = [json.loads(l) for l in open(jsonl_path) if l.strip()]
    out = []
    for r in rows:
        cites_ok = all(_cite_ok(c, corpus_dir, wiki_root) for c in r.get("citations", []))
        a = r.get("answer", "")
        checks = {"citation_validity": cites_ok}
        sc = r.get("scenario", "")
        if sc == "correction":
            checks["value_correct"] = bool(re.search(r"\b7\s*d(ays)?\b", a))
        elif sc == "historical":
            checks["value_correct"] = bool(re.search(r"\b30\s*d(ays)?\b", a))
        elif sc == "conflict":
            checks["both_sides"] = bool(re.search(r"\b3\b", a) and re.search(r"\b5\b", a))
        elif sc == "unanswerable":
            checks["refused"] = (not r.get("answerable")) or \
                bool(re.search(r"cannot|not (?:documented|available|answerable)|no evidence", a, re.I))
        out.append({**r, "checks": checks, "pass": all(checks.values())})
    summary = {}
    for arm in ("grep", "wiki"):
        arm_rows = [r for r in out if r.get("arm") == arm]
        if arm_rows:
            summary[arm] = {
                "n": len(arm_rows),
                "pass_rate": round(sum(1 for r in arm_rows if r["pass"]) / len(arm_rows), 3),
                "by_check": {k: sum(1 for r in arm_rows if r["checks"].get(k))
                             for k in ("citation_validity", "value_correct",
                                       "both_sides", "refused")},
            }
    return {"rows": out, "summary": summary}


if __name__ == "__main__":
    print(json.dumps(main(sys.argv[1], sys.argv[2],
                          sys.argv[3] if len(sys.argv) > 3 else None), indent=2))
```

- [ ] **Step 3: Write test_score_semantic.py**

```python
import json
import os
import tempfile
import unittest

import score_semantic


class ScorerTest(unittest.TestCase):
    def _run(self, rows, wiki_root=None):
        d = tempfile.mkdtemp()
        jl = os.path.join(d, "r.jsonl")
        with open(jl, "w") as f:
            for r in rows:
                f.write(json.dumps(r) + "\n")
        corpus = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                              "..", "golden-corpus")
        return score_semantic.main(jl, corpus, wiki_root)

    def test_citation_validity_and_values(self):
        res = self._run([
            {"arm": "grep", "scenario": "correction", "answerable": True,
             "answer": "The window is 7 days.", "citations": ["docs/adr-019-attribution.md:12"]},
            {"arm": "grep", "scenario": "correction", "answerable": True,
             "answer": "The window is 7 days.", "citations": ["docs/nope.md:1"]},
        ])
        self.assertTrue(res["rows"][0]["pass"])
        self.assertFalse(res["rows"][1]["pass"])
        self.assertEqual(res["summary"]["grep"]["by_check"]["citation_validity"], 1)

    def test_refusal_and_conflict(self):
        res = self._run([
            {"arm": "wiki", "scenario": "unanswerable", "answerable": False,
             "answer": "not documented", "citations": []},
            {"arm": "wiki", "scenario": "conflict", "answerable": True,
             "answer": "3 in one doc, 5 in another", "citations": []},
        ])
        self.assertTrue(res["rows"][0]["pass"])
        self.assertTrue(res["rows"][1]["checks"]["both_sides"])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 4: Run + commit**

```bash
python3 -m unittest discover -s evals/benchmarks -p "test_score*.py" 2>&1 | tail -2
git add evals/benchmarks/semantic_protocol.md evals/benchmarks/score_semantic.py evals/benchmarks/test_score_semantic.py
git commit -m "bench: two-arm semantic protocol (§60) + mechanical scorer"
```

---

### Task 7: Docs wiring (SKILL, COMMANDS, EVALUATION, wiki-write)

**Files:**
- Modify: `skill/llm-wiki/references/COMMANDS.md` (write-page `--allow-new`, new doctor codes, `similar_existing`/`extend-first` on page-targets)
- Modify: `skill/llm-wiki/references/EVALUATION.md` (benchmark layer section, two-source rule note)
- Modify: `skill/llm-wiki/SKILL.md` (one row in the router table if write guidance changes; red-flag line about near-duplicates)
- Modify: `skill/wiki-write/SKILL.md` (extend-first rule)

**Interfaces:**
- Consumes: exact behaviors from Tasks 1–2 (`--allow-new`, codes `orphan_page`/`dead_end_page`/`section_index_unlinked`, `similar_existing` field, `--min-claims/--min-sources` defaults).
- Produces: documentation consistent with implemented behavior (reviewer checks every documented flag exists in `wiki.py --help` output).

- [ ] **Step 1: Update COMMANDS.md** — add to the write-page section:

```markdown
`write-page` refuses to create a new page that near-duplicates an existing
one (token-Jaccard ≥ 0.6 on title/subject): extend the twin instead, or pass
`--allow-new` to force creation (the page records `forced_new: true`).
`page-targets` marks such targets `routing: extend-first` with a
`similar_existing` list.
```

and to the doctor section:

```markdown
Navigation lint: `orphan_page` (page missing from its section index),
`dead_end_page` (no claim or page links), `section_index_unlinked` (index
missing from overview). Warnings — visible debt, never a verify failure.
```

- [ ] **Step 2: Update EVALUATION.md** — append a section:

```markdown
## Benchmark layer (deterministic, no LLM)

`python3 evals/benchmarks/bench.py --engine <wiki.py> --label <name>` builds
the planted wiki end-to-end and measures retrieval (precision/recall/MRR vs
gold), context efficiency (pack tokens vs budget, compression), navigation
topology (orphans, dead ends, near-dup pairs), and the two-source rule
(corroborated vs waiting targets). `compare.py` diffs two runs — run it
against the base revision before merging engine changes that touch pages,
query, or context. Page corroboration defaults: `--min-claims 2
--min-sources 2` (the two-source rule: one source is an anecdote, two are
knowledge).
```

- [ ] **Step 3: Update SKILL.md** — add one red-flag bullet: "You are about to create a new page when `page-targets` said `routing: extend-first` — extend the twin or pass `--allow-new` deliberately."

- [ ] **Step 4: Update skill/wiki-write/SKILL.md** — add the extend-first rule with one line of rationale (near-duplicate pages are the default failure mode) and the 1–3 inlink guidance (every page must be reachable from its section index and link onward).

- [ ] **Step 5: Verify consistency + commit**

```bash
python3 skill/llm-wiki/scripts/wiki.py write-page --help 2>&1 | grep -q "allow-new" && echo flag-ok
grep -q "orphan_page" skill/llm-wiki/references/COMMANDS.md && echo doc-ok
git add -u && git commit -m "docs: wire extend-don't-create guard, navigation lint, and benchmark layer into skill references"
```

---

### Task 8: Full regression + final state

- [ ] **Step 1:** `python3 -m unittest discover -s skill/llm-wiki/scripts/tests 2>&1 | grep -E "^(OK|FAILED|Ran)"` → OK, 0 failures.
- [ ] **Step 2:** `python3 -m unittest discover -s evals/benchmarks -p "test_*.py" 2>&1 | tail -1` → OK.
- [ ] **Step 3:** `python3 evals/deterministic/run_gates.py 2>&1 | grep -c PASS` → 9.
- [ ] **Step 4:** `git status --short` → clean; `git log --oneline main..HEAD` shows the phase commits.
- [ ] **Step 5:** Commit any remaining fixups: `git add -A && git commit -m "chore: skill-effectiveness phase final state"` (only if non-empty).

---

## Self-Review (completed during planning)

- **Spec coverage:** L1→Task 1; L2→Task 2; L3→measured by Tasks 4–5 (corroborated/waiting metrics; no engine change, per amended spec); L4→Tasks 3–5; L5→Task 6 (+Phase-5 execution); docs→Task 7; regression→Tasks 1/2 step 5 + Task 8.
- **Placeholders:** sketch blocks inside Task 3/4 code are explicitly marked and replaced with concrete code in the same task — implementers must use the concrete versions.
- **Type consistency:** `similar_pages(wiki, kind, subject, title, exclude_slug=None)` used identically in Task 2 code; bench JSON section names (`retrieval/context/topology/hygiene/guard`) consistent across bench.py, compare.py, and the report.
