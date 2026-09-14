# OMP LLM Wiki — Design Spec

**Date:** 2026-09-15
**Status:** Approved (built from PRD v0.3 + user decisions)
**Source PRD:** `PRD.md` (repo root)

---

## 1. Goal

Build a working, installed Agent Skill for Oh My Pi (OMP) that implements the PRD's
MVP: a local-first, evidence-backed project memory system with seven commands
(`/wiki-init`, `/wiki-ingest`, `/wiki-compile`, `/wiki-query`, `/wiki-context`,
`/wiki-review`, `/wiki-doctor`), transactional writes, dependency invalidation,
bounded context packs, and an evaluation suite.

## 2. Locked decisions

| Decision | Choice |
|---|---|
| Scope | Full PRD MVP (all 7 commands, PRD §63) |
| Deterministic scripts | Python 3.9+ **stdlib only** (no pip installs) |
| Skill packaging | **Single facade skill** `llm-wiki` (thin SKILL.md + references + schemas + scripts) |
| Semantic work | **In-session agent** (the host coding agent), never an external API |
| Canonical metadata format | **JSON** (PRD permits "YAML/JSON"); derived pages are Markdown + restricted flat YAML frontmatter |
| Approach | Deterministic core, guided agent (Approach A) |

## 3. Non-goals (PRD §62)

3D palace, hosted SaaS, mandatory vector/graph DB, autonomous skill promotion,
enterprise permissions, real-time collaboration, global personal memory, ontology
editor. PDF/DOCX/XLSX/PPTX are **preserved but not semantically extracted** by the
stdlib core; the extraction report records this honestly (PRD §13 — no silent
failure). If `markitdown` or similar is available on PATH, ingest may use it as an
optional parser and says so in the report.

## 4. Repository layout

```
llm-wiki/
├── PRD.md
├── README.md
├── docs/superpowers/
│   ├── specs/2026-09-15-omp-llm-wiki-design.md
│   └── plans/2026-09-15-omp-llm-wiki-mvp.md
├── skill/llm-wiki/            # the skill package (installed via symlink)
│   ├── SKILL.md
│   ├── references/
│   │   ├── ARCHITECTURE.md    # 4-layer model, authority, temporal, scope, independence
│   │   ├── COMMANDS.md        # per-command contracts & workflows
│   │   ├── RECONCILIATION.md  # relationship taxonomy, classification procedure
│   │   ├── EXTRACTION.md      # candidate claims, locators, extraction reports
│   │   ├── CONTEXT-PACKS.md   # budgets, priority, receipts, resume
│   │   ├── SECURITY.md        # injection boundary, secret policy
│   │   └── EVALUATION.md      # eval categories, corpus, gates
│   ├── schemas/               # JSON Schema (draft-07 subset, validated by wikicore)
│   │   ├── source-manifest.schema.json
│   │   ├── claim.schema.json
│   │   ├── decision.schema.json
│   │   ├── candidate-claim.schema.json
│   │   ├── extraction-report.schema.json
│   │   ├── context-receipt.schema.json
│   │   ├── run-receipt.schema.json
│   │   └── review-item.schema.json
│   └── scripts/
│       ├── wiki.py            # single CLI entrypoint (argparse subcommands)
│       ├── wikicore/          # python package (see §6)
│       └── tests/             # unittest suites (run from repo root or anywhere)
├── evals/
│   ├── golden-corpus/         # fixture project per PRD §56
│   ├── deterministic/         # release-gate eval scripts (PRD §58)
│   └── semantic/              # scenario briefs + rubrics (PRD §57, §59)
└── install.sh                 # symlink skill into OMP user skills dir
```

## 5. Wiki storage layout (created by `init` inside a target project)

As PRD §9, with JSON canonical files:

```
<project>/.llm-wiki/
├── wiki.json                  # config: project name, acceptance policy, budgets, secret policy
├── README.md
├── raw/{files,urls,repositories,sessions,text}/<hash-prefix>...   # immutable originals
├── sources/<source_id>/
│   ├── manifest.json          # identity, versions[], locators available, adapter, quality
│   ├── content.md             # normalized text (text-like adapters)
│   ├── extraction.json        # agent's candidate claims + extraction report
│   └── assets/                # preserved binaries/pages
├── claims/claim_<ulid>.json
├── decisions/decision_<ulid>.json
├── notes/
├── wiki/                      # DERIVED (rebuildable)
│   ├── index.md  overview.md
│   ├── concepts/  entities/  decisions/  procedures/  questions/  changes/
├── context/context_<ulid>.md  # DERIVED packs (+ context_<ulid>.receipt.json)
├── views/
└── .state/
    ├── manifest.json          # revision counter, skill_version, content hash
    ├── dependencies.json      # derived artifact -> [{kind,id,version}]
    ├── aliases.json           # source identity aliases (path/url -> source_id)
    ├── graph.json             # claim<->claim relations (supersedes, corroborates, conflicts)
    ├── operations.jsonl       # append-only audit of committed mutations
    ├── review-queue.jsonl     # open review items
    ├── locks/                 # flock files
    └── staging/<run_id>/      # transaction scratch
```

