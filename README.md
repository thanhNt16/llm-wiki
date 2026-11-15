# llm-wiki — evidence-backed project memory for coding agents

Implementation of the OMP LLM Wiki PRD (`docs/PRD.md` v0.3) as an installable
[Agent Skill](https://agentskills.io) for [Oh My Pi](https://omp.dev) (OMP).

## What it does

Turns documents, configs, repos, meeting notes, and session transcripts into an
evidence-backed knowledge model:

```
evidence (verbatim, immutable) → claims (structured, cited) →
decisions (what the project chose) → derived views (wiki pages, context packs)
```

with correction propagation: when new evidence corrects old knowledge, every
dependent artifact is invalidated and rebuilt — history stays queryable.

## The nine commands

Each command is its own skill — say the name (or describe the intent) and OMP
loads it:

| Skill | Job |
|---|---|
| `wiki-init` | scaffold `.llm-wiki/` in a project (idempotent, non-destructive) |
| `wiki-ingest` | preserve + normalize evidence (never accepts claims) |
| `wiki-compile` | extract candidate claims, reconcile against knowledge, invalidate dependents |
| `wiki-query` | read-only Q&A with citations, scope, history, conflict surfacing |
| `wiki-context` | bounded, budgeted context pack + receipt; `--resume` briefing |
| `wiki-review` | human review of contradictions/candidates (transactional actions) |
| `wiki-doctor` | static + semantic diagnostics |
| `wiki-visualize` | 3D knowledge-graph explorer UI (React/Three.js, `graph-data` payload) |
| `wiki-write` | claims → prose: synthesizes readable `wiki/` pages from accepted knowledge |

All nine are thin facades over the shared `llm-wiki` skill (schemas +
references) and invoke the `llm-wiki` binary on `PATH` — a Rust engine.
The Python engine at `skill/llm-wiki/scripts/wiki.py` remains as a
dev-reference (parity oracle for the P7 benchmark), runnable from a repo
checkout. The agent does semantic work (extraction, classification,
prose) between deterministic engine calls; every canonical mutation is a
staged, locked, revision-checked transaction — failed runs never
partially commit.

## Install

One line, no clone (fetches the Rust binary release to `~/.local/bin/llm-wiki`
and installs the embedded skill set into OMP's user skills dir):

```bash
curl -fsSL https://raw.githubusercontent.com/thanhNt16/llm-wiki/main/install.sh | bash
```

From a checkout of this repo instead:

```bash
./install.sh              # ~/.local/bin/llm-wiki + ~/.omp/agent/skills/{llm-wiki,wiki-*}
./install.sh --agents     # cross-runtime: ~/.agents/skills/{llm-wiki,wiki-*}
./install.sh --cellockai  # CellockAI extension: ~/.cellockai/skills/{llm-wiki,wiki-*}
./install.sh --uninstall  # remove installed skills
./install.sh --purge      # uninstall + delete the ~/.llm-wiki clone
```

`LLM_WIKI_RELEASE=<tag>` pins a specific release;
`LLM_WIKI_BINARY_TARBALL=<path>` installs a locally built release tarball.
The Python engine is a dev-reference, not an install path — run it from
this checkout (`python3 skill/llm-wiki/scripts/wiki.py <cmd>`).

Then in any project, say `wiki-init`, "ingest this doc", "compile the wiki",
"resume my project context", etc.

## Layout

```
rust/crates/llm-wiki/ Rust engine (canonical): CLI, wikicore, embedded skills
skill/llm-wiki/      shared skill: schemas + references; scripts/ = Python
                     engine, dev-reference only (parity oracle, not installed)
skill/wiki-*/        nine thin command skills (init, ingest, compile, query, context, review, doctor, visualize, write)
evals/_engine.py     shared engine resolver (-e rust|python) for every lane
evals/deterministic/ PRD §58 release gates: run_gates.py -e rust
evals/survival/      planted-fact recall by position (early/mid/late/deep)
                     + trap/correction lane; run_survival.py -e rust, then
                     -e python — second run asserts cross-engine parity
evals/rust-vs-python/ P7 suite: quality (wiki vs baseline), perf, install-ux
                     lanes + significance + report: generate_report.py
evals/e2e/           end-to-end OMP scenarios; compare/ = state + semantic
                     parity between engines (state_parity.py, report.md)
evals/query-battery/ query-contract battery vs expected.json
evals/retrieval/     ranked-retrieval gold-set lane
evals/perf/          perf corpus + gates (run_perf_gates.py -e rust)
evals/smoke/         install smoke lane (run_smoke.py)
evals/benchmark/     PRD §60 wiki-vs-baseline lane comparison
evals/golden-corpus/ fixture project with planted correction/conflict/scope cases
evals/semantic/      agent-run scenarios + rubrics (PRD §57)
.github/workflows/   release-rust.yml: build + release tarball on tag

## Verify

```bash
(cd rust && cargo build --release && cargo test)                   # rust engine
python3 -m unittest discover -s skill/llm-wiki/scripts/tests       # python dev-reference unit tests
python3 evals/deterministic/run_gates.py --engine rust             # 10 release gates, rust engine
python3 evals/deterministic/run_gates.py --engine python           # same gates vs the dev-reference
python3 evals/survival/run_survival.py -e rust               # recall + traps + stability
python3 evals/survival/run_survival.py -e python             # …then assert cross-engine parity
python3 evals/rust-vs-python/generate_report.py               # quality/perf report (run lanes first)
python3 evals/smoke/run_smoke.py -e rust                      # install smoke
./install.sh && omp -p --no-session 'Do you have skills named llm-wiki and wiki-init?'
```

Requirements: the `llm-wiki` binary on PATH (install.sh or a release
tarball; `cargo build --release` for a local build), OMP v18+ for skill
discovery. Python ≥3.9 (stdlib only) is needed only for the dev-reference
engine and the eval harnesses.
