---
name: wiki-write
description: Use after wiki-compile reconcile to write or refresh readable wiki/ pages from claims, or when the user says wiki-write, write wiki pages, or synthesize wiki.
---

# wiki-write — claims → prose

Turns accepted knowledge into readable pages under `.llm-wiki/wiki/`. You (the
agent) author the prose; the script validates, registers dependencies, and
commits atomically. The script never invents prose; you never touch canonical
state.

## Engine

All page writes go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Resolve it once, then run from the
project root:

```bash
WIKI=$(ls "$SKILL_DIR/../llm-wiki/scripts/wiki.py" ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py "$PWD/../llm-wiki/scripts/wiki.py" 2>/dev/null | head -1)
[ -f "$WIKI" ] || echo "wiki.py not found — set WIKI=/path/to/llm-wiki/scripts/wiki.py manually"
python3 "$WIKI" page-targets
```

## Pipeline

1. `python3 "$WIKI" page-targets` — subjects that qualify for a page, plus
   stale pages needing refresh. Flags: `--kind <concepts|entities|procedures|questions|sources|changes>`,
   `--stale-only`, `--decide` (advisory Jev verdicts on heuristic-routed targets).
2. Author a page for **every** `targets[]` entry — `entities`,
   `procedures`, and `questions` populate via kind routing; they are not
   optional extras. Read each target's claim details and write the page per
   `references/PAGES.md` under the `references/CONVENTIONS.md` rules;
   assign writer roles per `agents/` when delegating. The kind comes from
   `page-kinds.json` file rules or the built-in heuristics (reported as
   `target.matched_rule`) — use `target.artifact` verbatim as `--artifact`;
   never guess `wiki/concepts/…`. If `matched_rule` looks wrong, add a
   `page-kinds.json` rule first, re-run `page-targets`, then write.
   Consulted-when-ready: run `wiki.py jev --check` once per session; when
   `ready: true`, re-run `page-targets --decide` and pin any differing
   `decision.suggested_kind` as a `page-kinds.json` rule before writing,
   and pass `--allow-new` only when a refusal's `decision.same_page_p` is
   low or you justify the escape in your output.
3. `python3 "$WIKI" write-page --file page.md --artifact <target.artifact> --deps 'claim_abc123@3,claim_def456@1'`
   — pass deps inline (comma-joined, current versions, one entry per cited
   `[[claim_id]]`; they duplicate the frontmatter `deps:` list). `--deps-file deps.json`
   remains for long lists. Validates frontmatter and links, stages the page,
   registers `deps`, clears the stale flag, commits. Receipt:
   `{"written": ..., "deps": [...], "claims_cited": n}`.
4. After the last page: `python3 "$WIKI" build-pages` — regenerates section
   indexes and `overview.md`.
5. `python3 "$WIKI" verify` — must report `"ok": true` before you finish. Fix
   every error (unresolved links, stale pages) and re-verify.

## Hard rules

- `wiki/{sources,concepts,entities,procedures,questions,changes}/*.md` are
  written ONLY through `write-page` — never hand-edit, not even a typo.
  Hand edits break dep registration and fail `verify`.
- `wiki/index.md`, `wiki/overview.md`, `wiki/decisions/*`, `*/index.md` are
  script-owned; `write-page` rejects them. There is no overview writer.
- Cite claims as `[[claim_id]]` only; `deps` must list every cited claim at
  its current version. Rejected/superseded claims never appear in body or
  deps; `disputed`/`provisional` claims only with visible labels.
- Extend, don't duplicate — near-duplicate pages are the default failure mode.
  When `page-targets` marks a target `routing: extend-first`, extend the
  `similar_existing` twin instead of creating a new page; `write-page` refuses
  near-duplicates unless you pass `--allow-new` deliberately.
- Every page must be navigable (the navigation ladder: overview → section
  index → page → See-also). End concept/entity pages with `## See also`
  carrying 1–3 `[[wiki:kind/slug]]` links to related pages; each link needs
  its `wiki/<kind>/<slug>.md` dep (auto-derived when you don't pass `--deps`).
  Pages with no inbound links are `orphan_page` findings; pages that link
  nowhere are `dead_end_page`. Circular first-creation needs two passes:
  write B without the link, write A linking B, then re-write B linking A.
- Exit codes: 3 validation, 4 stale revision (re-run `page-targets`,
  re-author, commit with the fresh revision), 5 locked.

Details: `references/CONVENTIONS.md` (ownership regimes, eligibility,
citations, status handling), `references/PAGES.md` (per-type page contracts),
`agents/source_writer.md` + `agents/page_writer.md` (role briefs).
