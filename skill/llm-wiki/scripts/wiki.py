#!/usr/bin/env python3
"""wiki.py — deterministic core CLI for the llm-wiki skill.

Every subcommand prints a single JSON object on stdout.
Exit codes: 0 ok, 2 usage, 3 validation, 4 conflict/stale, 5 locked.
"""
import argparse
import collections
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from wikicore import claims, compile as wc_compile, contextpack, deps, doctor, dream, graphdata, pages, dirs  # noqa: E402
from wikicore import ids, query, review, sources  # noqa: E402
from wikicore.shorthand import normalize_candidate, parse_candidates_file  # noqa: E402
from wikicore import jev, synthesize  # noqa: E402
from wikicore.yamlite import loads as yamlloads  # noqa: E402
from wikicore.store import Wiki, SKILL_VERSION, init_wiki  # noqa: E402
from wikicore.transaction import ConflictError, LockedError, Transaction, TxnError  # noqa: E402


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


def _fetch_url(url: str, max_bytes: int) -> bytes:
    """Fetch a URL for ingest. http/https only; response capped like --dir."""
    import urllib.parse
    import urllib.request

    scheme = urllib.parse.urlsplit(url).scheme.lower()
    if scheme not in ("http", "https"):
        raise TxnError("only http/https URLs may be ingested (got %r)" % url)

    class _HttpOnlyRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            new_scheme = urllib.parse.urlsplit(newurl).scheme.lower()
            if new_scheme not in ("http", "https"):
                raise TxnError("refusing redirect to non-http(s) URL: %r" % newurl)
            return super().redirect_request(req, fp, code, msg, headers, newurl)

    opener = urllib.request.build_opener(_HttpOnlyRedirect())
    try:
        with opener.open(url, timeout=60) as resp:
            chunks = []
            total = 0
            while True:
                chunk = resp.read(min(1 << 20, max_bytes - total + 1))
                if not chunk:
                    break
                total += len(chunk)
                if total > max_bytes:
                    raise TxnError("response from %s exceeds --max-bytes %d"
                                   % (url, max_bytes))
                chunks.append(chunk)
    except TxnError:
        raise
    except Exception as e:
        raise TxnError("cannot fetch %s: %s" % (url, e))
    return b"".join(chunks)


