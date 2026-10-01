---
name: wiki-init
description: Use when asked to set up, initialize, or scaffold llm-wiki project memory in a repository, or when the user says wiki-init.
---

# wiki-init — scaffold `.llm-wiki/`

Creates the `.llm-wiki/` layout, config, and state in the current project.
Idempotent and non-destructive.

## Engine

All state changes go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Run **one** command from the project
root — it resolves the engine itself:

```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1) && python3 "$WIKI" init --name "$(basename "$PWD")"
```

Do **not** probe for the script, check for an existing `.llm-wiki/`, or verify
the result with `find`/`ls` — `init` is idempotent, its JSON output is the
verification, and extra round-trips only add latency.

**Never `mkdir .llm-wiki/` or write `wiki.json`/state files by hand** — a
hand-made layout lacks state manifests and schemas and is unusable. If a
`.llm-wiki/` already exists without `.state/`, it is debris from an improvised
attempt: ask the user before removing it.

## Contract

- If output says `"initialized": false`, the wiki already exists — do nothing
  destructive; optionally run `wiki.py doctor`.
- After first init, report: storage path, revision, and the seven commands
  (init, ingest, compile, query, context, review, doctor).
- Exit codes: 3 validation, 4 stale revision, 5 locked (another writer active).

Full contract: `references/COMMANDS.md#init` inside the `llm-wiki` skill.
