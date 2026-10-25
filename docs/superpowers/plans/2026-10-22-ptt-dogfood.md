# ptt Real-Corpus Dogfooding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a permanent `ptt/.llm-wiki` from the real 439-file corpus, instrumented end-to-end, producing triaged correctness/scale findings plus real-corpus eval results.

**Architecture:** A thin instrumented wrapper (`evals/dogfood/run.py`) times every `wiki.py` invocation, captures stdout/stderr to per-step logs, and appends one JSONL row per call. The run proceeds through six phases (init → text ingest → vision ingest → compile → pages → query/report). Every anomaly is triaged `fix-now` / `eval-case` / `wontfix`; engine fixes land as separate commits on `topic-map-compile` with regression tests.

**Tech Stack:** Python ≥3.9 stdlib only (`subprocess`, `time.monotonic`, `json`); unittest; the `wiki.py` engine at `.worktrees/topic-map/skill/llm-wiki/scripts/wiki.py`.

## Global Constraints

- Engine under test: `WIKI=/Users/harry/Desktop/personal/llm-wiki/.worktrees/topic-map/skill/llm-wiki/scripts/wiki.py` (topic-map-compile HEAD). Never the installed skill copy.
- Wiki root: `ROOT=/Users/harry/Desktop/cellock/ptt` — creates `ptt/.llm-wiki` permanently.
- Corpus: `/Users/harry/Desktop/cellock/ptt/docs` — all 439 files.
- Python ≥3.9, stdlib only. No new dependencies.
- `wikicore/` must NOT be modified for instrumentation.
- Images are ingested individually with `--normalized-content` (agent-vision); do NOT use `ingest --dir` (it would store images as undescribed binaries — webp/png are not in `SKIP_EXTS`).
- Engine fixes never happen inside the instrumented run; they are separate commits with regression tests.
- Record `git -C /Users/harry/Desktop/cellock/ptt rev-parse HEAD` in the run report header (corpus drift guard).
- Add `.llm-wiki/` to `/Users/harry/Desktop/cellock/ptt/.git/info/exclude` — never modify ptt tracked files.

---

### Task 1: Instrumented runner `evals/dogfood/run.py`

**Files:**
- Create: `evals/dogfood/run.py`
- Test: `evals/dogfood/test_run.py`
- Create: `evals/dogfood/README.md`

**Interfaces:**
- Consumes: env/flags `--wiki` (path to wiki.py), `--root` (project root), `--runs` (run output dir)
- Produces: `run.py --wiki W --root R --runs D -- <wiki args>` → writes `D/NNN-<slug>.log`, appends `D/run.jsonl` row `{seq, cmd, wall_ms, exit, log, receipt}`, returns the wrapped exit code. Later tasks (2–9) invoke wiki commands only through this wrapper.

- [ ] **Step 1: Write the failing test**

```python
# evals/dogfood/test_run.py
import json, os, subprocess, sys, tempfile, unittest

RUN = os.path.join(os.path.dirname(__file__), "run.py")

STUB = """import sys
print('{\\\"ok\\\": true, \\\"seq_marker\\\": %d}' % len(sys.argv))
sys.exit(int(sys.argv[1]) if sys.argv[1].isdigit() else 0)
"""

class TestRunner(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.wiki = os.path.join(self.tmp, "stub.py")
        open(self.wiki, "w").write(STUB)
        self.root = os.path.join(self.tmp, "proj"); os.makedirs(self.root)
        self.runs = os.path.join(self.tmp, "runs")

    def _go(self, *argv):
        return subprocess.run(
            [sys.executable, RUN, "--wiki", self.wiki, "--root", self.root,
             "--runs", self.runs, "--", *argv],
            capture_output=True, text=True)

    def test_logs_row_and_returns_exit(self):
        r = self._go("status")
        self.assertEqual(r.returncode, 0)
        rows = [json.loads(l) for l in open(os.path.join(self.runs, "run.jsonl"))]
        self.assertEqual(rows[0]["cmd"], ["status"])
        self.assertEqual(rows[0]["exit"], 0)
        self.assertIn("wall_ms", rows[0])
        self.assertTrue(os.path.isfile(
            os.path.join(self.runs, rows[0]["log"])))

    def test_failure_exit_propagates_and_is_logged(self):
        r = self._go("3", "compile")
        self.assertEqual(r.returncode, 3)
        rows = [json.loads(l) for l in open(os.path.join(self.runs, "run.jsonl"))]
        self.assertEqual(rows[0]["exit"], 3)
        self.assertEqual(rows[0]["seq"], 1)

    def test_seq_increments(self):
        self._go("status"); self._go("verify")
        rows = [json.loads(l) for l in open(os.path.join(self.runs, "run.jsonl"))]
        self.assertEqual([r["seq"] for r in rows], [1, 2])
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/harry/Desktop/personal/llm-wiki && python3 -m unittest evals.dogfood.test_run -v`
Expected: FAIL — `run.py` does not exist (`No such file or directory`).