def cmd_init(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(init_wiki(wiki, args.name or os.path.basename(wiki.root)))
    return 0


def cmd_ingest(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.kind and (args.url or args.dir or args.triage):
        raise TxnError("--kind only applies to --file/--text ingest")
    if args.triage:
        if args.url or args.dir or args.file or args.text is not None:
            raise TxnError("--triage does not combine with --file/--url/--dir/--text")
        if args.normalized_content or args.source_id or args.parser_name:
            raise TxnError("--normalized-content/--source-id/--parser-name are per-file; not valid with --triage")
        try:
            manifest = _load_json_file(args.triage)
        except (OSError, json.JSONDecodeError) as e:
            raise TxnError("cannot read triage manifest %s: %s" % (args.triage, e))
        _emit(sources.ingest_triage(wiki, manifest, force=args.force))
        return 0
    if args.dir:
        if args.normalized_content:
            raise TxnError("--normalized-content is per-file; not valid with --dir")
        _emit(dirs.ingest_dir(wiki, args.dir, include_hidden=args.include_hidden,
                              max_bytes=args.max_bytes, force=args.force))
        return 0
    normalized = None
    if args.normalized_content:
        try:
            with open(args.normalized_content, "r", encoding="utf-8") as f:
                normalized = f.read()
        except (OSError, UnicodeDecodeError) as e:
            raise TxnError("cannot read --normalized-content %s: %s"
                           % (args.normalized_content, e))
    kwargs = {"source_id": args.source_id, "normalized_content": normalized,
              "parser_name": args.parser_name, "force": args.force}
    if args.url:
        data = _fetch_url(args.url, args.max_bytes)
        _emit(sources.ingest(wiki, "url", args.url, data, **kwargs))
    else:
        data = _read_bytes(args)
        ref = args.file or args.text_ref or "inline"
        kind = args.kind or ("text" if args.text is not None else "file")
        _emit(sources.ingest(wiki, kind, ref, data, **kwargs))
    return 0


def cmd_triage(args) -> int:
    if not os.path.isdir(args.dir):
        raise TxnError("not a directory: %s" % args.dir)
    manifest = sources.triage(args.dir)
    if args.out:
        try:
            with open(args.out, "w", encoding="utf-8") as f:
                json.dump(manifest, f, indent=2, ensure_ascii=False)
                f.write("\n")
        except OSError as e:
            raise TxnError("cannot write --out %s: %s" % (args.out, e))
    _emit(manifest)
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


def cmd_compile(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(wc_compile.compile_umbrella(wiki, resume=args.resume))
    return 0


def cmd_stage_candidates(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.file.endswith(".jsonl"):
        manifest = sources.get_manifest(wiki, args.source_id) or {}
        root_origin = manifest.get("root_origin") or args.source_id
        candidates = [normalize_candidate(c, args.source_id, args.source_version,
                                          root_origin)
                      for c in parse_candidates_file(args.file)]
    else:
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


def cmd_page_targets(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    out = synthesize.page_targets(wiki, kind=args.kind,
                                  stale_only=args.stale_only,
                                  min_claims=args.min_claims,
                                  min_sources=args.min_sources)
    if getattr(args, "decide", False) and jev.is_ready():
        # advisory only: heuristic-routed targets get a Jev kind verdict;
        # any failure simply leaves the target without a decision field.
        for t in out["targets"]:
            if not str(t.get("matched_rule") or "").startswith("heuristic:"):
                continue
            preds = sorted({(d.get("predicate") or "")
                            for d in t.get("claim_details", [])})
            state = "subject: %s; predicates: %s" % (
                t["subject"], ", ".join(preds))
            r = jev.ask(state, {"kind": {
                "type": "choice",
                "options": ["concepts", "entities", "procedures",
                            "questions"],
                "instructions": "Which wiki page kind fits this subject "
                                "best?"}})
            if r.get("ok"):
                v = (r["answers"].get("kind") or {}).get("value") or {}
                dist = v.get("distribution") or {}
                conf = dist.get(v.get("pick"), 0.0)
                t["decision"] = {"suggested_kind": v.get("pick"),
                                 "confidence": conf}
    _emit(out)
    return 0


def cmd_write_page(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.text is not None:
        text = args.text
    elif args.file == "-":
        text = sys.stdin.read()
    elif args.file:
        try:
            with open(args.file, "r", encoding="utf-8") as f:
                text = f.read()
        except (OSError, UnicodeDecodeError) as e:
            raise TxnError("cannot read --file %s: %s" % (args.file, e))
    else:
        raise TxnError("provide --file (- for stdin) or --text")
    dep_list = None  # absent everywhere -> write_page auto-derives from links
    if args.deps_file:
        try:
            parsed = _load_json_file(args.deps_file)
        except (OSError, json.JSONDecodeError) as e:
            raise TxnError("cannot parse --deps-file %s: %s" % (args.deps_file, e))
        if isinstance(parsed, dict):
            dep_list = parsed.get("claim_ids")  # page-targets target dict
            if dep_list is None:
                raise TxnError('--deps-file dict must contain "claim_ids"')
        elif isinstance(parsed, list):
            dep_list = parsed
        else:
            raise TxnError("--deps-file must be a JSON list or a page-target dict")
    elif args.deps is not None:
        dep_list = [d.strip() for d in args.deps.split(",") if d.strip()]
    else:
        front = {}
        try:
            front, _body = synthesize._split_front(text)
        except TxnError:
            front = {}
        fm = front.get("deps")
        if isinstance(fm, list):
            dep_list = fm
    base = args.base_revision if args.base_revision is not None else wiki.revision()
    try:
        _emit(synthesize.write_page(wiki, args.artifact, text, dep_list, base,
                                    allow_new=args.allow_new))
    except TxnError as e:
        # --decide: advisory noul verdict rides on the near-dup refusal so the
        # agent sees p(same page) before choosing --allow-new. Never blocks.
        if (getattr(args, "decide", False) and jev.is_ready()
                and str(e).startswith("near-duplicate page")):
            try:
                front, _body = synthesize._split_front(text)
            except TxnError:
                front = {}
            m = re.match(r"near-duplicate page: (\S+)", str(e))
            twin = m.group(1) if m else ""
            state = ("subject: %s; title: %s; proposed: %s; "
                     "existing twin: %s"
                     % (front.get("subject"), front.get("title"),
                        args.artifact, twin))
            r = jev.ask(state, {"same_page": {
                "type": "noul",
                "instructions": "Is the new page genuinely a near-duplicate "
                                "of the existing one — should creation be "
                                "refused?"}})
            if r.get("ok"):
                v = (r["answers"].get("same_page") or {}).get("value")
                if v is not None:
                    e.decision = {"same_page_p": v}
        raise
    return 0


def cmd_page_show(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit(synthesize.page_show(wiki, args.artifact))
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

def cmd_subjects(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    _emit({"subjects": query.subjects(wiki, prefix=args.prefix)})
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
        status = None if args.all else (args.status or "open")
        _emit({"items": review.list_items(wiki, kind=args.kind, status=status),
               "summary": review.summary(wiki)})
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


def _parse_decision_evidence(wiki, specs) -> list:
    """Each spec is source_<id>@<version>; version must exist in the manifest."""
    out = []
    for spec in specs or []:
        sid, sep, ver = spec.partition("@")
        if not sep or not ids.validate(sid) or not sid.startswith("source_") \
                or not ver.isdigit():
            raise TxnError("--evidence refs must look like source_<id>@<version>: %r" % spec)
        manifest = sources.get_manifest(wiki, sid)
        if not manifest:
            raise TxnError("unknown evidence source: %s" % sid)
        version = int(ver)
        if not any(v["version"] == version for v in manifest.get("versions", [])):
            raise TxnError("unknown version %d of %s" % (version, sid))
        out.append({"source_id": sid, "source_version": version,
                    "locator": {"type": "text",
                                "value": manifest.get("origin") or sid}})
    return out


def cmd_decide(args) -> int:
    """Record a project decision (PRD §3): flag-driven decisions/ write path.
    For file-based decision records see `record-decision`."""
    wiki = Wiki(args.root or os.getcwd())
    if not wiki.exists():
        raise TxnError("wiki not initialized; run wiki-init first")
    claim_ids = [c.strip() for c in (args.claims or "").split(",") if c.strip()]
    for cid in claim_ids:
        if not cid.startswith("claim_") or not ids.validate(cid):
            raise TxnError("--claims entries must be claim_<id>: %r" % cid)
        claims.load_claim(wiki, cid)  # TxnError on unknown id
    evidence = _parse_decision_evidence(wiki, args.evidence)
    decision = {
        "id": ids.new("decision"),
        "title": args.title,
        "status": args.status,
        "decided_on": args.decided_on or time.strftime("%Y-%m-%d", time.gmtime()),
        "claims": sorted(set(claim_ids)),
        "evidence": evidence,
    }
    if args.body:
        decision["body"] = args.body
    if args.authority:
        decision["authority"] = {"type": args.authority}
    txn = Transaction(wiki, "wiki-decide")
    claims.save_decision(txn, decision)
    txn.changes = {"decisions_created": 1}
    base = args.base_revision if args.base_revision is not None else wiki.revision()
    receipt = txn.commit(base)
    _emit({"decision": decision, "run_id": receipt["run_id"],
           "revision": receipt["revision"]})
    return 0


def cmd_record_decision(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    data = _load_json_file(args.file)
    _emit(claims.create_decision(wiki, data))
    return 0


def cmd_doctor(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.fix_indexes:
        # Repair mode (design D1, spec §3.1): rebuild every section index +
        # overview.md through the build-pages renderer, then re-lint.
        before = doctor.run(wiki)
        written = pages.rebuild_section_indexes(wiki)
        result = doctor.run(wiki)
        before_codes = collections.Counter(f["code"] for f in before["findings"])
        after_codes = collections.Counter(f["code"] for f in result["findings"])
        result["indexes_written"] = written
        result["repaired"] = {
            code: n for code, n in sorted(
                (code, before_codes[code] - after_codes[code])
                for code in before_codes)
            if n > 0}
    else:
        result = doctor.run(wiki)
    if args.scope:
        result["findings"] = [f for f in result["findings"]
                              if args.scope in json.dumps(f, ensure_ascii=False)]
        result["ok"] = not any(
            f["severity"] == "error" for f in result["findings"])
        result["counts"] = {s: sum(1 for f in result["findings"]
                                   if f["severity"] == s)
                            for s in ("error", "warn", "info")}
    _emit(result)
    return 0 if result["ok"] else 3

def cmd_topics(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    from wikicore import topics as topics_mod
    if args.backfill:
        _emit(topics_mod.backfill(wiki))
        return 0
    reg = topics_mod.load(wiki)
    claim_counts = {}
    for c in claims.list_claims(wiki):
        claim_counts[c["subject"]] = claim_counts.get(c["subject"], 0) + 1
    out = [{"subject": s, "kind": e["kind"], "status": e["status"],
            "title": e["title"], "claim_count": claim_counts.get(s, 0),
            "declared_by": len(e.get("declared_by", [])),
            "auto_registered": bool(e.get("auto_registered"))}
           for s, e in sorted(reg["topics"].items())]
    _emit({"version": reg.get("version", 0), "topics": out})
    return 0


def cmd_graph_data(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    payload = graphdata.graph_payload(wiki)
    text = json.dumps(payload, ensure_ascii=False)
    if args.output:
        with open(args.output, "w") as f:
            f.write(text + "\n")
        _emit({"written": args.output, "nodes": len(payload["nodes"]),
               "links": len(payload["links"])})
    else:
        print(text)
    return 0

def cmd_jev(args) -> int:
    """Read-only Jev advisory helper. --check answers "configured?" and
    always exits 0; --state/--question run one typed query and print the
    normalized ask() result (disabled/failed included — never raises)."""
    if args.check:
        missing = jev.missing_env()
        if missing:
            _emit({"ready": False,
                   "reason": "%s unset" % " and ".join(missing)})
        else:
            _emit({"ready": True, "model": os.environ[jev.MODEL_ENV],
                   "endpoint": jev.endpoint()})
        return 0
    if args.question is None:
        raise TxnError("provide --check or --question <json>")
    try:
        question = json.loads(args.question)
    except json.JSONDecodeError as e:
        raise TxnError("--question is not valid JSON: %s" % e)
    if not isinstance(question, dict):
        raise TxnError("--question must be a JSON object {name: question}")
    _emit(jev.ask(args.state or "", question))
    return 0



def cmd_dream(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.purge:
        _emit(dream.purge(wiki, dry_run=args.dry_run or not args.apply,
                          retention_days=args.retention_days))
        return 0
    _emit(dream.run(wiki, apply=args.apply, phases=args.phase or None))
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
    p.add_argument("--dir", default=None)
    p.add_argument("--include-hidden", action="store_true")
    p.add_argument("--max-bytes", type=int, default=dirs.DEFAULT_MAX_BYTES)
    p.add_argument("--text", default=None)
    p.add_argument("--text-ref", default=None, help="identity label for --text input")
    p.add_argument("--kind", default=None, choices=["session"],
                   help="source kind for --text/--file: 'session' marks episodic session evidence")
    p.add_argument("--source-id", default=None)
    p.add_argument("--normalized-content", default=None, help="agent-produced extraction markdown file")
    p.add_argument("--parser-name", default=None, help="parser label for --normalized-content, e.g. agent-vision")
    p.add_argument("--force", action="store_true", help="bypass sha256 dedup and append a new version")
    p.add_argument("--triage", default=None, metavar="MANIFEST",
                   help="batch-ingest a manifest produced by `wiki.py triage`")
    p.set_defaults(fn=cmd_ingest)

    p = sub.add_parser("triage",
                       help="deterministic ingest proposals for a directory (no LLM)")
    p.add_argument("--dir", required=True, help="directory of files to scan (top level)")
    p.add_argument("--out", default=None,
                   help="also write the manifest JSON to this path (stdout always)")
    p.set_defaults(fn=cmd_triage)

    p = sub.add_parser("status")
    p.set_defaults(fn=cmd_status)

    p = sub.add_parser("compile")
    p.add_argument("--resume", action="store_true")
    p.set_defaults(fn=cmd_compile)

    p = sub.add_parser("compile-plan")
    p.set_defaults(fn=cmd_compile_plan)

    p = sub.add_parser("stage-candidates")
    p.add_argument("--file", required=True, help="candidates JSON or JSONL file")
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

    p = sub.add_parser("page-targets")
    p.add_argument("--kind", default=None,
                   choices=tuple(synthesize.PAGE_KINDS))
    p.add_argument("--stale-only", action="store_true", dest="stale_only")
    p.add_argument("--min-claims", type=int, default=2, dest="min_claims")
    p.add_argument("--min-sources", type=int, default=2, dest="min_sources")
    p.add_argument("--decide", action="store_true",
                   help="attach advisory Jev kind verdicts to heuristic-routed targets (no-op when Jev env is unset)")
    p.set_defaults(fn=cmd_page_targets)
    p = sub.add_parser("write-page")
    p.add_argument("--file", default=None,
                   help="markdown page file, '-' for stdin (body + optional deps frontmatter)")
    p.add_argument("--text", default=None, help="markdown page as a string argument")
    p.add_argument("--artifact", required=True, help="target wiki path, e.g. wiki/concepts/foo.md")
    p.add_argument("--deps-file", default=None,
                   help="JSON file: list of dep strings, or a page-targets target dict")
    p.add_argument("--deps", default=None, help="comma-separated dep list (overrides deps: frontmatter)")
    p.add_argument("--allow-new", action="store_true", dest="allow_new",
                   help="allow creating a page that near-duplicates an existing one (records forced_new: true)")
    p.add_argument("--base-revision", type=int, default=None)
    p.add_argument("--decide", action="store_true",
                   help="attach an advisory Jev same-page verdict to a near-duplicate refusal (no-op when Jev env is unset)")
    p.set_defaults(fn=cmd_write_page)

    p = sub.add_parser("page-show")
    p.add_argument("--artifact", required=True, help="wiki/<kind>/<slug>.md")
    p.set_defaults(fn=cmd_page_show)

    p = sub.add_parser("verify")
    p.set_defaults(fn=cmd_verify)

    p = sub.add_parser("query-prepare")
    p.add_argument("--question", required=True)
    p.add_argument("--as-of", default=None, dest="as_of")
    p.set_defaults(fn=cmd_query_prepare)

    p = sub.add_parser("subjects")
    p.add_argument("--prefix", default=None)
    p.set_defaults(fn=cmd_subjects)

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
                        "mark_superseded|set_scope|set_validity|defer|reopen|"
                        "set_topic_kind")
    p.add_argument("--params", default=None, help="params JSON file")
    p.add_argument("--kind", default=None)
    p.add_argument("--status", default=None)
    p.add_argument("--all", action="store_true",
                   help="review list: include deferred items too")
    p.add_argument("--base-revision", type=int, default=None)
    p.set_defaults(fn=cmd_review)

    p = sub.add_parser("decide", help="record a project decision into decisions/ (flag-driven; see also record-decision)")
    p.add_argument("--title", required=True)
    p.add_argument("--claims", default=None,
                   help="comma-separated claim_<id>s the decision resolves")
    p.add_argument("--evidence", action="append", default=None, metavar="SOURCE@VER",
                   help="evidence ref source_<id>@<version>; repeatable")
    p.add_argument("--authority", default=None,
                   help="authority label, e.g. explicit_project_decision or human_review")
    p.add_argument("--status", default="accepted",
                   choices=("proposed", "accepted", "superseded", "retracted"))
    p.add_argument("--decided-on", dest="decided_on", default=None,
                   help="YYYY-MM-DD (default: today, UTC)")
    p.add_argument("--body", default=None, help="rationale text")
    p.add_argument("--base-revision", type=int, default=None)
    p.set_defaults(fn=cmd_decide)

    p = sub.add_parser("record-decision")
    p.add_argument("--file", required=True, help="decision input JSON file")
    p.set_defaults(fn=cmd_record_decision)

    p = sub.add_parser("doctor")
    p.add_argument("--scope", default=None,
                   help="filter findings to those whose path/detail contains this substring")
    p.add_argument("--fix-indexes", action="store_true", dest="fix_indexes",
                   help="repair section indexes (rebuild index.md/overview.md via build-pages renderer)")
    p.set_defaults(fn=cmd_doctor)

    p = sub.add_parser("graph-data")
    p.add_argument("--output", default=None, help="write JSON to file instead of stdout")
    p.set_defaults(fn=cmd_graph_data)

    p = sub.add_parser("topics")
    p.add_argument("--backfill", action="store_true",
                   help="register existing claim subjects (idempotent)")
    p.set_defaults(fn=cmd_topics)

    p = sub.add_parser(
        "jev",
        help="advisory System One client (env-gated: DECISION_TOKEN/DECISION_MODEL)")
    p.add_argument("--check", action="store_true",
                   help="report Jev readiness (exit 0 either way)")
    p.add_argument("--state", default=None,
                   help="state text for a one-shot query")
    p.add_argument("--question", default=None,
                   help="questions JSON object, e.g. '{\"q\":{\"type\":\"noul\",\"instructions\":\"...\"}}'")
    p.set_defaults(fn=cmd_jev)

    p = sub.add_parser(
        "dream",
        help="corpus reconciliation + GC: archive superseded/expired/dup/orphan data, delete junk")
    p.add_argument("--apply", action="store_true",
                   help="execute the plan (default: dry-run report)")
    p.add_argument("--phase", action="append", default=None,
                   choices=list(dream.PHASES),
                   help="run only this phase (repeatable)")
    p.add_argument("--purge", action="store_true",
                   help="delete _archive entries older than --retention-days and unreferenced")
    p.add_argument("--dry-run", action="store_true", dest="dry_run",
                   help="with --purge: plan only, delete nothing")
    p.add_argument("--retention-days", type=int, default=90,
                   dest="retention_days")
    p.set_defaults(fn=cmd_dream)

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
        payload = {"error": str(e), "exit_code": code}
        decision = getattr(e, "decision", None)
        if decision is not None:
            payload["decision"] = decision
        _emit(payload)
        return code
    except Exception as e:
        # unexpected failure: still emit structured JSON, never a traceback
        _emit({"error": "%s: %s" % (type(e).__name__, e),
               "error_type": type(e).__name__, "exit_code": 3})
        return 3


if __name__ == "__main__":
    sys.exit(main())
