#!/usr/bin/env bash
# Install the llm-wiki skills for Oh My Pi (OMP).
#
# One-line install (no clone needed):
#   curl -fsSL https://raw.githubusercontent.com/thanhNt16/llm-wiki/main/install.sh | bash
#
# From a git checkout of this repo:
#   ./install.sh              # symlink into OMP's native user skills dir
#   ./install.sh --agents     # symlink into ~/.agents/skills (cross-runtime)
#   ./install.sh --cellockai  # copy skills into ~/.cellockai/skills (CellockAI
#                             # extension reads real skill dirs, not symlinks)
#   ./install.sh --uninstall  # remove installed skills, keep the clone
#   ./install.sh --purge      # uninstall + delete the ~/.llm-wiki clone
#
# Curl mode clones this repo (shallow) to ~/.llm-wiki and symlinks the skills
# from there; later runs of the same curl command update the clone (git pull).
#
set -euo pipefail

REPO_URL="https://github.com/thanhNt16/llm-wiki.git"
SKILLS=(llm-wiki wiki-init wiki-ingest wiki-compile wiki-query wiki-context wiki-review wiki-doctor wiki-visualize wiki-write)
CLONE_HOME="$HOME/.llm-wiki"
# Dropped into each copied skill dir; uninstall only removes dirs carrying it.
MARKER=".llm-wiki-installed"

usage() {
  echo "usage: $0 [--agents|--cellockai|--uninstall|--purge]" >&2
  exit 2
}

# Prerequisites first: the engine needs python >= 3.9; clone/update needs git.
if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 not found; install Python >= 3.9 first" >&2
  exit 1
fi
if ! python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
  echo "error: python >= 3.9 required (found: $(python3 --version 2>&1))" >&2
  exit 1
fi
if ! command -v git >/dev/null 2>&1; then
  echo "error: git not found; install git first" >&2
  exit 1
fi

if [[ $# -gt 1 ]]; then
  usage
fi
case "${1:-}" in
  "" | --agents | --cellockai | --uninstall | --purge) ;;
  *) usage ;;
esac

TARGET_DIR="$HOME/.omp/agent/skills"
INSTALL_MODE="symlink"
case "${1:-}" in
  --agents) TARGET_DIR="$HOME/.agents/skills" ;;
  --cellockai)
    TARGET_DIR="$HOME/.cellockai/skills"
    INSTALL_MODE="copy"
    ;;
esac

uninstall() {
  local removed=0
  for dir in "$HOME/.omp/agent/skills" "$HOME/.agents/skills" "$HOME/.cellockai/skills"; do
    for name in "${SKILLS[@]}"; do
      local target="$dir/$name"
      if [[ -L "$target" ]]; then
        rm "$target"
        echo "removed $target"
        removed=1
      elif [[ -d "$target" && -f "$target/$MARKER" ]]; then
        rm -rf "$target"
        echo "removed $target (installed copy)"
        removed=1
      elif [[ -d "$target" && -f "$target/SKILL.md" ]]; then
        echo "warning: $target has no $MARKER marker; leaving it in place" >&2
      fi
    done
  done
  [[ "$removed" == "0" ]] && echo "no llm-wiki skill installs found"
  if [[ "${1:-}" == "--purge" ]]; then
    if [[ -d "$CLONE_HOME/.git" ]]; then
      local origin
      origin="$(git -C "$CLONE_HOME" remote get-url origin 2>/dev/null || true)"
      if [[ "$origin" == "$REPO_URL" || "$origin" == "${REPO_URL%.git}" ]]; then
        rm -rf "$CLONE_HOME"
        echo "removed clone $CLONE_HOME"
      else
        echo "warning: $CLONE_HOME is a clone of '$origin', not $REPO_URL; leaving it in place" >&2
      fi
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
if [[ -n "$SCRIPT_DIR" && "$SCRIPT_DIR" != "/dev" && -f "$SCRIPT_DIR/skill/llm-wiki/SKILL.md" ]]; then
  SRC="$SCRIPT_DIR"
  MODE="checkout"
else
  if [[ -d "$CLONE_HOME/.git" ]]; then
    ORIGIN="$(git -C "$CLONE_HOME" remote get-url origin 2>/dev/null || true)"
    if [[ "$ORIGIN" != "$REPO_URL" && "$ORIGIN" != "${REPO_URL%.git}" ]]; then
      echo "error: $CLONE_HOME is a clone of '$ORIGIN', not $REPO_URL" >&2
      echo "       refusing to pull/install from it; move it aside and re-run" >&2
      exit 1
    fi
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

mkdir -p "$TARGET_DIR"
for name in "${SKILLS[@]}"; do
  SKILL_SRC="$SRC/skill/$name"
  TARGET="$TARGET_DIR/$name"

  if [[ ! -f "$SKILL_SRC/SKILL.md" ]]; then
    echo "error: $SKILL_SRC/SKILL.md not found" >&2
    exit 1
  fi

  if [[ "$INSTALL_MODE" == "copy" ]]; then
    # CellockAI reads real dirs; existing skill dirs are backed up first.
    if [[ -d "$TARGET" || -L "$TARGET" ]]; then
      mv "$TARGET" "${TARGET}.bak.$(date +%Y%m%d%H%M%S)"
    fi
    cp -R "$SKILL_SRC" "$TARGET"
    touch "$TARGET/$MARKER"
    find "$TARGET" -name '.DS_Store' -delete 2>/dev/null || true
    find "$TARGET" -type d -name '__pycache__' -exec rm -rf {} + 2>/dev/null || true
    find "$TARGET" -type d -name 'node_modules' -exec rm -rf {} + 2>/dev/null || true
    # Keep dist/ under assets/: wiki-visualize ships a prebuilt graph-ui bundle.
    find "$TARGET" -type d -name 'dist' -not -path '*/assets/*' -exec rm -rf {} + 2>/dev/null || true
    echo "installed (copy): $TARGET"
  else
    if [[ -e "$TARGET" && ! -L "$TARGET" ]]; then
      echo "error: $TARGET exists and is not a symlink; remove it first" >&2
      exit 1
    fi
    ln -sfn "$SKILL_SRC" "$TARGET"
    echo "installed ($MODE): $TARGET -> $SKILL_SRC"
  fi
done

# Optional extractor: markitdown (office/pdf text extraction in wiki-ingest).
# Try uv tool -> pipx -> pip --user; failure is a warning, not fatal.
if ! command -v markitdown >/dev/null 2>&1; then
  installed=0
  if command -v uv >/dev/null 2>&1 && uv tool install markitdown 2>/dev/null; then
    installed=1
  fi
  if [[ "$installed" == "0" ]] && command -v pipx >/dev/null 2>&1 && \
     pipx install markitdown 2>/dev/null; then
    installed=1
  fi
  if [[ "$installed" == "0" ]] && command -v pip3 >/dev/null 2>&1 && \
     pip3 install --user markitdown 2>/dev/null; then
    installed=1
  fi
  if [[ "$installed" == "0" ]]; then
    echo "note: could not install markitdown; pdf/office files ingest as not_extracted" >&2
  fi
fi

echo
echo "verify with:"
echo "  omp -p --no-session 'Do you have skills named llm-wiki and wiki-init? Answer yes/no.'"
