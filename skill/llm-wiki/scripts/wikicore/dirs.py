"""Recursive directory ingest: deterministic walk, one source per file.

Traversal is deterministic (sorted); each member file goes through
sources.ingest() unchanged (own source_id, sha256 dedup, secrets scan,
extraction report). Skip rules are recorded per file in the batch receipt.
"""
import json
import os
import time
from typing import Optional

from . import ids, sources
from .transaction import TxnError

SKIP_DIRS = {
    ".git", ".svn", ".hg", "node_modules", "__pycache__", ".venv", "venv",
    "dist", "build", "target", "vendor", ".idea", ".llm-wiki",
}
DEFAULT_MAX_BYTES = 50 * 1024 * 1024


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _classify_skip(path: str, name: str, is_dir: bool,
                   include_hidden: bool, max_bytes: int) -> Optional[str]:
    """Return a skip reason, or None if the entry should be ingested."""
    if is_dir:
        if name in SKIP_DIRS:
            return "vcs/vendor dir"
        if name.startswith(".") and not include_hidden:
            return "hidden"
        return None
    if name in sources.SKIP_NAMES:
        return "non-evidence binary"
    if os.path.splitext(name)[1].lower() in sources.SKIP_EXTS:
        return "non-evidence binary"
    if name.startswith(".") and not include_hidden:
        return "hidden"
    if os.path.islink(path):
        return "symlink"
    try:
        if os.path.getsize(path) > max_bytes:
            return "oversize"
    except OSError:
        return "unreadable"
    return None


def _walk(root: str, include_hidden: bool, max_bytes: int) -> list:
    """Return (relpath, action, reason) for every entry encountered.

    action is "file" (ingest) or "skipped" (record reason). Entries inside a
    skipped directory inherit its skip reason so the receipt lists them
    per-file.
    """
    items = []
    skipped_dirs = {}  # relpath -> reason
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        dr = os.path.relpath(dirpath, root)
        inherited = next(
            (reason for anc, reason in skipped_dirs.items()
             if dr == anc or dr.startswith(anc + os.sep)),
            None,
        )
        # A .llm-wiki store inside the tree is omitted entirely (it is the
        # evidence database, not evidence); other skipped dirs report
        # per-file skips so nothing vanishes silently.
        omit_children = dr == ".llm-wiki" or \
            dr.startswith(".llm-wiki" + os.sep) or \
            os.sep + ".llm-wiki" + os.sep in dr or \
            dr.endswith(os.sep + ".llm-wiki")
        for d in list(dirnames):
            if omit_children:
                continue  # inside .llm-wiki: omit descendants entirely
            p = os.path.join(dirpath, d)
            rel = os.path.relpath(p, root)
            if os.path.islink(p):
                items.append((rel, "skipped", "symlink"))
                dirnames.remove(d)
                continue
            reason = inherited or _classify_skip(
                p, d, True, include_hidden, max_bytes)
            if reason:
                items.append((rel, "skipped", reason))
                skipped_dirs[rel] = reason
        for f in filenames:
            if omit_children:
                continue
            p = os.path.join(dirpath, f)
            rel = os.path.relpath(p, root)
            reason = inherited or _classify_skip(
                p, f, False, include_hidden, max_bytes)
            if reason:
                items.append((rel, "skipped", reason))
            else:
                items.append((rel, "file", None))
    items.sort(key=lambda t: t[0])
    return items


def ingest_dir(wiki, path: str, *, include_hidden: bool = False,
               max_bytes: int = DEFAULT_MAX_BYTES, force: bool = False) -> dict:
    if not wiki.exists():
        raise TxnError("wiki not initialized; run wiki-init first")
    if not os.path.isdir(path):
        raise TxnError("not a directory: %s" % path)
    root = os.path.abspath(path)
    root_origin = "dir:" + root
    batch_id = ids.new("batch")
    items = []
    counts = {"ingested": 0, "deduplicated": 0, "updated_normalized": 0,
              "skipped": 0, "errors": 0}
    for rel, action, reason in _walk(root, include_hidden, max_bytes):
        if action == "skipped":
            items.append({"path": rel, "status": "skipped", "reason": reason})
            counts["skipped"] += 1
            continue
        full = os.path.join(root, rel)
        try:
            with open(full, "rb") as fh:
                data = fh.read()
            r = sources.ingest(wiki, "file", full, data,
                               force=force, root_origin=root_origin)
        except Exception as e:
            items.append({"path": rel, "status": "error", "error": str(e)})
            counts["errors"] += 1
            continue
        if r.get("deduplicated"):
            status = "deduplicated"
        elif r.get("updated_normalized"):
            status = "updated_normalized"
        else:
            status = "ingested"
        counts[status] += 1
        item = {"path": rel, "status": status,
                "source_id": r["source_id"], "version": r["version"]}
        if r.get("coverage"):
            item["coverage"] = r["coverage"]
        if r.get("warnings"):
            item["warnings"] = r["warnings"]
        items.append(item)
    receipt = {"batch_id": batch_id, "dir": root, "captured_at": _now(),
               "counts": counts, "items": items}
    out_dir = wiki.p("state", "ingest-batches")
    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "%s.json" % batch_id), "w",
              encoding="utf-8") as f:
        json.dump(receipt, f, indent=2, ensure_ascii=False)
        f.write("\n")
    return receipt
