"""Recursive directory ingest: deterministic walk, one source per file.

Traversal is deterministic (sorted); each member file goes through
sources.ingest() unchanged (own source_id, sha256 dedup, secrets scan,
extraction report). Skip rules are recorded per entry in the batch receipt.
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


class _BecameSymlink(Exception):
    """Path became a symlink between walk-time classification and open."""


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

    action is "file" (ingest) or "skipped" (record reason). Skipped
    directories are recorded once and pruned from traversal — their contents
    are never walked (node_modules/.git scale) and never appear in the
    receipt. A .llm-wiki store inside the tree is skipped like any other
    store dir (it is the evidence database, not evidence).
    """
    items = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        for d in list(dirnames):
            p = os.path.join(dirpath, d)
            rel = os.path.relpath(p, root)
            if os.path.islink(p):
                items.append((rel, "skipped", "symlink"))
                dirnames.remove(d)
                continue
            reason = _classify_skip(p, d, True, include_hidden, max_bytes)
            if reason:
                items.append((rel, "skipped", reason))
                dirnames.remove(d)  # prune: never descend into skipped dirs
        for f in filenames:
            p = os.path.join(dirpath, f)
            rel = os.path.relpath(p, root)
            reason = _classify_skip(p, f, False, include_hidden, max_bytes)
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
            # re-check at open time: the path may have been swapped for a
            # symlink between _walk() and now (TOCTOU); O_NOFOLLOW where the
            # OS supports it, islink() as the portable fallback
            fd = os.open(full, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
            with os.fdopen(fd, "rb") as fh:
                data = fh.read()
            if os.path.islink(full):
                raise _BecameSymlink()
            r = sources.ingest(wiki, "file", full, data,
                               force=force, root_origin=root_origin)
        except _BecameSymlink:
            items.append({"path": rel, "status": "skipped", "reason": "symlink"})
            counts["skipped"] += 1
            continue
        except OSError as e:
            if os.path.islink(full):
                # O_NOFOLLOW refused a symlink (ELOOP): treat as skipped
                items.append({"path": rel, "status": "skipped",
                              "reason": "symlink"})
                counts["skipped"] += 1
            else:
                items.append({"path": rel, "status": "error", "error": str(e)})
                counts["errors"] += 1
            continue
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
    out_dir = wiki.p(".state", "ingest-batches")
    os.makedirs(out_dir, exist_ok=True)
    tmp = os.path.join(out_dir, "%s.json.tmp" % batch_id)
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(receipt, f, indent=2, ensure_ascii=False)
        f.write("\n")
    os.replace(tmp, os.path.join(out_dir, "%s.json" % batch_id))
    return receipt