- [ ] **Step 3: Write minimal implementation**

```python
#!/usr/bin/env python3
"""run.py — instrumented wrapper for wiki.py dogfood runs.

Usage: run.py --wiki <wiki.py> --root <project> --runs <dir> -- <wiki args>
Logs stdout+stderr to <runs>/NNN-<cmd>.log, appends one row to run.jsonl,
and propagates the wrapped command's exit code.
"""
import argparse, json, os, re, subprocess, sys, time


def _next_seq(runs_dir: str) -> int:
    log = os.path.join(runs_dir, "run.jsonl")
    if not os.path.exists(log):
        return 1
    with open(log) as f:
        return sum(1 for _ in f) + 1


def _slug(argv) -> str:
    return re.sub(r"[^a-z0-9]+", "-", "-".join(argv)).strip("-")[:60] or "run"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--wiki", required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("--runs", required=True)
    ap.add_argument("rest", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    argv = [x for x in a.rest if x != "--"]
    if not argv:
        ap.error("no wiki command after --")

    os.makedirs(a.runs, exist_ok=True)
    seq = _next_seq(a.runs)
    log_name = f"{seq:03d}-{_slug(argv)}.log"
    log_path = os.path.join(a.runs, log_name)

    t0 = time.monotonic()
    p = subprocess.run([sys.executable, a.wiki, "--root", a.root, *argv],
                       capture_output=True, text=True)
    wall_ms = round((time.monotonic() - t0) * 1000)

    with open(log_path, "w") as f:
        f.write(p.stdout + p.stderr)

    row = {"seq": seq, "cmd": argv, "wall_ms": wall_ms,
           "exit": p.returncode, "log": log_name}
    try:
        receipt = json.loads(p.stdout.strip().splitlines()[-1])
        row["receipt"] = receipt
    except Exception:
        pass

    with open(os.path.join(a.runs, "run.jsonl"), "a") as f:
        f.write(json.dumps(row) + "\n")

    print(f"[{seq:03d}] {' '.join(argv)} -> exit {p.returncode} ({wall_ms}ms) {log_name}")
    return p.returncode


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/harry/Desktop/personal/llm-wiki && python3 -m unittest evals.dogfood.test_run -v`
Expected: PASS — 3 tests.

- [ ] **Step 5: Write README.md**

```markdown
# evals/dogfood — instrumented real-corpus runs

`run.py` wraps `wiki.py`: per-call wall time, exit code, log capture, receipt
echo into `run.jsonl`. One run = one `--runs` dir.

    python3 evals/dogfood/run.py --wiki <wiki.py> --root <proj> \
        --runs evals/dogfood/runs/<ts> -- status

`triager.md` classifies anomalies: fix-now | eval-case | wontfix.
`questions-ptt.txt` holds the golden question set for the ptt corpus.
```

- [ ] **Step 6: Commit**

```bash
cd /Users/harry/Desktop/personal/llm-wiki
git add evals/dogfood/run.py evals/dogfood/test_run.py evals/dogfood/README.md
git commit -m "feat(evals): dogfood run.py instrumented wiki wrapper"
```

---

### Task 2: Triage table + ptt repo prep

**Files:**
- Create: `evals/dogfood/triager.md`
- Create: `evals/dogfood/questions-ptt.txt` (skeleton — populated during Task 4)
- Modify: `/Users/harry/Desktop/cellock/ptt/.git/info/exclude` (append `.llm-wiki/`)

**Interfaces:**
- Consumes: nothing
- Produces: `triager.md` classification table used by Task 9; `questions-ptt.txt` consumed by Task 7

- [ ] **Step 1: Write triager.md**

