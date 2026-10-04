"""Read-only knowledge-graph payload for the wiki-visualize skill.

`wiki.py graph-data` prints one JSON object: entity/source/decision/page
nodes and typed links derived from .llm-wiki/claims, .llm-wiki/decisions,
.llm-wiki/sources. The wiki-visualize app fetches this payload and renders it;
layout happens client-side, so no x/y/z here. Deterministic: identical wiki
state -> byte-identical output except `generated_at`.
"""
import json
import math
import os
import re
import time

from . import claims, deps, review, synthesize
from .text import norm_tokens
from .transaction import TxnError

TICKET_RE = re.compile(r"^(?:stc|pm)-?\d+$", re.I)

MENTION_STOP = {
    "stock", "workflow", "part", "build", "order", "item", "items", "detail",
    "list", "modal", "api", "ui", "feature", "status", "field", "source",
    "data", "page", "tab", "table", "column", "po", "bo", "service",
}
# Alias words too generic to imply a "mentions" edge. The module constant is
# the default; setting the MENTION_STOP env var (comma-separated words)
# replaces it for a run (Q8).

SUMMARY_PREDICATES = ("status", "summary", "verdict", "feature", "title")


def _mention_stop() -> set:
    env = os.environ.get("MENTION_STOP")
    if env is None:
        return set(MENTION_STOP)
    return {w.strip().lower() for w in env.split(",") if w.strip()}


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

    # Agent-authored wiki/<kind>/*.md pages; deterministic: sorted by path.
    pages = []
    page_ids = {}
    page_rels = []
    for kind in synthesize.PAGE_KINDS:
        pdir = wiki.p("wiki", kind)
        if os.path.isdir(pdir):
            for name in os.listdir(pdir):
                if name.endswith(".md") and name != "index.md":
                    page_rels.append("wiki/%s/%s" % (kind, name))
    for rel in sorted(page_rels):
        with open(wiki.p(rel)) as f:
            front, _ = synthesize._split_front(f.read())
        singular = synthesize.KIND_TYPE[rel.split("/")[1]]
        idx = len(nodes) + len(src_nodes) + len(dec_nodes) + len(pages)
        page_ids[rel] = idx
        summary = [
            {"p": "type", "v": singular},
            {"p": "stale", "v": bool(front.get("stale", False))},
        ]
        if front.get("title"):
            summary.append({"p": "title", "v": front["title"]})
        pages.append({
            "id": idx,
            "kind": "page",
            "key": rel,
            "label": front.get("title") or os.path.basename(rel)[:-3],
            "wtype": "page_" + singular,
            "claim_count": 0,
            "size": 5.0,
            "summary": summary,
            "claims": [],
            "sources": [],
            "path": rel,
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

    # Alias matching (Q1/Q8): normalized token overlap, not a raw prefix
    # regex. Alias = second key segment minus project-qualifying prefix,
    # minimum 4 chars, filtered through the stop list; word boundaries come
    # free from tokenization, and morphology matches inflected forms
    # ("orders" hits alias "order").
    stop = _mention_stop()
    alias_stems = {}
    for key, lst in by_ent.items():
        seg = key.split(".")[1] if "." in key else key
        alias = seg.lower()
        if project:
            low = project.lower()
            for sep in ("_", "-", "."):
                if alias.startswith(low + sep):
                    alias = alias[len(low) + 1:]
                    break
            if alias == low:
                alias = ""
        # stop membership is checked on canonical stems so "orders" is
        # filtered exactly like "order"
        stems = norm_tokens(alias)
        if len(alias) >= 4 and stems and not (stems & stop) and len(lst) >= 2:
            alias_stems[key] = stems
    for key, lst in by_ent.items():
        blob = " ".join(
            str(c.get("predicate", "")) + " " + str(c.get("value", ""))
            for c in lst
        ).lower()
        tokens = re.findall(r"[a-z0-9_]+", blob)
        for other, stem in alias_stems.items():
            if other == key:
                continue
            hits = sum(1 for t in tokens if norm_tokens(t) & stem)
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

    dep_edges = deps.graph(wiki).get("edges", {})
    for artifact in sorted(dep_edges):
        page_id = page_ids.get(artifact)
        if page_id is None:
            continue
        for dep in dep_edges[artifact]:
            dep_id = dep.split("@", 1)[0]
            if dep_id.startswith("claim_"):
                ent = claim_to_ent.get(dep_id)
                if ent in ent_ids:
                    add(ent_ids[ent], page_id, "documents")
            elif dep_id.startswith("decision_"):
                if dep_id in dec_ids:
                    add(dec_ids[dep_id], page_id, "documents")
            elif dep_id in page_ids:
                add(page_ids[dep_id], page_id, "depends_on")

    all_nodes = nodes + src_nodes + dec_nodes + pages

    # Review-surface annotations (R3): per-node flags plus the queue summary
    # under meta. Entity nodes carry claim dicts; decision nodes reference
    # claim ids; pages/sources have none.
    open_affected = set()
    for item in wiki.read_jsonl(review.QUEUE):
        if item.get("status") == "open":
            open_affected |= {a.split("@", 1)[0] for a in item.get("affected", [])}
    claim_status = {c["id"]: c.get("status", "") for c in all_claims}
    dec_status = {d.get("id", ""): d.get("status", "") for d in decisions}
    degree = {}
    for l in links:
        degree[l["source"]] = degree.get(l["source"], 0) + 1
        degree[l["target"]] = degree.get(l["target"], 0) + 1
    for n in all_nodes:
        raw = n.get("claims") or []
        if raw and isinstance(raw[0], dict):
            cids = {c["id"] for c in raw}
            statuses = {c.get("status", "") for c in raw}
        else:
            cids = set(raw)
            statuses = {claim_status.get(c, "") for c in cids}
            if n["kind"] == "decision":
                statuses.add(dec_status.get(n["key"], ""))
        terminal = statuses & set(claims.TERMINAL)
        n["flags"] = {
            "review_open": bool(cids & open_affected),
            "disputed": "disputed" in statuses,
            "superseded": "superseded" in statuses,
            "stale": bool(terminal) and terminal == statuses - {""},
            "orphan": degree.get(n["id"], 0) == 0,
        }

    generated_at = _now()
    return {
        "project": project,
        "nodes": all_nodes,
        "links": links,
        "claim_count": len(all_claims),
        "source_count": len(src_nodes),
        "decision_count": len(dec_nodes),
        "page_count": len(pages),
        "generated_at": generated_at,
        "meta": {"review": review.summary(wiki), "generated_at": generated_at},
        "warnings": warnings,
    }
