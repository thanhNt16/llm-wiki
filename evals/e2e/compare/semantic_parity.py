#!/usr/bin/env python3
"""Semantic parity under each build mode: compare the USER-VISIBLE wiki
state (claims multiset, source manifests, wiki pages) rather than the
.llm-wiki/.state internals, which are legitimately implementation-specific.

Claim identity = (subject, predicate, value, scope) so differing ULID
allocation order does not register as a diff. References (evidence source,
supersedes, root_origin, page deps/owner) are re-expressed via that
identity so referential integrity is still checked.

Exit 0 = semantic parity on every mode. Findings (implementation-level
differences observed but not semantic diffs) are always reported.
"""
import glob
import json
import os
import re
import sys
import tempfile
import shutil

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.abspath(os.path.join(_HERE, "..", "..", ".."))
for p in ("evals/e2e", "evals", "evals/benchmarks"):
    sys.path.insert(0, os.path.join(_REPO, p))

import _engine  # noqa: E402
import assertions  # noqa: E402
import run_e2e  # noqa: E402
from driver import Engine  # noqa: E402

MODES = ("init+inbox", "build", "build_uncorrected")
VOLATILE_CLAIM = {"id", "recorded_at", "acceptance"}
VOLATILE_MANIFEST = {"id", "created_at", "origin", "root_origin"}
VOLATILE_VERSION = {"captured_at", "normalized_path", "raw_path"}
TS_RE = re.compile(r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b")
RUN_RE = re.compile(r"\brun_[0-9A-HJKMNP-TV-Z]{26}\b")


def load_json(path):
    with open(path) as f:
        return json.load(f)


def claim_key(c):
    return json.dumps([c.get("subject"), c.get("predicate"),
                       c.get("value"), c.get("scope") or {}],
                      sort_keys=True)


def source_key(m):
    """Identity for a source: title + first version sha."""
    vers = m.get("versions") or [{}]
    return "%s|%s" % (m.get("title"), vers[0].get("sha256"))


def canon_claim(c, src_map, claim_map):
    """Claim with ids replaced by semantic keys."""
    d = {k: v for k, v in c.items() if k not in VOLATILE_CLAIM}
    d["evidence"] = [
        {**{k: v for k, v in ev.items() if k != "source_id"},
         "source": src_map.get(ev.get("source_id"), ev.get("source_id"))}
        for ev in c.get("evidence", [])
    ]
    d["supersedes"] = sorted(claim_map.get(t, t)
                             for t in c.get("supersedes", []))
    d["root_origin"] = src_map.get(c.get("root_origin"),
                                 c.get("root_origin"))
    if c.get("superseded_by"):
        d["superseded_by"] = claim_map.get(c["superseded_by"],
                                         c["superseded_by"])
    return d


def canon_manifest(m):
    d = {k: v for k, v in m.items() if k not in VOLATILE_MANIFEST}
    d["versions"] = [
        {k: v for k, v in ver.items() if k not in VOLATILE_VERSION}
        for ver in d.get("versions", [])
    ]
    return d


def canon_page(text, src_map, claim_map):
    """Split frontmatter/body; parse fm lines 'k: v' + '- item' lists."""
    fm, body = {}, text
    m = re.match(r"^---\n(.*?)\n---\n", text, re.S)
    if m:
        body = text[m.end():]
        cur = None
        for line in m.group(1).splitlines():
            if line.startswith("  - ") or line.startswith("- "):
                if cur is not None:
                    if not isinstance(fm.get(cur), list):
                        fm[cur] = []
                    fm[cur].append(line.split("- ", 1)[1])
            elif ":" in line:
                cur, _, val = line.partition(":")
                fm[cur.strip()] = val.strip() or None
    def sub_ids(s):
        if not isinstance(s, str):
            return s
        for cid, key in claim_map.items():
            s = s.replace(cid, "claim[%s]" % key)
        for sid, key in src_map.items():
            s = s.replace(sid, "source[%s]" % key)
        return s
    fm = {k: ([sub_ids(i) for i in v] if isinstance(v, list)
              else sub_ids(v)) for k, v in fm.items()
          if k not in ("generated", "created", "updated")}
    body = RUN_RE.sub("run#", sub_ids(TS_RE.sub("<TS>", body)))
    return {"frontmatter": fm, "body": body}


def collect(project):
    store = os.path.join(project, ".llm-wiki")
    mans = {}
    for p in glob.glob(os.path.join(store, "sources", "*", "manifest.json")):
        m = load_json(p)
        mans[m["id"]] = m
    src_map = {sid: source_key(m) for sid, m in mans.items()}
    claims = {}
    for p in glob.glob(os.path.join(store, "claims", "claim_*.json")):
        c = load_json(p)
        claims[c["id"]] = c
    claim_map = {cid: claim_key(c) for cid, c in claims.items()}
    return {
        "claims": sorted(
            json.dumps(canon_claim(c, src_map, claim_map), sort_keys=True)
            for c in claims.values()),
        "sources": sorted(
            json.dumps(canon_manifest(m), sort_keys=True)
            for m in mans.values()),
        "pages": {
            os.path.relpath(p, store): canon_page(
                open(p).read(), src_map, claim_map)
            for p in glob.glob(os.path.join(store, "wiki", "**", "*.md"),
                               recursive=True)
        },
        "inbox": sorted(os.listdir(os.path.join(project, "inbox")))
        if os.path.isdir(os.path.join(project, "inbox")) else None,
    }


def diff_set(name, a, b):
    out = []
    sa, sb = set(a), set(b)
    for x in sorted(sa - sb):
        out.append("%s only-in-python: %s" % (name, x[:400]))
    for x in sorted(sb - sa):
        out.append("%s only-in-rust:   %s" % (name, x[:400]))
    return out


def main():
    engines = (("python", Engine(assertions.engine_path())),
               ("rust", run_e2e._CliEngine(_engine.engine_argv("rust"))))
    report, any_diff = {}, False
    parent = tempfile.mkdtemp(prefix="e2e-sem-")
    try:
        for mode in MODES:
            states = {}
            for name, eng in engines:
                proj = os.path.join(parent, "%s-%s" % (mode, name))
                os.makedirs(proj)
                shutil.copytree(run_e2e.CORPUS, proj, dirs_exist_ok=True)
                run_e2e.setup_project(mode, eng, proj)
                states[name] = collect(proj)
            diffs = []
            for sect in ("claims", "sources"):
                diffs += diff_set(sect, states["python"][sect],
                                  states["rust"][sect])
            pa, pb = states["python"]["pages"], states["rust"]["pages"]
            for k in sorted(set(pa) | set(pb)):
                if k not in pa:
                    diffs.append("page only-in-rust: %s" % k)
                elif k not in pb:
                    diffs.append("page only-in-python: %s" % k)
                elif pa[k] != pb[k]:
                    diffs.append("page diff %s: py=%s rust=%s"
                                 % (k, json.dumps(pa[k], sort_keys=True)[:300],
                                    json.dumps(pb[k], sort_keys=True)[:300]))
            if states["python"]["inbox"] != states["rust"]["inbox"]:
                diffs.append("inbox differs: %s vs %s"
                             % (states["python"]["inbox"],
                                states["rust"]["inbox"]))
            report[mode] = {
                "claims": len(states["python"]["claims"]),
                "sources": len(states["python"]["sources"]),
                "pages": len(pa),
                "semantic_parity": not diffs,
                "diffs": diffs,
            }
            print("[%s] %s  claims=%d sources=%d pages=%d diffs=%d"
                  % (mode, "SEMANTIC PARITY" if not diffs else "DIVERGENCE",
                     report[mode]["claims"], report[mode]["sources"],
                     report[mode]["pages"], len(diffs)))
            for d in diffs[:40]:
                print("   " + d)
            any_diff = any_diff or bool(diffs)
    finally:
        shutil.rmtree(parent, ignore_errors=True)
    out = os.path.join(_HERE, "semantic-parity.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print("wrote %s" % out)
    return 1 if any_diff else 0


if __name__ == "__main__":
    sys.exit(main())
