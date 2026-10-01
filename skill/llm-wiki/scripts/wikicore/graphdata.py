"""Read-only knowledge-graph payload for the wiki-visualize skill.

`wiki.py graph-data` prints one JSON object: entity/source/decision nodes and
typed links derived from .llm-wiki/claims, .llm-wiki/decisions and
.llm-wiki/sources. The wiki-visualize app fetches this payload and renders it;
layout happens client-side, so no x/y/z here. Deterministic: identical wiki
state -> byte-identical output except `generated_at`.
"""
import json
import math
import os
import re
import time
from typing import Optional

from . import claims
from .transaction import TxnError

TICKET_RE = re.compile(r"^(?:stc|pm)-?\d+$", re.I)

MENTION_STOP = {
    "stock", "workflow", "part", "build", "order", "item", "items", "detail",
    "list", "modal", "api", "ui", "feature", "status", "field", "source",
    "data", "page", "tab", "table", "column", "po", "bo", "service",
}

SUMMARY_PREDICATES = ("status", "summary", "verdict", "feature", "title")


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _entity_key(subject: str) -> str:
    return ".".join(subject.split(".")[:2])


def _summarize(clist: list) -> list:
    pri = [c for c in clist if c["predicate"] in SUMMARY_PREDICATES]
    pick = (pri + [c for c in clist if c not in pri])[:6]
    seen = {}
    for c in pick:
        seen[c["predicate"]] = c["value"]  # latest wins
    return [{"p": k, "v": v} for k, v in list(seen.items())[:4]]


def _load_type_map(wiki, warnings: list) -> dict:
    path = wiki.p("graph-types.json")
    if not os.path.isfile(path):
        return {}
    try:
        with open(path) as f:
            m = json.load(f)
        if not isinstance(m, dict) or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in m.items()
        ):
            raise ValueError("expected object of string->string")
        return m
    except (OSError, ValueError, json.JSONDecodeError) as e:
        warnings.append(
            "graph-types.json ignored: %s" % e
        )
        return {}


def _wtype(key: str, type_map: dict) -> str:
    seg = key.split(".")[1] if "." in key else key
    if seg in type_map:
        return type_map[seg]
    if TICKET_RE.match(seg):
        return "ticket"
    return "domain"


def _size(kind: str, claim_count: int) -> float:
    if kind == "source":
        return 5.0
    if kind == "decision":
        return 7.0
    return max(4.0, 6.0 * math.sqrt(max(1, claim_count)))


def _list_sources(wiki) -> list:
    sdir = wiki.p("sources")
    out = []
    if not os.path.isdir(sdir):
        return out
    for name in sorted(os.listdir(sdir)):
        mpath = os.path.join(sdir, name, "manifest.json")
        if not os.path.isfile(mpath):
            continue
        try:
            out.append(wiki.load_json("sources/%s/manifest.json" % name))
        except (OSError, json.JSONDecodeError):
            continue
    return out


