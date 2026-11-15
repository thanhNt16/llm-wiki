#!/usr/bin/env bash
# Install the llm-wiki binary + skills for Oh My Pi (OMP).
#
# One-line install (no clone needed):
#   curl -fsSL https://raw.githubusercontent.com/thanhNt16/llm-wiki/main/install.sh | bash
#
# From a git checkout of this repo:
#   ./install.sh              # fetch the Rust binary release to
#                             # ~/.local/bin/llm-wiki, then
#                             # `llm-wiki --install-skills` into OMP's native
#                             # user skills dir (no Python engine required)
#   ./install.sh --agents     # same, targeting ~/.agents/skills (cross-runtime)
#   ./install.sh --cellockai  # same, targeting ~/.cellockai/skills
#   ./install.sh --uninstall  # remove installed skills, keep the clone
#   ./install.sh --purge      # uninstall + delete the ~/.llm-wiki clone
#   ./install.sh --python     # REMOVED — the Python engine is a dev-reference
#                             # under skill/llm-wiki/scripts/ (run it from a
#                             # repo checkout); it is no longer an install path
#
# Binary install needs no clone and no Python: the release binary embeds the
# skill set. LLM_WIKI_RELEASE=<tag> pins a specific release (default:
# latest). LLM_WIKI_BINARY_TARBALL=<path> installs a local release tarball
# (llm-wiki-<ver>-<triple>.tar.gz, with its .sha256 sidecar when present)
# instead of fetching from GitHub — for dev/CI installs of local builds.

set -euo pipefail

REPO_URL="https://github.com/thanhNt16/llm-wiki.git"
GITHUB_REPO="thanhNt16/llm-wiki"
SKILLS=(llm-wiki wiki-init wiki-ingest wiki-compile wiki-query wiki-context wiki-review wiki-doctor wiki-visualize wiki-write)
CLONE_HOME="$HOME/.llm-wiki"
BIN_DIR="$HOME/.local/bin"
# Dropped into each copied skill dir; uninstall only removes dirs carrying it.
MARKER=".llm-wiki-installed"

usage() {
  echo "usage: $0 [--binary] [--python] [--agents|--cellockai|--uninstall|--purge]" >&2
  exit 2
}

SKILLS_FLAG=""      # "" | --agents | --cellockai  (preset for llm-wiki --install-skills)
for arg in "$@"; do
  case "$arg" in
    --binary) ;;    # default install mode; flag kept for compatibility
    --python)
      echo "error: --python was removed at the phase-6 flip (spec §13)." >&2
      echo "       The Python engine is now a dev-reference for parity work —" >&2
      echo "       run it from a repo checkout:" >&2
      echo "         python3 skill/llm-wiki/scripts/wiki.py <cmd>" >&2
      echo "       The installed skill surface invokes the llm-wiki binary;" >&2
      echo "       run ./install.sh (or --binary) to install it." >&2
      exit 1
      ;;
    --agents | --cellockai | --uninstall | --purge)
      # target/uninstall selector — only one allowed
      if [[ -n "${SKILLS_FLAG:-}" || -n "${MODE_FLAG:-}" ]]; then
        usage
      fi
      case "$arg" in
        --agents | --cellockai) SKILLS_FLAG="$arg" ;;
        *) MODE_FLAG="$arg" ;;
      esac
      ;;
    *) usage ;;
  esac
done
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

# --uninstall/--purge dispatch before prerequisites: they need no tools
# and no binary fetch (uninstall() is defined above).
if [[ -n "${MODE_FLAG:-}" ]]; then
  uninstall "$MODE_FLAG"
  exit 0
fi

TARGET_DIR="$HOME/.omp/agent/skills"
case "${SKILLS_FLAG:-}" in
  --agents) TARGET_DIR="$HOME/.agents/skills" ;;
  --cellockai) TARGET_DIR="$HOME/.cellockai/skills" ;;
esac

# Prerequisites: tar + a sha256 tool always; curl only when fetching the
# release tarball from GitHub (not for LLM_WIKI_BINARY_TARBALL).
if [[ -z "${LLM_WIKI_BINARY_TARBALL:-}" ]]; then
  if ! command -v curl >/dev/null 2>&1; then
    echo "error: curl not found; required to fetch the binary release" >&2
    exit 1
  fi
fi
if ! command -v tar >/dev/null 2>&1; then
  echo "error: tar not found; required for the binary install" >&2
  exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  echo "error: sha256sum or shasum not found; required to verify the binary" >&2
  exit 1
fi