```markdown
# Triage table — dogfood findings

Every unexpected observation gets exactly one classification.

| Signal | Classification | Action |
|---|---|---|
| wiki.py exit != 0 on valid input | fix-now | minimal repro, engine commit + regression test |
| verify / doctor red after valid sequence | fix-now | same |
| wall_ms > 30s for a single-source operation | eval-case | record; scale-finding in report |
| review queue >20% manual items | eval-case | reconciliation-gap finding |
| query answers with citation resolving to wrong source | fix-now | citation gate violation |
| unanswerable question answered confidently | fix-now | refusal gate violation |
| cosmetic/log noise only | wontfix | logged in report rationale |
```

- [ ] **Step 2: Create questions-ptt.txt skeleton**

```text
# Golden questions for ptt corpus — fill during ingest (Task 4).
# Format: question | expected rubric | kind
# Kinds: direct-recall, cross-source, correction, historical, scope,
#        conflict, unanswerable, vision
```

- [ ] **Step 3: Prep ptt repo**

```bash
cd /Users/harry/Desktop/cellock/ptt
echo ".llm-wiki/" >> .git/info/exclude
git rev-parse HEAD   # record this sha for the run report
git status --short | head -5   # note pre-existing dirty state
```

- [ ] **Step 4: Init wiki + verify**

```bash
WIKI=/Users/harry/Desktop/personal/llm-wiki/.worktrees/topic-map/skill/llm-wiki/scripts/wiki.py
RUNS=/Users/harry/Desktop/personal/llm-wiki/evals/dogfood/runs/$(date +%Y%m%dT%H%M)
cd /Users/harry/Desktop/personal/llm-wiki
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt --runs $RUNS -- init --name ptt
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt --runs $RUNS -- status
```

Expected: init exit 0; status shows `initialized: true`, `revision: 0`, empty counts.

- [ ] **Step 5: Commit**

```bash
cd /Users/harry/Desktop/personal/llm-wiki
git add evals/dogfood/triager.md evals/dogfood/questions-ptt.txt
git commit -m "feat(evals): dogfood triage table and ptt question skeleton"
```

---

### Task 3: Text ingest — per-file loop over md/sql/html

**Files:**
- Modify: `evals/dogfood/questions-ptt.txt` (notes added opportunistically)
- Uses: `run.py` from Task 1; `$WIKI`, `$ROOT`, `$RUNS` from Task 2

**Interfaces:**
- Consumes: `run.py --wiki --root --runs` contract
- Produces: `ptt/.llm-wiki/sources/` populated for all text files; per-file timing rows in run.jsonl; candidate golden questions noted

- [ ] **Step 1: Enumerate text files deterministically**

```bash
cd /Users/harry/Desktop/cellock/ptt/docs
find . -type f \( -name "*.md" -o -name "*.sql" -o -name "*.html" -o -name "*.txt" \) \
  ! -path "./.worktrees/*" | sort > /tmp/ptt-text-files.txt
wc -l /tmp/ptt-text-files.txt   # expect ~120
```

- [ ] **Step 2: Ingest each file through the runner**

```bash
cd /Users/harry/Desktop/personal/llm-wiki
while IFS= read -r f; do
  python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
    --runs $RUNS -- ingest --file "/Users/harry/Desktop/cellock/ptt/docs/$f" \
    || echo "FAILED: $f" >> $RUNS/failures.txt
done < /tmp/ptt-text-files.txt
```

Expected: each exits 0; failures logged not fatal. Skim each source as it lands — when a doc contains a correction/scope/conflict/unanswerable pattern, append a question line to `evals/dogfood/questions-ptt.txt` immediately (fresh knowledge = best questions).

- [ ] **Step 3: Check status + failure log**

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt --runs $RUNS -- status
cat $RUNS/failures.txt 2>/dev/null || echo "no ingest failures"
```

Expected: `counts.sources` ≈ file count; zero or triaged failures.

- [ ] **Step 4: Summarize timings**

```bash
python3 -c "
import json
rows=[json.loads(l) for l in open('$RUNS/run.jsonl')]
ing=[r for r in rows if r['cmd'][0]=='ingest']
print('ingest calls:',len(ing),'total ms:',sum(r['wall_ms'] for r in ing),
      'max ms:',max(r['wall_ms'] for r in ing),'nonzero exit:',sum(1 for r in ing if r['exit']))"