**Canonical:** `raw/ sources/ claims/ decisions/ notes/ .state/{manifest,aliases,graph,operations,review-queue}`.
**Derived:** `wiki/ context/ views/` + caches inside `.state`. Deleting derived dirs
must leave semantic state recoverable by recompile (PRD §10, §8.2).

## 6. Deterministic core (`wikicore`)

Modules (each ≤ ~300 lines, single responsibility):

- `ids.py` — ULID-style ids: 48-bit ms timestamp + 80-bit randomness, Crockford base32 → `claim_01JABC...`. `new(kind)`, `validate(s)`.
- `hashing.py` — `sha256_file`, `sha256_bytes`, `canonical_json(obj)` (sorted keys, compact separators) used for content addressing and revision hashes.
- `yamlite.py` — restricted frontmatter writer/reader: flat scalars + lists of strings only (`key: value`, `key:` + `  - item`). Used ONLY for derived Markdown pages. Canonical data never passes through it.
- `store.py` — path layout, `wiki.json` load/save, `.state/manifest.json` (revision counter + hash), aliases, graph. `current_revision()`, `bump_revision()`.
- `schema.py` — minimal JSON Schema validator (draft-07 subset: type, required, properties, enum, items, additionalProperties, pattern, minimum/maximum). Validates objects against `schemas/*.schema.json`. No external deps.
- `transaction.py` — PLAN/STAGE/VALIDATE/COMMIT: staging dir per run, `flock` on `.state/locks/commit.lock` for semantic commits, optimistic `base_revision` check (reject stale commit with exit code 409), atomic `os.replace` moves, run receipt emission, append to `operations.jsonl`, rollback = discard staging (canonical never touched before commit).
- `sources.py` — source identity (canonical origin key: abs path / normalized URL / session id), version dedup by sha256 (same bytes ⇒ no new version), raw preservation, manifest update, extraction report validation, pending-version listing.
- `claims.py` — claim/decision CRUD via transactions; statuses (`candidate accepted disputed superseded rejected provisional`); scope, valid_from/valid_to, recorded_at, authority, evidence with locators, `supersedes`, provenance `root_origin`/`derived_from`; independence computation (distinct root_origin count).
- `deps.py` — dependency graph: `register(artifact, deps)` where dep = `claim_X@N`, `source_Y@N`, `decision_Z@N`; `invalidate(changed)` → transitively mark stale; stale set persisted in `.state/dependencies.json`.
- `compile.py` — pending plan (uncompiled source versions), deterministic reconcile prep (identity match on `(subject, predicate, scope)`; temporal overlap check), auto-accept policy application, apply reconcile plan transactionally (accept/supersede/dispute/review), trigger invalidation.
- `contextpack.py` — selection by PRD §19 priority order under a hard token budget (estimate: `len(text)//4`), pack writer + receipt (PRD §20), `--resume` briefing and `--changes-since <context_id>` diff from operations log.
- `query.py` — read-only retrieval: keyword+scope+time filtering, scoring (subject/predicate/value token overlap; accepted > provisional; recency tiebreak), conflict surfacing, citation targets. Never writes.
- `review.py` — review-queue append/list/show; transactional actions (accept, reject, merge, mark-duplicate, mark-authoritative, mark-superseded, set-scope, set-validity, defer).
- `doctor.py` — static checks (schema validity, broken references, orphan sources, invalid ids, missing deps, corrupt/stale indexes, staging leftovers, lock issues) + semantic debt (unsupported accepted claims, unresolved contradictions, duplicates, missing scope/dates, stale decisions, low extraction coverage).
- `secrets.py` — secret heuristics (PEM headers, `AKIA...`, `ghp_`, `xoxb-`, `-----BEGIN`, high-entropy assignments, `.env` files, URL credentials) with policy `deny|warn|redact|allow` from `wiki.json`; redaction replaces values in normalized content and never copies secrets into logs.
- `cli.py` — argparse: `init ingest compile-plan stage-candidates reconcile-prepare reconcile-apply build-pages verify query-prepare context-pack review doctor status`. All output JSON on stdout (machine-readable for the agent), `--pretty` for humans.

**CLI exit codes:** 0 ok, 2 usage, 3 validation, 4 conflict/stale, 5 locked.

## 7. Command workflows (agent ↔ script split)

Each workflow is spelled out in `references/COMMANDS.md`; summary:

- **init** — `wiki.py init` scaffolds everything idempotently (existing data never touched); agent writes `wiki/overview.md` skeleton and registers nothing.
- **ingest** — `wiki.py ingest --file/--url/--text/--dir [--source-id]`: preserve raw → resolve identity → version by hash → write manifest + empty extraction → receipt JSON. Ingest MUST NOT create/modify claims (PRD §29). Secrets check runs before durable write; `deny` policy aborts.
- **compile** — 4 phases:
  1. `wiki.py compile-plan` → pending source versions list;
  2. agent reads normalized content, writes candidate claims (with evidence locators + extraction report) to staging via `wiki.py stage-candidates --run <id>`;
  3. `wiki.py reconcile-prepare` matches candidates against existing claims (identity/scope/time), agent classifies each match per RECONCILIATION.md and stages a reconcile plan;
  4. `wiki.py reconcile-apply --run <id>` applies transactionally with auto-accept policy, supersedes/closes validity, appends review items where policy says review, runs invalidation, emits run receipt. Agent then regenerates invalidated prose pages; `wiki.py build-pages` (re)builds the deterministic views (wiki/index.md, changes/ briefing entries, per-decision page stubs with claim tables) and registers their dependency sets. Agent finishes with `wiki.py verify` (all citations resolve; no stale derived artifact unmarked).