# uname -sm → Rust release triple (spec §12 table).
binary_triple() {
  local s m
  s="$(uname -s)"; m="$(uname -m)"
  case "$s/$m" in
    Darwin/arm64)    echo "aarch64-apple-darwin" ;;
    Darwin/x86_64)   echo "x86_64-apple-darwin" ;;
    Linux/x86_64)    echo "x86_64-unknown-linux-musl" ;;
    Linux/aarch64)   echo "aarch64-unknown-linux-musl" ;;
    *)
      echo "error: no binary release for $s/$m (Windows: use the PowerShell" >&2
      echo "       installer; other platforms: re-run without --binary)" >&2
      return 1
      ;;
  esac
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# Binary install (default): fetch llm-wiki-<ver>-<triple>.tar.gz (+ .sha256
# sidecar) from GitHub releases — or use LLM_WIKI_BINARY_TARBALL for a local
# tarball — → ~/.local/bin/llm-wiki → --install-skills preset → self-test on
# a tmp wiki. No clone, no Python engine.
install_binary() {
  local triple tag archive url tmp got want
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  if [[ -n "${LLM_WIKI_BINARY_TARBALL:-}" ]]; then
    # Local tarball: llm-wiki-<ver>-<triple>.tar.gz produced by a local
    # `cargo build --release` + manual packaging (release-rust.yml naming).
    # The <tarball>.sha256 sidecar is verified when present.
    archive="$(basename "$LLM_WIKI_BINARY_TARBALL")"
    if [[ ! -f "$LLM_WIKI_BINARY_TARBALL" ]]; then
      echo "error: LLM_WIKI_BINARY_TARBALL=$LLM_WIKI_BINARY_TARBALL not found" >&2
      exit 1
    fi
    cp "$LLM_WIKI_BINARY_TARBALL" "$tmp/$archive"
    tag="local"
    if [[ -f "$LLM_WIKI_BINARY_TARBALL.sha256" ]]; then
      want="$(awk '{print $1}' "$LLM_WIKI_BINARY_TARBALL.sha256")"
      got="$(sha256_file "$tmp/$archive")"
      if [[ -z "$want" || "$want" != "$got" ]]; then
        echo "error: sha256 mismatch for $archive (want $want, got $got)" >&2
        exit 1
      fi
      echo "verified $archive.sha256"
    else
      echo "note: no $archive.sha256 sidecar; skipping checksum verification" >&2
    fi
  else
    triple="$(binary_triple)"
    if [[ -n "${LLM_WIKI_RELEASE:-}" ]]; then
      tag="$LLM_WIKI_RELEASE"
    else
      tag="$(curl -fsSL "https://api.github.com/repos/$GITHUB_REPO/releases/latest" \
        2>/dev/null | grep '"tag_name"' \
        | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/' || true)"
      if [[ -z "$tag" ]]; then
        echo "error: no release published for $GITHUB_REPO yet." >&2
        echo "  - merge the release CI workflow and push a vX.Y.Z tag, or" >&2
        echo "  - build locally: (cd rust && cargo build --release), then" >&2
        echo "    LLM_WIKI_BINARY_TARBALL=<tarball> ./install.sh" >&2
        exit 1
      fi
    fi
    archive="llm-wiki-${tag#v}-$triple.tar.gz"
    url="https://github.com/$GITHUB_REPO/releases/download/$tag/$archive"
    curl -fsSL "$url" -o "$tmp/$archive"
    curl -fsSL "$url.sha256" -o "$tmp/$archive.sha256"
    want="$(awk '{print $1}' "$tmp/$archive.sha256")"
    got="$(sha256_file "$tmp/$archive")"
    if [[ -z "$want" || "$want" != "$got" ]]; then
      echo "error: sha256 mismatch for $archive (want $want, got $got)" >&2
      exit 1
    fi
  fi

  echo "installing $archive → $BIN_DIR/llm-wiki"
  tar -xzf "$tmp/$archive" -C "$tmp"
  mkdir -p "$BIN_DIR"
  install -m 0755 "$tmp/llm-wiki" "$BIN_DIR/llm-wiki" 2>/dev/null \
    || { cp "$tmp/llm-wiki" "$BIN_DIR/llm-wiki"; chmod +x "$BIN_DIR/llm-wiki"; }
  echo "installed $BIN_DIR/llm-wiki ($tag)"
  case "$(uname -s)" in
    Darwin) xattr -d com.apple.quarantine "$BIN_DIR/llm-wiki" 2>/dev/null || true ;;
  esac

  # Skills come from the binary's embedded tree — copy-mode, same target
  # presets as this script (--agents/--cellockai or a DIR).
  local skills_arg=()
  if [[ -n "$SKILLS_FLAG" ]]; then
    skills_arg=("$SKILLS_FLAG")
  else
    skills_arg=("$TARGET_DIR")
  fi
  "$BIN_DIR/llm-wiki" --install-skills "${skills_arg[@]}"

  # Post-install self-test: `status` on a throwaway root must emit one JSON
  # object and exit 0 ({"initialized":false} on a non-wiki dir is a pass).
  local wtmp
  wtmp="$(mktemp -d)"
  if ! "$BIN_DIR/llm-wiki" --root "$wtmp" status | grep -q '"initialized"'; then
    echo "error: post-install self-test failed (llm-wiki status)" >&2
    rm -rf "$wtmp"
    exit 1
  fi
  rm -rf "$wtmp"
}


install_binary

# Optional extractor: markitdown (office/pdf text extraction in wiki-ingest).
# Try uv tool -> pipx -> pip --user; failure is a warning, not fatal. Runs on
# both install paths when a Python toolchain is around to host it.
if command -v python3 >/dev/null 2>&1 && ! command -v markitdown >/dev/null 2>&1; then
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
