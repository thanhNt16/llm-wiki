#!/usr/bin/env python3
"""wiki.py — deterministic core CLI for the llm-wiki skill.

Every subcommand prints a single JSON object on stdout.
Exit codes: 0 ok, 2 usage, 3 validation, 4 conflict/stale, 5 locked.
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from wikicore import sources  # noqa: E402
from wikicore.store import Wiki, SKILL_VERSION, init_wiki  # noqa: E402
from wikicore.transaction import ConflictError, LockedError, TxnError  # noqa: E402


def _emit(obj) -> None:
    print(json.dumps(obj, indent=2, ensure_ascii=False, default=str))


def _read_bytes(args) -> bytes:
    if args.file:
        with open(args.file, "rb") as f:
            return f.read()
    if args.text is not None:
        return args.text.encode("utf-8")
    raise TxnError("provide --file, --url or --text")


def cmd_init(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(init_wiki(wiki, args.name or os.path.basename(wiki.root)))
    return 0


def cmd_ingest(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.url:
        import urllib.request

        with urllib.request.urlopen(args.url, timeout=60) as resp:
            data = resp.read()
        _emit(sources.ingest(wiki, "url", args.url, data, source_id=args.source_id))
    else:
        data = _read_bytes(args)
        ref = args.file or args.text_ref or "inline"
        kind = "text" if args.text is not None else "file"
        _emit(sources.ingest(wiki, kind, ref, data, source_id=args.source_id))
    return 0


def cmd_status(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if not wiki.exists():
        _emit({"initialized": False})
        return 0
    state = wiki.state()
    counts = {}
    for label, rel in [("sources", "sources"), ("claims", "claims"), ("decisions", "decisions")]:
        p = wiki.p(rel)
        counts[label] = len(os.listdir(p)) if os.path.isdir(p) else 0
    _emit({
        "initialized": True,
        "project": wiki.load_config().get("project"),
        "skill_version": SKILL_VERSION,
        "revision": state["revision"],
        "counts": counts,
        "pending_versions": len(sources.pending_versions(wiki)),
    })
    return 0


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="wiki.py", description="llm-wiki deterministic core")
    ap.add_argument("--root", default=None, help="project root (default: cwd)")
    sub = ap.add_subparsers(dest="command")

    p = sub.add_parser("init")
    p.add_argument("--name", default=None)
    p.set_defaults(fn=cmd_init)

    p = sub.add_parser("ingest")
    p.add_argument("--file", default=None)
    p.add_argument("--url", default=None)
    p.add_argument("--text", default=None)
    p.add_argument("--text-ref", default=None, help="identity label for --text input")
    p.add_argument("--source-id", default=None)
    p.set_defaults(fn=cmd_ingest)

    p = sub.add_parser("status")
    p.set_defaults(fn=cmd_status)

    return ap


EXIT_CODES = {(TxnError,): 3, (ConflictError,): 4, (LockedError,): 5}


def main(argv=None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if not getattr(args, "fn", None):
        parser.print_help()
        return 2
    try:
        return args.fn(args)
    except TxnError as e:
        code = 3
        for types, c in EXIT_CODES.items():
            if isinstance(e, types):
                code = c
        _emit({"error": str(e), "exit_code": code})
        return code


if __name__ == "__main__":
    sys.exit(main())
