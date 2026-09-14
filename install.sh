#!/usr/bin/env bash
# Install the llm-wiki skill for Oh My Pi (OMP).
#
#   ./install.sh              # symlink into OMP's native user skills dir
#   ./install.sh --agents     # symlink into ~/.agents/skills (cross-runtime)
#   ./install.sh --uninstall  # remove the symlink
#
# The skill source of truth stays in this repo; the install is a symlink.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
SKILL_SRC="$REPO_ROOT/skill/llm-wiki"
SKILL_NAME="llm-wiki"

TARGET_DIR="$HOME/.omp/agent/skills"
if [[ "${1:-}" == "--agents" ]]; then
  TARGET_DIR="$HOME/.agents/skills"
fi
TARGET="$TARGET_DIR/$SKILL_NAME"

if [[ "${1:-}" == "--uninstall" ]]; then
  for dir in "$HOME/.omp/agent/skills" "$HOME/.agents/skills"; do
    if [[ -L "$dir/$SKILL_NAME" ]]; then
      rm "$dir/$SKILL_NAME"
      echo "removed $dir/$SKILL_NAME"
    fi
  done
  exit 0
fi

if [[ ! -f "$SKILL_SRC/SKILL.md" ]]; then
  echo "error: $SKILL_SRC/SKILL.md not found (run from repo root)" >&2
  exit 1
fi

mkdir -p "$TARGET_DIR"
if [[ -e "$TARGET" && ! -L "$TARGET" ]]; then
  echo "error: $TARGET exists and is not a symlink; remove it first" >&2
  exit 1
fi
ln -sfn "$SKILL_SRC" "$TARGET"
echo "installed: $TARGET -> $SKILL_SRC"
echo
echo "verify with:"
echo "  omp -p --no-session 'Do you have a skill named llm-wiki? Answer yes/no.'"