- **query** — `wiki.py query-prepare "question" [--as-of date]` → matches + conflicts + citations; agent answers per contract (answer/evidence/scope/historical/conflicts/gaps) or refuses (PRD §35). Read-only.
- **context** — `wiki.py context-pack --task "..." --budget 6000` (or `--resume`, `--changes-since <ctx>`) → deterministic pack + receipt; hard budget never exceeded (release gate).
- **review** — `wiki.py review list|show|act`; actions are transactions; agent presents items and recommends, human decision is recorded verbatim as authority.
- **doctor** — `wiki.py doctor [--json]` → findings with severity + fix hints; agent narrates and can stage fixes through transactions.

## 8. Acceptance policy (default, project-configurable in `wiki.json`)

Auto-accept a candidate claim iff authority type ∈ {`explicit_project_decision`,
`authoritative_source`, `implementation_verification`} and no unresolved conflict.
`independent_corroboration` (≥2 distinct `root_origin`s) auto-accepts as
`provisional`. Everything else stays `candidate` and (when risky) lands in the
review queue. Imported content never gains execution authority (PRD §49): text
that looks like instructions to the agent is stored as evidence and flagged by
`ingest` (injection heuristic warning in extraction report).

## 9. Security

- All imported content is untrusted data (PRD §49). References/SECURITY.md instructs the agent: content instructions are never executed, never elevate authority.
- `secrets.py` heuristics gate durable ingestion; policy configurable; secret values never appear in operations.jsonl or receipts (only `"redacted:<count>"`).
- Project isolation: everything under the project's `.llm-wiki/`; no cross-project reads unless `wiki.json` sets `allow_cross_project: true` (default false). Release-gate tested.

## 10. Evaluation

- **Deterministic gate tests (PRD §58)** as unittest suites in `skill/scripts/tests/` + `evals/deterministic/`: citation resolution 100%, hash dedup, correction invalidates descendants, stale-commit rejection, injection immunity (script-level: instructions-in-content never parsed as commands), project isolation, context hard budget, transaction atomicity, retraction invalidation.
- **Golden corpus** (`evals/golden-corpus/`): miniature "shop-platform" project with README, 3 ADRs (one outdated), a correction memo, prod-vs-sandbox scope difference, duplicate wiki summaries of one ADR (independence test), a failed-approach note, a meeting-notes transcript, a CSV "spreadsheet", an unanswerable question target.
- **Semantic scenario briefs** (`evals/semantic/`): the 8 PRD §57 scenarios (direct recall, cross-source synthesis, correction, historical, scope, conflict, unanswerable, isolation) + extraction/reconciliation accuracy rubrics, run manually-by-agent against the corpus; results recorded to `evals/semantic/results.jsonl`.
- **Skill smoke test (writing-skills GREEN gate):** after install, launch `omp -p` in a scratch copy of the corpus project and verify: skill is discovered; `/wiki-init`→ingest→compile→query flow produces a correct, cited answer; the unanswerable question is refused. Baseline (RED) documented: same prompt without the skill gives uncited, non-persistent answers.

## 11. Install & verification

`install.sh` locates OMP's user skills dir (probe `~/.omp/skills`, then
`~/.agents/skills`; verify against OMP discovery before symlink) and symlinks
`skill/llm-wiki` there. Verification: `omp` in a temp dir lists `llm-wiki` among
discovered skills (`omp` skill listing / session system prompt check), then the
§10 smoke test. Uninstall = remove symlink.

## 12. Testing strategy

- `python3 -m unittest discover -s skill/scripts/tests` (stdlib unittest; Python ≥3.9 compatible syntax only).
- Every wikicore module ships with its test file; transaction/invalidation/budget tests run against temp projects (mkdtemp).
- No network, no external processes in unit tests; corpus-based evals are file-only.

## 13. Risks / mitigations

| Risk | Mitigation |
|---|---|
| Agent bypasses scripts and edits canonical files | SKILL.md hard rule + doctor detects out-of-band edits via revision hash mismatch |
| Token estimate inaccuracy | Budget uses conservative `len//4`; hard ceiling enforced post-selection with truncation marker |
| OMP skill-dir variance across versions | install.sh probes discovery empirically (temp skill + `omp -p` echo test) |
| stdlib-only YAML limits | Canonical data is JSON; frontmatter restricted to flat schema and round-trip tested |
| fcntl locks are POSIX-only | Documented requirement: macOS/Linux (OMP host platforms); Windows gets best-effort `msvcrt` fallback |