def graph_payload(wiki) -> dict:
    if not wiki.exists():
        raise TxnError("wiki not initialized: run wiki init first")
    warnings = []
    cfg = wiki.load_config()
    project = cfg.get("project") or ""

    all_claims = claims.list_claims(wiki)
    all_claims.sort(key=lambda c: c.get("id", ""))
    decisions = claims.list_decisions(wiki)
    decisions.sort(key=lambda d: d.get("id", ""))
    manifests = _list_sources(wiki)
    type_map = _load_type_map(wiki, warnings)

    # Group claims by entity key; superseded stay in claims but carry no edges.
    by_ent = {}
    for c in all_claims:
        by_ent.setdefault(_entity_key(c.get("subject", "")), []).append(c)
    live_claims = [c for c in all_claims if c.get("status") != "superseded"]
    live_by_ent = {}
    for c in live_claims:
        live_by_ent.setdefault(_entity_key(c.get("subject", "")), []).append(c)
    claim_to_ent = {
        c["id"]: _entity_key(c.get("subject", "")) for c in all_claims
    }

    # Labels: strip "<project>." prefix; on collision keep the full key.
    raw_labels = {}
    for key in by_ent:
        if project and key.startswith(project + "."):
            raw_labels[key] = key[len(project) + 1:]
        else:
            raw_labels[key] = key
    counts = {}
    for lbl in raw_labels.values():
        counts[lbl] = counts.get(lbl, 0) + 1
    labels = {
        k: (k if counts[v] > 1 else v) for k, v in raw_labels.items()
    }

    nodes = []
    ent_ids = {}
    for i, key in enumerate(sorted(by_ent)):
        ent_ids[key] = i
        clist = by_ent[key]
        cited = sorted({
            ev["source_id"]
            for c in clist
            for ev in c.get("evidence", [])
            if ev.get("source_id")
        })
        auth = sorted({
            (c.get("authority") or {}).get("source", "")
            for c in clist
            if (c.get("authority") or {}).get("source")
        })
        nodes.append({
            "id": i,
            "kind": "entity",
            "key": key,
            "label": labels[key],
            "wtype": _wtype(key, type_map),
            "claim_count": len(clist),
            "size": _size("entity", len(clist)),
            "summary": _summarize(clist),
            "claims": clist,
            "sources": cited + [a for a in auth if a not in cited],
        })

    src_ids = {}
    src_nodes = []
    for m in manifests:
        sid = m.get("id", "")
        if not sid:
            continue
        idx = len(nodes) + len(src_nodes)
        src_ids[sid] = idx
        title = m.get("title") or os.path.basename(str(m.get("origin", sid)))
        src_nodes.append({
            "id": idx,
            "kind": "source",
            "key": sid,
            "label": title,
            "wtype": "source",
            "claim_count": 0,
            "size": _size("source", 0),
            "summary": [{"p": "document", "v": m.get("origin", "")}],
            "claims": [],
            "sources": [sid],
            "title": title,
            "origin": m.get("origin", ""),
        })

    dec_nodes = []
    dec_ids = {}
    for d in decisions:
        idx = len(nodes) + len(src_nodes) + len(dec_nodes)
        did = d.get("id", "")
        dec_ids[did] = idx
        dec_nodes.append({
            "id": idx,
            "kind": "decision",
            "key": did,
            "label": d.get("title", did),
            "wtype": "decision",
            "claim_count": 0,
            "size": _size("decision", 0),
            "summary": [
                {"p": "status", "v": d.get("status", "")},
                {"p": "decided_on", "v": d.get("decided_on")},
            ],
            "claims": d.get("claims", []),
            "sources": sorted({
                ev.get("source_id", "")
                for ev in d.get("evidence", [])
                if ev.get("source_id")
            }),
        })

    links = []
    eset = set()

    def add(a: int, b: int, t: str, w: int = 1):
        if a == b or (a, b, t) in eset:
            return
        eset.add((a, b, t))
        links.append({"source": a, "target": b, "type": t, "w": w})

    for key in live_by_ent:
        for sid in {
            ev["source_id"]
            for c in live_by_ent[key]
            for ev in c.get("evidence", [])
        }:
            if sid in src_ids:
                add(ent_ids[key], src_ids[sid], "evidence")

    for key in by_ent:
        parent = ".".join(key.split(".")[:-1])
        if parent in by_ent and parent != key:
            add(ent_ids[parent], ent_ids[key], "contains")

    alias_rx = {}
    for key, lst in by_ent.items():
        seg = key.split(".")[1] if "." in key else key
        alias = seg.lower()
        if len(alias) >= 5 and alias not in MENTION_STOP and len(lst) >= 2:
            alias_rx[key] = re.compile(r"\b%s" % re.escape(alias), re.I)
    for key, lst in by_ent.items():
        blob = " ".join(
            str(c.get("predicate", "")) + " " + str(c.get("value", ""))
            for c in lst
        ).lower()
        for other, rx in alias_rx.items():
            if other == key:
                continue
            hits = len(rx.findall(blob))
            if hits >= 2:
                add(ent_ids[other], ent_ids[key], "mentions", hits)

    for d in decisions:
        did = d.get("id", "")
        for cid in d.get("claims", []):
            ent = claim_to_ent.get(cid)
            if ent in ent_ids and did in dec_ids:
                add(dec_ids[did], ent_ids[ent], "decision")
        for ev in d.get("evidence", []):
            sid = ev.get("source_id", "")
            if sid in src_ids and did in dec_ids:
                add(dec_ids[did], src_ids[sid], "evidence")

    all_nodes = nodes + src_nodes + dec_nodes
    return {
        "project": project,
        "nodes": all_nodes,
        "links": links,
        "claim_count": len(all_claims),
        "source_count": len(src_nodes),
        "decision_count": len(dec_nodes),
        "generated_at": _now(),
        "warnings": warnings,
    }
