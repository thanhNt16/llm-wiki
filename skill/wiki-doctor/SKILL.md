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
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" doctor
```

Findings have `severity` (error/warn/info), `code`, `detail`, `fix`.
Categories:

- Schema validity of canonical claim/decision files; broken references
  (evidence source ids, supersedes chains, decision claim lists).
- Derived-dependency integrity: every registered dep must be a current
  claim, decision, or registered `wiki/` artifact; malformed or stale
  versions and unknown namespaces are reported, as are edges pointing at
  missing artifacts (orphan edges) and dependency cycles.
- Page integrity: frontmatter `subject` vs artifact slug mismatches,
  subjects colliding on the same `(kind, slug)`, pages depending on
  disputed claims without the required labeling, duplicate `page-targets`
  artifacts, and ambiguous `page-kinds.json` routing (shadowed rules).
- Interrupted commits: operation receipts disagreeing with canonical files.
  Recommend recovery — never auto-delete canonical data.
- `staging_leftover` entries can be deleted after inspection.
- Semantic debt: unsupported claims, disputed claims, unresolved
  contradictions, stale derived artifacts, stale or dangling context packs,
  low extraction coverage. Report to the user — do not auto-resolve.

Fix errors first — usually broken references from out-of-band edits. Report
unresolved semantic debt to the user.

## Hard rules

- Never repair canonical files by hand; fixes go through `wiki.py`
  subcommands.
- On repeated command failures, run doctor and report findings to the user.

Full contract: `references/COMMANDS.md#doctor` inside the `llm-wiki` skill.