```

Record output for the report.

- [ ] **Step 5: Commit questions + any notes**

```bash
git add evals/dogfood/questions-ptt.txt
git commit -m "evals(dogfood): ptt golden questions from text-ingest pass"
```

---

### Task 4: Vision lane — batched image ingest

**Files:**
- Uses: `run.py`; `questions-ptt.txt`
- Creates: `/tmp/ptt-vision/` extraction dir (scratch, not committed)

**Interfaces:**
- Consumes: image list; produces extraction `.md` per image → `ingest --file <img> --normalized-content <md> --parser-name agent-vision`
- Produces: image sources with description-backed extraction; vision-kind golden questions

- [ ] **Step 1: Enumerate images**

```bash
find /Users/harry/Desktop/cellock/ptt/docs -type f \( -name "*.webp" -o -name "*.png" \) \
  ! -path "*/.worktrees/*" | sort > /tmp/ptt-images.txt
wc -l /tmp/ptt-images.txt   # expect ~320
mkdir -p /tmp/ptt-vision
```

- [ ] **Step 2: Batched vision loop (batches of 25)**

For each batch, for each image: view the file, write a description markdown to `/tmp/ptt-vision/<basename>.md` following the per-image-type guidance (UI screenshot → what screen/state/errors shown; diagram → boxes+arrows semantics; log shot → key lines), then:

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- ingest --file "$IMG" \
  --normalized-content "/tmp/ptt-vision/$(basename $IMG).md" \
  --parser-name agent-vision
```

Failure rule: one bad image → log path to `$RUNS/failures.txt`, continue. End of each batch: `status` call through runner.

- [ ] **Step 3: Coverage check**

```bash
python3 -c "
import json,os
src='/Users/harry/Desktop/cellock/ptt/.llm-wiki/sources'
imgs=sum(1 for l in open('/tmp/ptt-images.txt'))
print('images:',imgs,'sources dirs:',len(os.listdir(src)))"
```

Expected: source count ≈ text files + image files minus dedup skips.

- [ ] **Step 4: Vision golden questions**

Append 3–5 questions answerable only via screenshot content (e.g. "what did the stc90 retest show for the QA console flow?") to `questions-ptt.txt` with kind `vision`.

- [ ] **Step 5: Commit**

```bash
git add evals/dogfood/questions-ptt.txt
git commit -m "evals(dogfood): vision-lane golden questions"
```

---

### Task 5: Compile — umbrella run + review stats

**Files:**
- Uses: `run.py`; receipts under `ptt/.llm-wiki/.state/`

**Interfaces:**
- Consumes: populated sources; produces `compile` receipt JSON, review queue snapshot
- Produces: compile wall times, candidate counts, auto-verdict rate, manual review item count

- [ ] **Step 1: Run compile umbrella**

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- compile
```

If exit != 0 → triager: hard failure is the finding; capture receipt/log, minimal repro, engine fix as separate commit on topic-map-compile, then `-- resume` (or re-run) through the runner.

- [ ] **Step 2: Snapshot review queue**

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- review list --all
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- verify
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- doctor
```

- [ ] **Step 3: Extract compile metrics**

```bash
python3 -c "
import json
rows=[json.loads(l) for l in open('$RUNS/run.jsonl')]
for r in rows:
    if r['cmd'][0] in ('compile','review','verify','doctor'):
        print(r['seq'], ' '.join(r['cmd']), r['exit'], r['wall_ms'],'ms')"
```

Expected: `verify` → `"ok": true`; `doctor` no red findings; record review-item counts.

- [ ] **Step 4: Triage review items**

If manual review items exist: work the queue (`review show`/`act`) per item kind. >20% manual rate → `eval-case` finding (reconciliation gap). Record per-item dispositions for the report.

- [ ] **Step 5: Commit notes** (if any findings written)

```bash
git add -A evals/dogfood && git commit -m "evals(dogfood): compile phase metrics + review dispositions" || true
```

---

### Task 6: Pages — targets, write, build

**Files:**
- Uses: `run.py`; `page-targets` output

- [ ] **Step 1: Page targets**

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt --runs $RUNS -- page-targets
```

- [ ] **Step 2: Write pages through runner**

For each target in the receipt (loop over the JSON output):

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- write-page --target <id> ...
```

(exact flag names from `python3 $WIKI write-page --help` at run time — record help output in the log dir first)

