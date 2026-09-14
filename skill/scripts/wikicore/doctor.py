"""wiki-doctor: static + semantic diagnostics over the wiki (PRD §37, §24)."""
import json
import os
import re

from . import claims, deps, schema as schema_mod
from .hashing import canonical_json
from .transaction import TxnError

_SCHEMAS = os.path.join(os.path.dirname(__file__), "..", "..", "llm-wiki", "schemas")

_ID_RE = re.compile(r"^[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$")


def _load_schema(name):
    with open(os.path.join(_SCHEMAS, name)) as f:
        return json.load(f)


def run(wiki) -> dict:
    findings = []

    def add(severity, code, detail, fix):
        findings.append({"severity": severity, "code": code, "detail": detail, "fix": fix})

    # --- static: schema validity of every canonical object -----------------
    claim_files = sorted(os.listdir(wiki.p("claims"))) if os.path.isdir(wiki.p("claims")) else []
    for name in claim_files:
        if not (name.startswith("claim_") and name.endswith(".json")):
            continue
        path = wiki.p("claims", name)
        try:
            with open(path) as f:
                data = json.load(f)
        except ValueError as e:
            add("error", "schema_invalid", "claims/%s: %s" % (name, e),
                "restore from operations history or delete (derived views rebuild)")
            continue
        errors = schema_mod.validate(data, _load_schema("claim.schema.json"))
        if errors:
            add("error", "schema_invalid", "claims/%s: %s" % (name, "; ".join(errors)),
                "fix via wiki-review actions or correct the JSON to schema")
        if data.get("id") != name[:-5]:
            add("error", "invalid_id", "claims/%s: id mismatch" % name,
                "rename file or fix id field")

    decision_files = (sorted(os.listdir(wiki.p("decisions")))
                      if os.path.isdir(wiki.p("decisions")) else [])
    for name in decision_files:
        if not (name.startswith("decision_") and name.endswith(".json")):
            continue
        with open(wiki.p("decisions", name)) as f:
            data = json.load(f)
        errors = schema_mod.validate(data, _load_schema("decision.schema.json"))
        if errors:
            add("error", "schema_invalid", "decisions/%s: %s" % (name, "; ".join(errors)),
                "fix decision JSON to schema")

    # --- static: broken references ----------------------------------------
    known_sources = set(os.listdir(wiki.p("sources"))) if os.path.isdir(wiki.p("sources")) else set()
    claim_ids = {name[:-5] for name in claim_files if name.endswith(".json")}
    for c in claims.list_claims(wiki):
        for ev in c.get("evidence", []):
            sid = ev.get("source_id", "")
            if sid.split("@")[0] not in known_sources:
                add("error", "broken_reference",
                    "claim %s cites missing source %s" % (c["id"], sid),
                    "re-ingest the source or correct the citation")
            elif isinstance(ev.get("source_version"), int):
                try:
                    m = wiki.load_json("sources/%s/manifest.json" % sid)
                    if ev["source_version"] > len(m["versions"]):
                        add("error", "broken_reference",
                            "claim %s cites version %d of %s (only %d exist)"
                            % (c["id"], ev["source_version"], sid, len(m["versions"])),
                            "correct the version or re-ingest")
                except Exception:
                    pass
        for sup in c.get("supersedes", []):
            if sup not in claim_ids:
                add("error", "broken_reference",
                    "claim %s supersedes missing claim %s" % (c["id"], sup),
                    "correct or remove the supersedes entry")
    for d in claims.list_decisions(wiki):
        for cid in d.get("claims", []):
            if cid not in claim_ids:
                add("error", "broken_reference",
                    "decision %s references missing claim %s" % (d["id"], cid),
                    "update the decision's claim list")

    # --- static: orphan sources / staging leftovers ------------------------
    for sid in known_sources:
        if not _ID_RE.match(sid):
            add("warn", "invalid_id", "sources/%s is not a valid id" % sid, "rename directory")
        elif sid not in {c for c in claim_ids} and not _source_cited(wiki, sid):
            add("info", "orphan_source", "source %s is not cited by any claim" % sid,
                "run wiki-compile to extract claims, or leave as raw evidence")
    staging = wiki.p(".state/staging")
    if os.path.isdir(staging):
        for entry in sorted(os.listdir(staging)):
            if entry.startswith("run_"):
                add("warn", "staging_leftover", "staging run %s still present" % entry,
                    "inspect then delete .state/staging/%s (failed or abandoned run)" % entry)

    # --- static: stale derived artifacts -----------------------------------
    for artifact in deps.stale_artifacts(wiki):
        add("warn", "stale_dependency", "derived artifact %s is stale" % artifact,
            "rebuild via wiki-compile / build-pages")

    # --- semantic debt (PRD §24) -------------------------------------------
    for c in claims.list_claims(wiki):
        if c["status"] == "accepted":
            indep = claims.independence(wiki, c)
            if indep == 0 and (c.get("authority") or {}).get("type") == "agent_inference":
                add("warn", "unsupported_accepted",
                    "accepted claim %s has no independent evidence" % c["id"],
                    "corroborate, demote to candidate, or reject via wiki-review")
            if not c.get("scope"):
                add("warn", "missing_scope",
                    "accepted claim %s has empty scope" % c["id"],
                    "set scope via wiki-review set-scope")
            if not c.get("valid_from"):
                add("warn", "missing_effective_date",
                    "accepted claim %s has no valid_from" % c["id"],
                    "set validity via wiki-review set-validity")
        if c["status"] == "disputed":
            add("warn", "unresolved_contradiction",
                "claim %s is disputed" % c["id"], "resolve via wiki-review")

    for item in wiki.read_jsonl(".state/review-queue.jsonl"):
        if item.get("status") == "open" and item.get("kind") == "possible_contradiction":
            add("warn", "unresolved_contradiction",
                "open review item: %s" % item["problem"], "resolve via wiki-review")

    # low extraction coverage on text-like sources
    if os.path.isdir(wiki.p("sources")):
        for sid in sorted(known_sources):
            xpath = wiki.p("sources", sid, "extraction.json")
            if not os.path.isfile(xpath):
                continue
            with open(xpath) as f:
                ext = json.load(f)
            cov = (ext.get("extraction_report") or {}).get("coverage", {})
            if cov.get("text") == "not_extracted":
                add("info", "low_extraction_coverage",
                    "source %s text was not extracted" % sid,
                    "ingest with a document parser for semantic extraction")

    severity_rank = {"error": 0, "warn": 1, "info": 2}
    findings.sort(key=lambda f: (severity_rank[f["severity"]], f["code"]))
    ok = not any(f["severity"] == "error" for f in findings)
    return {"ok": ok, "findings": findings,
            "counts": {s: sum(1 for f in findings if f["severity"] == s)
                       for s in ("error", "warn", "info")}}


def _source_cited(wiki, sid: str) -> bool:
    for c in claims.list_claims(wiki):
        for ev in c.get("evidence", []):
            if ev.get("source_id") == sid:
                return True
    return False
