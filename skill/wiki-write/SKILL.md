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
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" page-targets
```

## Pipeline

1. `python3 "$WIKI" page-targets` — subjects that qualify for a page, plus
   stale pages needing refresh. Flags: `--kind <concepts|entities|procedures|questions|sources|changes>`,
   `--stale-only`.
2. For each target, read its claim details and author the page per
   `references/PAGES.md` under the `references/CONVENTIONS.md` rules. Assign
   writer roles per `agents/` when delegating. The kind comes from
   `.llm-wiki/page-kinds.json` routing — use `target.artifact` verbatim as
   the `--artifact` value; never guess `wiki/concepts/…`.
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
- Exit codes: 3 validation, 4 stale revision (re-run `page-targets`,
  re-author, commit with the fresh revision), 5 locked.

Details: `references/CONVENTIONS.md` (ownership regimes, eligibility,
citations, status handling), `references/PAGES.md` (per-type page contracts),
`agents/source_writer.md` + `agents/page_writer.md` (role briefs).
