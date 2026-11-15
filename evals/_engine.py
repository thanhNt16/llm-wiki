"""Shared wiki-engine resolution for eval lanes (spec §13 P5).

Every lane that shells out to the wiki engine picks its argv prefix here:

    python  -> [sys.executable, <repo>/skill/llm-wiki/scripts/wiki.py]
    rust    -> [<repo>/rust/target/release/llm-wiki]   (or $LLM_WIKI_BINARY)

Selection order: -e/--engine flag > $LLM_WIKI_ENGINE > "python".
Missing rust binary is a clear error pointing at `cargo build --release`
in rust/ — never a silent fallback to python.

Stdlib only, Python >= 3.9.
"""
import argparse
import os
import sys

EVAL_ROOT = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(EVAL_ROOT, ".."))

WIKI_PY = os.path.join(REPO_ROOT, "skill", "llm-wiki", "scripts", "wiki.py")
RUST_BINARY = os.path.join(REPO_ROOT, "rust", "target", "release", "llm-wiki")

ENGINE_CHOICES = ("python", "rust")


class EngineError(RuntimeError):
    """Raised when the requested engine cannot be resolved."""


def add_engine_arg(ap, default=None):
    """Attach the shared -e/--engine flag to a runner's argparse.

    default=None -> env LLM_WIKI_ENGINE > "python"; an explicit default
    (e.g. "rust") overrides the env entirely.
    """
    ap.add_argument(
        "-e", "--engine", default=None, choices=list(ENGINE_CHOICES),
        help="wiki engine under test (default: $LLM_WIKI_ENGINE or python). "
             "'rust' resolves $LLM_WIKI_BINARY, else "
             "rust/target/release/llm-wiki — build it with "
             "`cargo build --release` in rust/.")


def engine_name(engine_arg=None):
    """Resolve which engine to run: flag value > env > python."""
    name = engine_arg or os.environ.get("LLM_WIKI_ENGINE") or "python"
    if name not in ENGINE_CHOICES:
        raise EngineError(
            "unknown engine %r (choices: %s; flag -e/--engine or "
            "$LLM_WIKI_ENGINE)" % (name, "/".join(ENGINE_CHOICES)))
    return name


def engine_argv(engine_arg=None):
    """argv prefix that invokes the selected engine.

    Returns e.g. ``[sys.executable, ".../wiki.py"]`` for python or
    ``[".../llm-wiki"]`` for rust. Caller appends subcommands/flags.

    Raises EngineError with a build hint when the rust binary is absent.
    """
    name = engine_name(engine_arg)
    if name == "rust":
        binary = os.environ.get("LLM_WIKI_BINARY") or RUST_BINARY
        binary = os.path.abspath(binary)
        if not os.path.isfile(binary) or not os.access(binary, os.X_OK):
            raise EngineError(
                "rust engine not found at %s — build it first: "
                "`cd rust && cargo build --release` (or set "
                "$LLM_WIKI_BINARY to an existing llm-wiki binary)" % binary)
        return [binary]
    if not os.path.isfile(WIKI_PY):
        raise EngineError("python engine missing: %s" % WIKI_PY)
    return [sys.executable, WIKI_PY]


def engine_invocation(engine_arg=None):
    """Human-readable invocation string (for agent instructions/docs)."""
    return " ".join(engine_argv(engine_arg))


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    add_engine_arg(ap)
    args = ap.parse_args()
    try:
        argv = engine_argv(args.engine)
    except EngineError as e:
        print("error: %s" % e, file=sys.stderr)
        return 2
    print("engine=%s argv=%s" % (engine_name(args.engine), argv))
    return 0


if __name__ == "__main__":
    sys.exit(main())
