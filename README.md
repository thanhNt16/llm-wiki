# llm-wiki — evidence-backed project memory for coding agents

Implementation of the OMP LLM Wiki PRD (`PRD.md` v0.3) as an installable
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

## The eight commands

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

All eight are thin facades over the shared `llm-wiki` skill, which ships the
`wiki.py` engine, schemas, and references. The agent does semantic work
(extraction, classification, prose) between deterministic script calls; every
canonical mutation is a staged, locked, revision-checked transaction — failed
runs never partially commit.

## Install

One line, no clone (clones to `~/.llm-wiki` and symlinks the skill into OMP's
user skills dir; re-running the same command updates the clone):

```bash
curl -fsSL https://raw.githubusercontent.com/thanhNt16/llm-wiki/main/install.sh | bash
```

From a checkout of this repo instead:

```bash
./install.sh              # OMP native: ~/.omp/agent/skills/{llm-wiki,wiki-*}
./install.sh --agents     # cross-runtime: ~/.agents/skills/{llm-wiki,wiki-*}
./install.sh --uninstall  # remove the symlinks
./install.sh --purge      # uninstall + delete the ~/.llm-wiki clone
```

Then in any project, say `wiki-init`, "ingest this doc", "compile the wiki",
"resume my project context", etc.

## Layout

```
skill/llm-wiki/      shared engine skill: wiki.py CLI + wikicore + schemas + references + tests
skill/wiki-*/        eight thin command skills (init, ingest, compile, query, context, review, doctor, visualize)
evals/golden-corpus/ fixture project with planted correction/conflict/scope cases
evals/deterministic/ PRD §58 release gates: python3 evals/deterministic/run_gates.py
evals/semantic/      agent-run scenarios + rubrics (PRD §57)
```

## Verify

```bash
python3 -m unittest discover -s skill/llm-wiki/scripts/tests   # unit tests
python3 evals/deterministic/run_gates.py                       # 9 release gates
./install.sh && omp -p --no-session 'Do you have skills named llm-wiki and wiki-init?'
```

Requirements: Python ≥3.9 (stdlib only), OMP v18+ for skill discovery.
