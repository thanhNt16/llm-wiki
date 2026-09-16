#!/usr/bin/env bash
# Install the llm-wiki skill for Oh My Pi (OMP).
#
# One-line install (no clone needed):
#   curl -fsSL https://raw.githubusercontent.com/thanhNt16/llm-wiki/main/install.sh | bash
#
# From a git checkout of this repo:
#   ./install.sh              # symlink into OMP's native user skills dir
#   ./install.sh --agents     # symlink into ~/.agents/skills (cross-runtime)
#   ./install.sh --uninstall  # remove the symlink
#   ./install.sh --purge      # uninstall + delete the ~/.llm-wiki clone
#
# Curl mode clones this repo (shallow) to ~/.llm-wiki and symlinks the skill
# from there; later runs of the same curl command update the clone (git pull).
set -euo pipefail

REPO_URL="https://github.com/thanhNt16/llm-wiki.git"
SKILL_NAME="llm-wiki"
CLONE_HOME="$HOME/.llm-wiki"

TARGET_DIR="$HOME/.omp/agent/skills"
if [[ "${1:-}" == "--agents" ]]; then
  TARGET_DIR="$HOME/.agents/skills"
fi

uninstall() {
  local removed=0
  for dir in "$HOME/.omp/agent/skills" "$HOME/.agents/skills"; do
    if [[ -L "$dir/$SKILL_NAME" ]]; then
      rm "$dir/$SKILL_NAME"
      echo "removed $dir/$SKILL_NAME"
      removed=1
    fi
  done
  [[ "$removed" == "0" ]] && echo "no $SKILL_NAME symlink found"
  if [[ "${1:-}" == "--purge" ]]; then
    if [[ -d "$CLONE_HOME/.git" ]]; then
      rm -rf "$CLONE_HOME"
      echo "removed clone $CLONE_HOME"
    fi
  fi
}

if [[ "${1:-}" == "--uninstall" || "${1:-}" == "--purge" ]]; then
  uninstall "$@"
  exit 0
fi

# Resolve the skill source: a checkout next to this script, else clone/pull.
SRC=""
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd)" || SCRIPT_DIR=""
if [[ -n "$SCRIPT_DIR" && "$SCRIPT_DIR" != "/dev" && -f "$SCRIPT_DIR/skill/$SKILL_NAME/SKILL.md" ]]; then
  SRC="$SCRIPT_DIR"
  MODE="checkout"
else
  if [[ -d "$CLONE_HOME/.git" ]]; then
    if ! git -C "$CLONE_HOME" pull --ff-only >/dev/null 2>&1; then
      echo "note: could not update existing clone at $CLONE_HOME; using it as-is" >&2
    fi
    MODE="update"
  elif [[ -e "$CLONE_HOME" ]]; then
    echo "error: $CLONE_HOME exists but is not a git clone; move it aside or remove it" >&2
    exit 1
  else
    git clone --depth 1 "$REPO_URL" "$CLONE_HOME"
    MODE="clone"
  fi
  SRC="$CLONE_HOME"
fi

SKILL_SRC="$SRC/skill/$SKILL_NAME"
TARGET="$TARGET_DIR/$SKILL_NAME"

if [[ ! -f "$SKILL_SRC/SKILL.md" ]]; then
  echo "error: $SKILL_SRC/SKILL.md not found" >&2
  exit 1
fi

mkdir -p "$TARGET_DIR"
if [[ -e "$TARGET" && ! -L "$TARGET" ]]; then
  echo "error: $TARGET exists and is not a symlink; remove it first" >&2
  exit 1
fi
ln -sfn "$SKILL_SRC" "$TARGET"

echo "installed ($MODE): $TARGET -> $SKILL_SRC"
echo
echo "verify with:"
echo "  omp -p --no-session 'Do you have a skill named llm-wiki? Answer yes/no.'"
