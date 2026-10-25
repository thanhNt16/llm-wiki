# Lane-Fill Design — auto-populate every wiki/ kind

**Date:** 2026-10-25 · **Branch:** `skill-effectiveness` · **Status:** approved in dialogue

## Problem

The full agent pipeline (ingest → compile → reconcile → `page-targets` → `write-page`) produces **only `concepts/` pages** and leaves `entities/`, `procedures/`, `questions/`, and the `decisions/` lanes empty. Verified live on a 129-source / 446-claim corpus: `page-targets` emitted 42/42 concept targets and zero of every other kind; `decisions/` had no records at all.

Root causes:

1. `page-kinds.json` does not exist by default; absent file → all subjects route to `concepts` (synthesize.py `_load_kind_rules` → `default="concepts"`).
2. Nothing mints `decision_*.json` records — `decide` is the only writer, and no skill step tells the agent to run it after compile.
3. Skills document kind-routing as an optional per-project extension, not as part of the standard loop.

## Goals

- A fresh `init` + full pipeline produces populated lanes for every kind the corpus actually supports — entities/procedures/questions pages where subjects warrant, and `decision_*.json` records for every `explicit_project_decision`-authority claim — with **zero manual setup**.
- Per-project control stays: `page-kinds.json` rules still override everything.

## Design

### 1. Engine: heuristic kind-routing fallback (`wikicore/synthesize.py`)

`_load_kind_rules` gains a third return: `heuristics_active: bool` — true when `page-kinds.json` is absent OR has an empty `rules` list. When active, `page_targets` evaluates each eligible subject through ordered built-in rules:

| # | Matcher | Routes to |
|---|---|---|
| 1 | `predicate` regex `^(step|procedure|runbook|how[ _-]?to)` | `procedures` |
| 2 | `subject` regex `^[a-z]+-\d+$` (ticket ids: `stc-97`) OR `\.(api|service|ui|db|cli)$` | `entities` |
| 3 | `subject` has ≥2 claims AND looks like a code identifier (single-token snake/kebab/camel, no spaces) | `entities` |
| 4 | `subject`/`predicate` regex `open[ _-]?question|unresolved|tbd` | `questions` |
| 5 | else | `concepts` |

- First match wins; file rules (when present) are evaluated BEFORE heuristics — a file rule for a subject always wins; file `rules: []` means "use heuristics" not "disable routing". To pin everything to concepts, add a catch-all file rule `{"kind":"concepts","regex":".*"}`.
- Each target reports `matched_rule`: integer index for file rules, `"heuristic:N"` for heuristic hits, `null` for the default — so agents can see and override the routing decision.
- `--kind` CLI override unchanged (wins over both).

### 2. Engine: starter `page-kinds.json` at init (`wikicore/init` or wherever init scaffolds)

`init` writes `.llm-wiki/page-kinds.json`:

```json
{"rules": [], "default": "concepts", "_comment": "ordered rules; matchers: subject|prefix|suffix|regex|predicate; when.min_claims; absent file → built-in heuristics"}
```

Presence with empty rules activates heuristics and gives the agent a visible, documented file to extend. Idempotent: existing file is never overwritten on re-init.

### 3. Engine: decision auto-materialization (reconcile-apply path)

When `reconcile-apply` accepts a claim whose `authority.type == "explicit_project_decision"`:

- Mint `.llm-wiki/decisions/decision_<ulid>.json` in the same transaction:

```json
{"id": "decision_<ulid>", "title": "<subject>",
 "body": "<predicate> = <value>",
 "status": "accepted", "decided_on": "<claim.recorded_at[:10]>",
 "claims": ["<claim_id>"], "evidence": [<claim.evidence verbatim>],
 "authority": "explicit_project_decision", "auto_materialized": true}
```

- **Idempotent:** if any existing decision's `claims[]` already contains the claim id, skip (no dupes on re-accept/supersede cycles).
- `wiki.py decide` remains the richer manual path; a human/agent `mark_superseded` can replace a stub later.
- `doctor` gains an `info`-severity finding `auto_decision_stub` listing stubs awaiting human review (count only — never warn/error).
- `build-pages` already renders `wiki/decisions/*.md` per record — no change needed there.

### 3.5. Jev decision client (typed judgments, env-gated)

The engine stays deterministic-stdlib — Jev is a **read-only advisory helper** the agent calls at judgment points; every wired flag must produce identical results with Jev absent (fallback = skill-documented agent judgment).

**Config (env):**

- `DECISION_TOKEN` — bearer key for the System One endpoint (unset → Jev disabled everywhere).
- `DECISION_MODEL` — model string, e.g. `cf/@cf/cloudflare/clef-flash` or `openrouter/typesafe/jev-1.13` (unset → disabled; a missing model is NOT an error, just off).
- `DECISION_ENDPOINT` — optional, default `http://localhost:20128/v1/systemone`.

**Client** — `wikicore/jev.py` (stdlib `urllib` only):

- `is_ready() -> bool` — token + model both set.
- `ask(state: str, questions: dict) -> dict` — POST `{model, state, questions}`; returns `{"ok": bool, "answers": {q: {type, value}}, "usage": {...}, "disabled": bool, "error": str|None}`; normalizes wire shape (`noul`→`p(yes)`, `choice`→`{pick, distribution}`, `score`→`{level, expected}`); ≤10s timeout; never raises to caller — errors return `ok:false`.

**CLI** — `wiki.py jev`:

- `jev --check` — readiness probe: prints `{"ready": true, "model": "<m>"}` or `{"ready": false, "reason": "DECISION_TOKEN unset"}`; exit 0 either way (it answers *configured?*, not a failure).
- `jev --state <text> --question <json>` — runs one query, prints the normalized result.