- [ ] **Step 3: Build + verify**

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt --runs $RUNS -- build-pages
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt --runs $RUNS -- verify
```

Expected: `ok: true`; count pages under `ptt/wiki/`.

---

### Task 7: Golden questions — query + context-pack + results.jsonl

**Files:**
- Uses: `evals/dogfood/questions-ptt.txt` (complete set)
- Modify: `evals/semantic/results.jsonl` (append real-corpus rows)

- [ ] **Step 1: Ensure question coverage**

`questions-ptt.txt` must contain ≥1 of each kind: direct-recall, cross-source, correction, historical, scope, conflict, unanswerable, vision. Fill gaps from corpus knowledge before proceeding.

- [ ] **Step 2: Answer each question through the engine**

```bash
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- query-prepare --question "<question>"
# then context-pack for the top hits:
python3 evals/dogfood/run.py --wiki $WIKI --root /Users/harry/Desktop/cellock/ptt \
  --runs $RUNS -- context-pack --task "<question>" --budget 6000
```

Answer from the pack only; record answer + citation ids.

- [ ] **Step 3: Append results**

One JSONL row per question into `evals/semantic/results.jsonl`:

```json
{"scenario": "ptt:<kind>", "timestamp": "<ISO>", "pass": true,
 "answer": "<short answer>", "citations": ["claim_..."],
 "rubric_notes": "ptt dogfood run <ts>"}
```

- [ ] **Step 4: Commit**

```bash
git add evals/semantic/results.jsonl evals/dogfood/questions-ptt.txt
git commit -m "evals(semantic): ptt real-corpus question results"
```

---

### Task 8: §60 baseline — same questions, no wiki

**Files:**
- Modify: run report (Task 9) — baseline section

- [ ] **Step 1: Answer the identical question set** against `ptt/docs` using only `rg` + a hand-written PROJECT.md-style summary. Record: which questions the baseline could answer, citation quality (file:line vs claim id), correction/historical handling failures.

- [ ] **Step 2: Score comparison table** for the report: recall / correction handling / historical accuracy / traceability / context efficiency / session continuation — wiki vs baseline per PRD §60.

No commit needed — output feeds Task 9's report.

---

### Task 9: Run report + triage findings

**Files:**
- Create: `docs/dogfood/2026-10-22-ptt-run.md` (llm-wiki repo)

- [ ] **Step 1: Generate timing summary**

```bash
python3 -c "
import json,collections
rows=[json.loads(l) for l in open('$RUNS/run.jsonl')]
by=collections.defaultdict(lambda:[0,0,0])
for r in rows:
    k=r['cmd'][0]; by[k][0]+=1; by[k][1]+=r['wall_ms']; by[k][2]=max(by[k][2],r['wall_ms'])
for k,(n,tot,mx) in sorted(by.items(),key=lambda x:-x[1][1]):
    print(f'{k:20s} n={n:4d} total={tot/1000:8.1f}s max={mx}ms')
print('failures:',sum(1 for r in rows if r['exit']))"
```

- [ ] **Step 2: Write report**

`docs/dogfood/2026-10-22-ptt-run.md` must contain: ptt HEAD sha, engine commit, corpus counts, the timing table, ingest failures list, compile metrics (candidates/source, auto-verdict %, manual review count), question results table, baseline comparison table, and the triaged findings list (each finding: signal, class, action taken or link to fixing commit).

- [ ] **Step 3: Commit**

```bash
mkdir -p docs/dogfood
git add docs/dogfood/2026-10-22-ptt-run.md evals/dogfood/runs/
git commit -m "evals(dogfood): ptt run report — instrumented findings"
```

---

### Task 10: Triage → fixes protocol

No code — the decision procedure:

- [ ] **Step 1:** For each `fix-now` finding: minimal repro script in `evals/dogfood/repro-<name>.sh`, engine fix + regression test committed on `topic-map-compile`, link commit sha into the report's finding row.
- [ ] **Step 2:** For each `eval-case`: add a deterministic gate or semantic scenario entry referencing the real-corpus evidence.
- [ ] **Step 3:** For each `wontfix`: one line in the report rationale.
- [ ] **Step 4:** Re-run `python3 -m unittest discover -s skill/llm-wiki/scripts/tests` + `python3 evals/deterministic/run_gates.py` after every engine fix; both must stay green.
- [ ] **Step 5:** Final commit per fix: `fix(engine): <finding> — found via ptt dogfood run`.
