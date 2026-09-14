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

## The seven commands

| Command | Job |
|---|---|
| `/wiki-init` | scaffold `.llm-wiki/` in a project (idempotent, non-destructive) |
| `/wiki-ingest` | preserve + normalize evidence (never accepts claims) |
| `/wiki-compile` | extract candidate claims, reconcile against knowledge, invalidate dependents |
| `/wiki-query` | read-only Q&A with citations, scope, history, conflict surfacing |
| `/wiki-context` | bounded, budgeted context pack + receipt; `--resume` briefing |
| `/wiki-review` | human review of contradictions/candidates (transactional actions) |
| `/wiki-doctor` | static + semantic diagnostics |

The agent does semantic work (extraction, classification, prose) between
deterministic script calls; every canonical mutation is a staged, locked,
revision-checked transaction — failed runs never partially commit.

## Install

```bash
./install.sh              # OMP native: ~/.omp/agent/skills/llm-wiki
./install.sh --agents     # cross-runtime: ~/.agents/skills/llm-wiki
./install.sh --uninstall
```

Then in any project, ask OMP to "initialize the llm wiki", "ingest this doc",
"compile the wiki", "resume my project context", etc.

## Layout

```
skill/llm-wiki/      the skill (SKILL.md facade + references + schemas + scripts)
skill/scripts/       wiki.py CLI + wikicore package + unit tests (stdlib only)
evals/golden-corpus/ fixture project with planted correction/conflict/scope cases
evals/deterministic/ PRD §58 release gates: python3 evals/deterministic/run_gates.py
evals/semantic/      agent-run scenarios + rubrics (PRD §57)
docs/superpowers/    design spec + implementation plan
```

## Verify

```bash
python3 -m unittest discover -s skill/scripts/tests    # 108 unit tests
python3 evals/deterministic/run_gates.py               # 9 release gates
./install.sh && omp -p --no-session 'Do you have a skill named llm-wiki?'
```

Requirements: Python ≥3.9 (stdlib only), OMP v18+ for skill discovery.