**Wired flags** (`--decide` on two judgment commands; all advisory, never blocking):

- `page-targets --decide` — for each subject routed by a *heuristic* rule, one `choice` question (`{"kind":{"type":"choice","options":["concepts","entities","procedures","questions"],"instructions":"…"}}` over subject+predicates); the verdict rides on the target as `decision: {suggested_kind, confidence}`; agent may act on it or ignore — routing itself stays deterministic.
- `write-page --decide` — when the near-dup guard would refuse, one `noul` ("is this genuinely the same page?"); verdict returned in the error JSON's `decision` field so the agent sees `p(yes)` before choosing `--allow-new`.

**Skill guidance (required-when-ready)** — COMMANDS.md + wiki-write instruct: call `wiki.py jev --check` once per session. When `ready:true`, the agent MUST consult Jev at the three judgment points before acting: (a) **kind routing** — `page-targets --decide` verdicts are applied: where `suggested_kind` differs from the routed kind, the agent adds a `page-kinds.json` rule for that subject (making the decision durable, not per-run), then re-runs targets; (b) **decision-worthiness** — a `noul` verdict gates whether an `explicit_project_decision` claim warrants a richer `wiki.py decide` record beyond its auto-stub; (c) **`--allow-new`** — the write-page `decision.p(yes)` verdict must be low or justified in output before the agent passes the escape flag. When `ready:false`, the agent applies its own judgment per existing skill rules (extend-first etc.) and notes the fallback. Jev never appears in `verify`/`doctor`/gate paths — advisory only, but consulted-when-ready is a skill requirement, not optional.

### 4. Skill instruction updates

- `skill/wiki-write/SKILL.md` — pipeline step 2 rewritten: "author a page for **every** `page-targets` entry; if `matched_rule` looks wrong, add a `page-kinds.json` rule first, re-run targets, then write." Explicit note that `entities/procedures/questions` are populated via routing, not optional.
- `skill/llm-wiki/references/COMMANDS.md` — `page-targets` section documents heuristic rules, `matched_rule` values, the starter file; `decide` section documents auto-materialized stubs + `auto_decision_stub` doctor finding.
- `skill/wiki-write/references/CONVENTIONS.md` — kind-routing section updated: default behavior is heuristics, file rules override.
- COMMANDS.md/wiki-write also document `wiki.py jev --check`, `DECISION_TOKEN`/`DECISION_MODEL`/`DECISION_ENDPOINT`, and the unconfigured→agent-judgment fallback.
- `skill/llm-wiki/SKILL.md` — body at 598 words is at the <600 cap; at most ONE clause added to the compile router row ("page lanes auto-fill via page-targets routing"); if the cap blocks it, leave SKILL.md untouched.

### 5. Testing

**Unit tests** (extend test_synthesize / test_compile / test_init):

- heuristic routing: each of the 5 rules hits the expected kind; file rule beats heuristic; `matched_rule` values correct; catch-all file rule forces all-concepts.
- decision stub: accept an `explicit_project_decision` claim → `decision_*.json` exists with claims[]+evidence; re-accept → no duplicate; supersede path keeps old decision's status update; `auto_decision_stub` appears in `doctor.run` findings as info.
- `init` writes starter `page-kinds.json`; re-init doesn't clobber a modified one.
- jev client: `ask` parses noul/choice/score wire shapes (stubbed HTTP); disabled when env unset; `jev --check` prints ready/not-ready with reason, exit 0 both ways; `page-targets --decide`/`write-page --decide` produce identical targets/errors with env unset, and include `decision` fields when a stubbed endpoint answers.

**E2E acceptance** (the user-facing proof):

1. Fresh `init` at `/tmp/ptt-wiki-demo-2` → full real pipeline on `/Users/harry/Desktop/cellock/ptt/docs` (129 sources, parallel extraction, serial reconcile).
2. Assert: `decisions/` non-empty; `page-targets` emits ≥3 distinct kinds; agent authors all non-`extend-first` targets; `build-pages` + `verify` ok; `doctor` shows no new error findings.
3. With `DECISION_TOKEN`/`DECISION_MODEL` set: `page-targets --decide` emits `decision` verdicts on heuristic-routed targets; `wiki.py jev --check` reports ready. Unset: identical outputs minus `decision` fields.
4. **Separate `omp -p --no-session` session** pointed at the wiki path only — answers across entity/procedure/question pages with citations + one unanswerable → refusal. Recorded as an eval row.

## Non-goals

- `verified` authority stays human-only; `auto_materialized` stubs are explicitly NOT `verified` — they're flagged for review, never silent authority.
- No changes to page-target eligibility (≥2 eligible claims across ≥2 `root_origin`s, or 1 decision-authority claim).
- No new CLI subcommands other than the read-only `wiki.py jev` helper; `page-kinds.json` remains the only routing-override mechanism. Jev output is advisory and never enters verify/doctor/gate paths or changes deterministic results.
- No changes to `query-prepare`/`context-pack` semantics.

## Risks

- **Wrong-kind noise:** heuristic subject-shape rules could misroute (e.g. a `db`-suffixed concept). Mitigation: `matched_rule` visibility + trivial file-rule override + tests pinning the common shapes.
- **Decision stub noise:** a corpus heavy on decision-authority claims yields many stubs. Mitigation: they're `info`-level, marked `auto_materialized`, and cheap — still better than an invisible empty lane.
- **Jev endpoint availability/timeout:** `ask` hard-times-out ≤10s and returns `ok:false`; wired flags degrade to absent `decision` fields, never fail the command. The endpoint is localhost-dev — `DECISION_ENDPOINT` keeps it configurable.
