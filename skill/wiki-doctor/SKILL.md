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

- Fix errors first — usually broken references from out-of-band edits.
- `staging_leftover` entries can be deleted after inspection.
- Report unresolved semantic debt (unsupported claims, contradictions) to the
  user — do not auto-resolve.

## Hard rules

- Never repair canonical files by hand; fixes go through `wiki.py`
  subcommands.
- On repeated command failures, run doctor and report findings to the user.

Full contract: `references/COMMANDS.md#doctor` inside the `llm-wiki` skill.
