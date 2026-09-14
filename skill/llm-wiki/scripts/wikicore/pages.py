"""Derived views: deterministic page builders + verification gates.

Concept/procedure prose is written by the agent for invalidated pages only;
index/decision/changes stubs are built here. Every derived page carries
yamlite frontmatter with its dependency set (PRD §17). Derived state is
disposable — deleting wiki/ and re-running build_pages restores it (PRD §8.2).
"""
import os
import re
import time

from . import claims, deps, ids
from .transaction import Transaction, TxnError
from .yamlite import dumps as yamldumps, loads as yamlloads

LINK_RE = re.compile(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]")


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _page(rel: str, front: dict, body: str) -> str:
    return "---\n%s\n---\n\n%s" % (yamldumps(front), body.strip() + "\n")


def build_pages(wiki, base_revision: int) -> dict:
    """(Re)build deterministic views and register their dependency sets."""
    txn = Transaction(wiki, "wiki-build-pages")
    built = []

    all_claims = claims.list_claims(wiki)
    decisions = claims.list_decisions(wiki)
    claim_versions = {c["id"]: c["version"] for c in all_claims}

    # --- index.md ---------------------------------------------------------
    by_subject = {}
    for c in all_claims:
        if c["status"] in ("accepted", "provisional"):
            by_subject.setdefault(c["subject"], []).append(c)
    lines = ["# Wiki index", "", "| Subject | Accepted claims |", "|---|---|"]
    for subject in sorted(by_subject):
        lines.append("| %s | %d |" % (subject, len(by_subject[subject])))
    lines += ["", "## Decisions", "", "| Decision | Status |", "|---|---|"]
    for d in sorted(decisions, key=lambda x: x["id"]):
        lines.append("| [%s](decisions/%s.md) | %s |" % (d["title"], d["id"], d["status"]))
    front = {"type": "index", "generated": _now(), "deps": []}
    txn.stage_write("wiki/index.md", _page("wiki/index.md", front, "\n".join(lines)))
    built.append("wiki/index.md")
    deps.register(txn, "wiki/index.md", [])

    # --- decision pages ---------------------------------------------------
    for d in decisions:
        deps_list = ["%s@%d" % (d["id"], d["version"])]
        rows = ["| Subject | Predicate | Value | Scope | Status |", "|---|---|---|---|---|"]
        for cid in d.get("claims", []):
            try:
                c = claims.load_claim(wiki, cid)
            except TxnError:
                continue
            deps_list.append("%s@%d" % (c["id"], c["version"]))
            scope = ", ".join("%s=%s" % kv for kv in sorted((c.get("scope") or {}).items())) or "-"
            rows.append("| %s | %s | %s | %s | %s |" % (
                c["subject"], c["predicate"], c.get("value"), scope, c["status"]))
        body = ["# %s" % d["title"], "", "Status: **%s**" % d["status"], ""]
        if d.get("body"):
            body += [d["body"], ""]
        body += ["## Supporting claims", ""] + rows
        rel = "wiki/decisions/%s.md" % d["id"]
        front = {"type": "decision", "decision": d["id"], "generated": _now(), "deps": deps_list}
        txn.stage_write(rel, _page(rel, front, "\n".join(body)))
        built.append(rel)
        deps.register(txn, rel, deps_list)

    # agent-authored concept/procedure/question pages: rebuild if stale
    for sub in ("concepts", "entities", "procedures", "questions"):
        subdir = wiki.p("wiki", sub)
        if not os.path.isdir(subdir):
            continue
        for name in sorted(os.listdir(subdir)):
            if not name.endswith(".md"):
                continue
            rel = "wiki/%s/%s" % (sub, name)
            if deps.is_stale(wiki, rel):
                # stale agent page: rebuild placeholder preserving frontmatter deps
                with open(os.path.join(subdir, name)) as f:
                    text = f.read()
                front = yamlloads(text.split("---\n")[1] if text.startswith("---\n") else "")
                front["stale"] = True
                txn.stage_write(rel, _page(rel, front,
                                           "(stale — regenerate from current claims)"))
                built.append(rel)

    txn.changes = {"pages_built": len(built)}
    receipt = txn.commit(base_revision)

    # clear stale flags for artifacts we just rebuilt
    cleared = [a for a in deps.stale_artifacts(wiki) if a in built]
    if cleared:
        txn2 = Transaction(wiki, "wiki-build-pages-clear")
        deps.clear_stale(txn2, cleared)
        txn2.commit(wiki.revision())
    receipt["changes"]["pages_built"] = len(built)
    return receipt


def verify(wiki) -> dict:
    """Citation + staleness gates (PRD §58: every citation resolves; stale
    derived artifacts must not silently remain current)."""
    errors = []
    g = deps.graph(wiki)
    edges = g.get("edges", {})
    stale = set(g.get("stale", []))

    known_ids = {c["id"]: c["version"] for c in claims.list_claims(wiki)}
    known_ids.update({d["id"]: d["version"] for d in claims.list_decisions(wiki)})

    for artifact, dep_list in edges.items():
        path = wiki.p(artifact)
        if not os.path.isfile(path):
            errors.append("missing artifact: %s" % artifact)
            continue
        if artifact in stale:
            errors.append("stale artifact not rebuilt: %s" % artifact)
        if artifact == "wiki/index.md":
            continue
        with open(path) as f:
            text = f.read()
        if not text.startswith("---\n"):
            errors.append("missing frontmatter: %s" % artifact)
            continue
        front = yamlloads(text.split("---\n")[1] if "\n---\n" in text else text)
        for dep in front.get("deps", []) or []:
            dep_id = dep.split("@", 1)[0]
            if dep_id not in known_ids:
                errors.append("unresolved dependency %r in %s" % (dep, artifact))
        for target in LINK_RE.findall(text.split("---", 2)[-1]):
            if target not in known_ids:
                errors.append("unresolved wiki-link [[%s]] in %s" % (target, artifact))

    # every derived page must be registered with a dependency set (PRD §17)
    for sub in ("concepts", "entities", "procedures", "questions", "decisions"):
        subdir = wiki.p("wiki", sub)
        if not os.path.isdir(subdir):
            continue
        for name in sorted(os.listdir(subdir)):
            if not name.endswith(".md"):
                continue
            rel = "wiki/%s/%s" % (sub, name)
            if rel not in edges:
                errors.append("derived page missing dependency registration: %s" % rel)

    return {"ok": not errors, "errors": errors, "stale": sorted(stale)}
