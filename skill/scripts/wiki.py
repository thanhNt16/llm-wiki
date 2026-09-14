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

from wikicore import claims, compile as wc_compile, contextpack, deps, doctor, pages  # noqa: E402
from wikicore import query, review, sources  # noqa: E402
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


def _load_json_file(path: str):
    with open(path) as f:
        return json.load(f)


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
        "stale_artifacts": len(deps.stale_artifacts(wiki)),
    })
    return 0


def cmd_compile_plan(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(wc_compile.compile_plan(wiki))
    return 0


def cmd_stage_candidates(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    candidates = _load_json_file(args.file)
    if isinstance(candidates, dict):
        candidates = candidates.get("candidates", [])
    report = _load_json_file(args.report) if args.report else {
        "source_id": args.source_id,
        "source_version": args.source_version,
        "coverage": {"text": "complete", "tables": "skipped", "images": "skipped",
                     "diagrams": "skipped", "formulas": "skipped"},
        "warnings": [],
        "parser": {"name": "agent", "version": "in-session"},
    }
    run_id = wc_compile.stage_candidates(wiki, args.source_id, args.source_version,
                                         candidates, report)
    _emit({"run_id": run_id, "staged": len(candidates)})
    return 0


def cmd_reconcile_prepare(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(wc_compile.reconcile_prepare(wiki, args.run))
    return 0


def cmd_reconcile_apply(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    classifications = _load_json_file(args.classifications)
    if isinstance(classifications, dict):
        classifications = classifications.get("classifications", [])
    base = args.base_revision if args.base_revision is not None else wiki.revision()
    _emit(wc_compile.reconcile_apply(wiki, args.run, classifications, base))
    return 0


def cmd_build_pages(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    base = args.base_revision if args.base_revision is not None else wiki.revision()
    _emit(pages.build_pages(wiki, base))
    return 0


def cmd_verify(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    result = pages.verify(wiki)
    _emit(result)
    return 0 if result["ok"] else 3


def cmd_query_prepare(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(query.prepare(wiki, args.question, as_of=args.as_of))
    return 0


def cmd_context_pack(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    out = contextpack.build(wiki, task=args.task or "", budget=args.budget,
                            resume=args.resume, changes_since=args.changes_since)
    if args.print_pack:
        with open(wiki.p(out["pack_path"])) as f:
            print(f.read())
    else:
        _emit(out)
    return 0


def cmd_review(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.review_cmd == "list":
        _emit({"items": review.list_items(wiki, kind=args.kind,
                                          status=args.status or "open")})
        return 0
    if args.review_cmd == "show":
        _emit(review.show(wiki, args.item))
        return 0
    if args.review_cmd == "act":
        params = _load_json_file(args.params) if args.params else {}
        base = args.base_revision if args.base_revision is not None else wiki.revision()
        out = review.act(wiki, args.item, args.action, params, base)
        _emit({"item": out["item"], "run_id": out["run_id"]})
        return 0
    raise TxnError("review sub-command required: list|show|act")


def cmd_doctor(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    result = doctor.run(wiki)
    _emit(result)
    return 0 if result["ok"] else 3


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

    p = sub.add_parser("compile-plan")
    p.set_defaults(fn=cmd_compile_plan)

    p = sub.add_parser("stage-candidates")
    p.add_argument("--file", required=True, help="candidates JSON file")
    p.add_argument("--source-id", required=True)
    p.add_argument("--source-version", required=True, type=int)
    p.add_argument("--report", default=None, help="extraction report JSON file")
    p.set_defaults(fn=cmd_stage_candidates)

    p = sub.add_parser("reconcile-prepare")
    p.add_argument("--run", required=True)
    p.set_defaults(fn=cmd_reconcile_prepare)

    p = sub.add_parser("reconcile-apply")
    p.add_argument("--run", required=True)
    p.add_argument("--classifications", required=True, help="classifications JSON file")
    p.add_argument("--base-revision", type=int, default=None)
    p.set_defaults(fn=cmd_reconcile_apply)

    p = sub.add_parser("build-pages")
    p.add_argument("--base-revision", type=int, default=None)
    p.set_defaults(fn=cmd_build_pages)

    p = sub.add_parser("verify")
    p.set_defaults(fn=cmd_verify)

    p = sub.add_parser("query-prepare")
    p.add_argument("--question", required=True)
    p.add_argument("--as-of", default=None, dest="as_of")
    p.set_defaults(fn=cmd_query_prepare)

    p = sub.add_parser("context-pack")
    p.add_argument("--task", default="")
    p.add_argument("--budget", type=int, default=None)
    p.add_argument("--resume", action="store_true")
    p.add_argument("--changes-since", default=None)
    p.add_argument("--print-pack", action="store_true")
    p.set_defaults(fn=cmd_context_pack)

    p = sub.add_parser("review")
    p.add_argument("review_cmd", nargs="?", default=None)
    p.add_argument("--item", default=None)
    p.add_argument("--action", default=None,
                   help="accept|reject|merge|mark_duplicate|mark_authoritative|"
                        "mark_superseded|set_scope|set_validity|defer")
    p.add_argument("--params", default=None, help="params JSON file")
    p.add_argument("--kind", default=None)
    p.add_argument("--status", default=None)
    p.add_argument("--base-revision", type=int, default=None)
    p.set_defaults(fn=cmd_review)

    p = sub.add_parser("doctor")
    p.set_defaults(fn=cmd_doctor)

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
