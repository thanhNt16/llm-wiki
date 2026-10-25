---
name: wiki-doctor
description: Use when llm-wiki memory looks broken, commands fail repeatedly, or diagnostics are needed on project memory state, or when the user says wiki-doctor.
---

# wiki-doctor — memory diagnostics

Static + semantic diagnostics over `.llm-wiki/`. Run when something looks
wrong, or before reporting.

## Engine

Diagnostics go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Resolve it once, then run from the
project root:

```bash
WIKI=$(ls "$SKILL_DIR/../llm-wiki/scripts/wiki.py" ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py "$PWD/../llm-wiki/scripts/wiki.py" 2>/dev/null | head -1)
[ -f "$WIKI" ] || echo "wiki.py not found — set WIKI=/path/to/llm-wiki/scripts/wiki.py manually"
python3 "$WIKI" doctor
```

Findings have `severity` (error/warn/info), `code`, `detail`, `fix`.
Categories:

- Schema validity of canonical claim/decision files (including file↔`id`
  field mismatches); broken references (evidence source ids, cited source
  versions beyond the manifest, supersedes chains, decision claim lists).
- Derived-dependency integrity: artifacts registered stale in
  `.state/dependencies.json`, dep edges whose target is missing or
  unresolvable (`unresolved_dependency`), registered artifacts whose file is
  absent (`missing_artifact`), and derived `wiki/**` pages with no
  dependency registration at all (`unregistered_page`).
- Interrupted transactions: leftover `.state/journal.json`, stale
  `.state/.backup-<rev>/` directories, and stale/unexpected lock files
  (`stale_lock`). Recommend recovery — never auto-delete canonical data.
- `staging_leftover` run dirs can be deleted after inspection.
- Review-queue hygiene: malformed JSONL lines, items missing required
  fields, `affected` ids that no longer exist, and items deferred >7 days.
- Orphan sources (never cited by any claim) and invalid source-dir ids.
- Decisions: `stale_decision` (cites a superseded/rejected claim) and
  `orphan_decision` (no claims and no evidence).
- Semantic debt: accepted claims with no independent evidence or missing
  `scope`/`valid_from`, disputed claims and open `possible_contradiction`
  review items, claims with no evidence, `possible_duplicate` claims
  (same subject+predicate+scope+value, both non-terminal), `stale_candidate`
  claims unreviewed >14 days, and text sources whose extraction report still
  says `not_extracted`. Report to the user — do not auto-resolve.
- Page-graph lint (all warn): `orphan_page` (no inbound `[[wiki:]]` links),
  `dead_end_page` (links to no other page), `walled_garden` (≥3 pages whose
  links stay inside the cluster — the largest component is never flagged),
  `near_duplicate_subject` (two pages whose subject tokens overlap
  Jaccard ≥0.7 with no link between them), `stale_page_ttl` (frontmatter
  `stale_after` date has passed).

Fix errors first — usually broken references from out-of-band edits. Report
unresolved semantic debt and page-graph findings to the user — retargeting
links, splitting, or deleting pages are user decisions, never silent edits.

## Hard rules

- Never repair canonical files by hand; fixes go through `wiki.py`
  subcommands.
- On repeated command failures, run doctor and report findings to the user.

Full contract: `references/COMMANDS.md#doctor` inside the `llm-wiki` skill.
