"""One-command static graph export (SP2b #6).

Copies the prebuilt wiki-visualize UI (assets/graph-ui/dist) plus a freshly
generated full graph-data payload into a target directory — openwiki's
`visualize --export` contract, mirrored. Deterministic for a given store
(except generated_at); destination is replaced atomically, never partially.
"""
import json
import os
import shutil
import tempfile

from . import graphdata
from .transaction import TxnError

# Candidate UI template roots, tried in order: packaged sibling of the
# llm-wiki skill (repo checkout and installed skill dir share this shape).
_TEMPLATE_CANDIDATES = (
    "../../../wiki-visualize/assets/graph-ui/dist",
)


def ui_dist(explicit=None) -> str:
    """Resolve the prebuilt UI template dir."""
    cands = []
    if explicit:
        cands.append(explicit)
    env = os.environ.get("WIKI_UI_DIST")
    if env:
        cands.append(env)
    here = os.path.dirname(os.path.abspath(__file__))
    cands += [os.path.normpath(os.path.join(here, rel))
              for rel in _TEMPLATE_CANDIDATES]
    for c in cands:
        if os.path.isfile(os.path.join(c, "index.html")):
            return c
    raise TxnError(
        "graph UI template not found (need index.html): tried %s — pass "
        "--ui-dist <dir> or set WIKI_UI_DIST" % ", ".join(cands))


def export(wiki, out_dir: str, ui_dist_dir=None) -> dict:
    payload = graphdata.graph_payload(wiki, full=True)
    if not payload.get("nodes"):
        raise TxnError("wiki graph is empty: nothing to export")
    src = ui_dist(ui_dist_dir)
    tmp = tempfile.mkdtemp(prefix="graph-export-")
    try:
        shutil.copytree(src, tmp, dirs_exist_ok=True)
        with open(os.path.join(tmp, "graph-data.json"), "w") as f:
            f.write(json.dumps(payload, ensure_ascii=False) + "\n")
        # atomic replace, restoring the old bundle if the rename fails
        stale = out_dir + ".stale-%d" % os.getpid()
        had_old = os.path.exists(out_dir)
        if had_old:
            os.rename(out_dir, stale)
        try:
            os.rename(tmp, out_dir)
        except Exception:
            if had_old:
                os.rename(stale, out_dir)
            raise
        if had_old:
            shutil.rmtree(stale, ignore_errors=True)
        files = [os.path.join(r, f) for r, _, fs in os.walk(out_dir) for f in fs]
        return {"dir": out_dir, "files": len(files),
                "nodes": len(payload["nodes"]),
                "links": len(payload["links"]),
                "bytes": sum(os.path.getsize(p) for p in files)}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
