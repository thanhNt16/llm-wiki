"""wiki-dream: corpus reconciliation + garbage collection.

One "sleep cycle" over the wiki: detect lifecycle debt (superseded, expired,
duplicated, orphaned, dead sources, junk state files, dangling dependency
edges), report it as a plan, and — under --apply — archive canonical data and
delete only proven junk inside a transaction.

Safety model (spec 2026-10-25-dream-design.md):
- canonical data is never hard-deleted; it moves to claims/_archive/ or
  .state/dream-archive/ with dreamed_at + dream_reason embedded
- junk is .state/* non-canonical files only (staging, journals, backups,
  non-held locks)
- --purge deletes archive entries older than retention_days with no live
  reference, always printing the plan first
"""
import datetime
import fcntl
import json
import os
import shutil

from . import claims, deps, review
from .hashing import canonical_json
from .transaction import Transaction

PHASES = ("supersede", "expire", "dedupe", "orphan",
          "sources", "junk", "derived")

RETENTION_DAYS = 90
_ARCHIVE_CLAIMS = "claims/_archive"
_ARCHIVE_SOURCES = ".state/dream-archive/sources"


def _today(now) -> datetime.date:
    if now is None:
        return datetime.date.today()
    if isinstance(now, datetime.date):
        return now
    return datetime.date.fromisoformat(str(now))


def _iso(d: datetime.date) -> str:
    return d.isoformat()


def _iso_now(now) -> str:
    return "%sT00:00:00Z" % _iso(now)


# ---------------------------------------------------------------- plan

def collect_plan(wiki, phases=None, now=None) -> dict:
    """Detect every dream action without touching the filesystem."""
    phases = tuple(phases) if phases else PHASES
    now_d = _today(now)
    claim_objs = claims.list_claims(wiki)
    decision_objs = claims.list_decisions(wiki)
    queue_items = _queue_items(wiki)
    under_review = {
        a.split("@")[0]
        for it in queue_items if it.get("status") == "open"
        for a in it.get("affected", [])}
    live_ids = {c["id"] for c in claim_objs if c.get("id")}
    live_ids |= {d["id"] for d in decision_objs if d.get("id")}

    plan = {p: {"archive": [], "delete": [], "move": [],
                "edge_drops": [], "skip": []} for p in PHASES}

    if "supersede" in phases:
        for c in claim_objs:
            if c.get("status") == "superseded" or c.get("superseded_by"):
                plan["supersede"]["archive"].append(
                    {"id": c["id"], "reason": "superseded_by %s"
                     % c.get("superseded_by", "?")})

    if "expire" in phases:
        superseded = {a["id"] for a in plan["supersede"]["archive"]}
        for c in claim_objs:
            vt = c.get("valid_to")
            if not vt or c["id"] in superseded:
                continue
            if c.get("status") in claims.TERMINAL:
                continue
            try:
                if datetime.date.fromisoformat(str(vt)[:10]) < now_d:
                    plan["expire"]["archive"].append(
                        {"id": c["id"], "reason": "valid_to %s passed" % vt})
            except ValueError:
                plan["expire"]["skip"].append(
                    {"id": c["id"], "reason": "unparseable valid_to %r" % vt})

    if "dedupe" in phases:
        groups = {}
        already = {a["id"]
                   for ph in ("supersede", "expire") for a in plan[ph]["archive"]}
        for c in claim_objs:
            if c.get("status") in claims.TERMINAL or not c.get("id") \
                    or c["id"] in already:
                continue
            key = (c.get("subject"), c.get("predicate"),
                   canonical_json(c.get("scope") or {}),
                   canonical_json(c.get("value")))
            groups.setdefault(key, []).append(c)
        for group in groups.values():
            if len(group) < 2:
                continue
            group.sort(key=lambda c: c.get("recorded_at") or "")
            keep = group[-1]
            for c in group[:-1]:
                plan["dedupe"]["archive"].append(
                    {"id": c["id"],
                     "reason": "duplicate of %s" % keep["id"]})

    if "orphan" in phases:
        already = {a["id"] for ph in ("supersede", "expire", "dedupe")
                   for a in plan[ph]["archive"]}
        for c in claim_objs:
            if c.get("evidence") or not c.get("id") or c["id"] in already:
                continue
            if c["id"] in under_review:
                plan["orphan"]["skip"].append(
                    {"id": c["id"], "reason": "open review item"})
                continue
            if c.get("status") == "candidate":
                plan["orphan"]["archive"].append(
                    {"id": c["id"], "reason": "candidate with no evidence"})
            else:
                plan["orphan"]["skip"].append(
                    {"id": c["id"],
                     "reason": "%s claim lacks evidence — human review"
                               % c.get("status")})

    if "sources" in phases:
        cited = {ev.get("source_id") for c in claim_objs
                 for ev in c.get("evidence") or [] if ev.get("source_id")}
        sdir = wiki.p("sources")
        if os.path.isdir(sdir):
            for sid in sorted(os.listdir(sdir)):
                full = os.path.join(sdir, sid)
                if not os.path.isdir(full) or sid.startswith("_"):
                    continue
                if sid not in cited:
                    plan["sources"]["move"].append(
                        {"id": sid, "reason": "no claim cites this source"})

    if "junk" in phases:
        state = wiki.p(".state")
        staging = os.path.join(state, "staging")
        if os.path.isdir(staging):
            for entry in sorted(os.listdir(staging)):
                plan["junk"]["delete"].append(
                    {"path": ".state/staging/%s" % entry,
                     "reason": "staging run leftover"})
        for name in sorted(os.listdir(state)) if os.path.isdir(state) else []:
            if name.startswith("journal") and name.endswith(".json"):
                plan["junk"]["delete"].append(
                    {"path": ".state/%s" % name,
                     "reason": "transaction journal leftover"})
            elif name.startswith(".backup-"):
                plan["junk"]["delete"].append(
                    {"path": ".state/%s" % name,
                     "reason": "transaction backup dir leftover"})
        locks = os.path.join(state, "locks")
        if os.path.isdir(locks):
            for entry in sorted(os.listdir(locks)):
                rel = ".state/locks/%s" % entry
                if entry == "commit.lock" and _lock_held(os.path.join(locks, entry)):
                    plan["junk"]["skip"].append(
                        {"path": rel, "reason": "commit.lock is held"})
                else:
                    plan["junk"]["delete"].append(
                        {"path": rel, "reason": "stale lock file"})

    if "derived" in phases:
        edges = deps.graph(wiki).get("edges", {})
        for artifact in sorted(edges):
            if not os.path.isfile(wiki.p(artifact)):
                plan["derived"]["edge_drops"].append(
                    {"artifact": artifact,
                     "reason": "registered artifact missing"})
                continue
            for dep in edges[artifact]:
                dep_id = str(dep).split("@", 1)[0]
                if dep_id.startswith("wiki/"):
                    if dep_id not in edges and not os.path.isfile(wiki.p(dep_id)):
                        plan["derived"]["edge_drops"].append(
                            {"artifact": artifact, "dep": dep,
                             "reason": "unresolved page dep"})
                elif dep_id.startswith("review_"):
                    if dep_id not in {i.get("id") for i in queue_items}:
                        plan["derived"]["edge_drops"].append(
                            {"artifact": artifact, "dep": dep,
                             "reason": "unresolved review dep"})
                elif dep_id not in live_ids:
                    plan["derived"]["edge_drops"].append(
                        {"artifact": artifact, "dep": dep,
                         "reason": "dep id not live"})

    counts = {p: len(plan[p]["archive"]) + len(plan[p]["delete"])
              + len(plan[p]["move"]) + len(plan[p]["edge_drops"])
              for p in PHASES}
    return {"ok": True, "applied": False, "phases": plan, "counts": counts,
            "purge": None, "as_of": _iso(now_d)}


# ---------------------------------------------------------------- lock

def _lock_held(path: str) -> bool:
    """True when commit.lock is actually held (non-blocking flock fails)."""
    try:
        fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW)
    except OSError:
        return False
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        return True
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError:
            pass
        os.close(fd)
    return False


def _queue_items(wiki) -> list:
    path = wiki.p(review.QUEUE)
    if not os.path.isfile(path):
        return []
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    continue
    return out


# ---------------------------------------------------------------- apply

def run(wiki, apply=False, phases=None, now=None) -> dict:
    """Dry-run by default; --apply executes the plan."""
    plan = collect_plan(wiki, phases=phases, now=now)
    if not apply:
        return plan
    now_d = _today(now)
    dreamed_at = _iso_now(now_d)

    # junk: direct fs deletes (all under .state — non-canonical)
    deleted = []
    for item in plan["phases"]["junk"]["delete"]:
        full = wiki.p(item["path"])
        if not os.path.lexists(full):
            continue
        if os.path.isdir(full) and not os.path.islink(full):
            shutil.rmtree(full, ignore_errors=True)
        else:
            os.unlink(full)
        deleted.append(item["path"])

    # sources: dir move can't ride the file-level txn; pre-commit, recorded
    moved = []
    for item in plan["phases"]["sources"]["move"]:
        src = wiki.p("sources", item["id"])
        dst = wiki.p(_ARCHIVE_SOURCES, item["id"])
        if not os.path.isdir(src):
            continue
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        marker = {"id": item["id"], "dreamed_at": dreamed_at,
                  "dream_reason": item["reason"]}
        with open(src + "/.dream.json", "w", encoding="utf-8") as f:
            json.dump(marker, f, indent=2)
        shutil.move(src, dst)
        moved.append(dst)

    # canonical moves + edge cleanup in one transaction
    txn = Transaction(wiki, "wiki-dream")
    archived = []
    for phase in ("supersede", "expire", "dedupe", "orphan"):
        for item in plan["phases"][phase]["archive"]:
            cid = item["id"]
            cpath = wiki.p("claims", "%s.json" % cid)
            if not os.path.isfile(cpath):
                continue
            with open(cpath, encoding="utf-8") as f:
                data = json.load(f)
            data["dreamed_at"] = dreamed_at
            data["dream_reason"] = item["reason"]
            txn.stage_write("%s/%s.json" % (_ARCHIVE_CLAIMS, cid), data)
            txn.stage_delete("claims/%s.json" % cid)
            archived.append(cid)

    drops = plan["phases"]["derived"]["edge_drops"]
    if drops:
        def _upd(data):
            edges = data.get("edges", {})
            missing = {d["artifact"] for d in drops if "dep" not in d}
            for a in missing:
                edges.pop(a, None)
            dep_drops = {}
            for d in drops:
                if "dep" in d:
                    dep_drops.setdefault(d["artifact"], set()).add(d["dep"])
            for a, dead in dep_drops.items():
                edges[a] = [x for x in edges.get(a, []) if x not in dead]
            data["edges"] = edges
        txn.stage_state(deps.DEPS_FILE, _upd)

    report = {
        "ok": True, "applied": True,
        "phases": plan["phases"], "counts": plan["counts"],
        "archived": archived, "moved_sources": moved,
        "deleted": deleted, "edge_drops": len(drops),
        "skipped": {p: plan["phases"][p]["skip"] for p in PHASES},
        "dreamed_at": dreamed_at,
        "purge": None,
    }
    txn.stage_write(".state/dream_report.json", report)
    txn.changes = {"claims_archived": len(archived),
                   "sources_moved": len(moved),
                   "junk_deleted": len(deleted),
                   "edges_dropped": len(drops)}
    receipt = txn.commit(wiki.revision())
    report["run_id"] = receipt["run_id"]
    report["revision"] = receipt["revision"]["after"]
    return report


# ---------------------------------------------------------------- purge

def _live_references(wiki) -> set:
    """Every id a live claim/decision/queue item/dep edge still points at."""
    refs = set()
    for c in claims.list_claims(wiki):
        refs.update(c.get("supersedes") or [])
        if c.get("superseded_by"):
            refs.add(c["superseded_by"])
        refs.update(ev.get("source_id") for ev in c.get("evidence") or []
                    if ev.get("source_id"))
    for d in claims.list_decisions(wiki):
        refs.update(d.get("claims") or [])
    for it in _queue_items(wiki):
        refs.update(a.split("@")[0] for a in it.get("affected") or [])
    for artifact, ds in deps.graph(wiki).get("edges", {}).items():
        refs.add(artifact)
        refs.update(str(x).split("@", 1)[0] for x in ds)
    return refs


def _archived_at(path: str) -> str:
    """dreamed_at of an archive entry: JSON field or .dream.json marker."""
    try:
        with open(path if os.path.isfile(path)
                  else os.path.join(path, ".dream.json"),
                  encoding="utf-8") as f:
            data = json.load(f)
        return str(data.get("dreamed_at", ""))[:10]
    except (OSError, ValueError):
        return ""


def purge(wiki, dry_run=False, now=None,
          retention_days=RETENTION_DAYS) -> dict:
    """Delete archive entries older than retention_days and unreferenced."""
    now_d = _today(now)
    cutoff = now_d - datetime.timedelta(days=retention_days)
    refs = _live_references(wiki)

    entries = []  # (rel, id, age_date)
    adir = wiki.p(_ARCHIVE_CLAIMS)
    if os.path.isdir(adir):
        for name in sorted(os.listdir(adir)):
            if name.endswith(".json"):
                entries.append(("%s/%s" % (_ARCHIVE_CLAIMS, name),
                                name[:-5]))
    sdir = wiki.p(_ARCHIVE_SOURCES)
    if os.path.isdir(sdir):
        for sid in sorted(os.listdir(sdir)):
            entries.append(("%s/%s" % (_ARCHIVE_SOURCES, sid), sid))

    purged, skipped = [], []
    for rel, eid in entries:
        full = wiki.p(rel)
        at = _archived_at(full)
        try:
            old = datetime.date.fromisoformat(at) < cutoff
        except ValueError:
            old = False
        if not old:
            skipped.append({"path": rel, "reason": "too_recent",
                            "dreamed_at": at or "unknown"})
        elif eid in refs:
            skipped.append({"path": rel, "reason": "still_referenced"})
        else:
            purged.append(rel)

    if not dry_run and purged:
        txn = Transaction(wiki, "wiki-dream-purge")
        for rel in purged:
            full = wiki.p(rel)
            if os.path.isdir(full):
                shutil.rmtree(full, ignore_errors=True)
            else:
                txn.stage_delete(rel)
        txn.stage_write(".state/dream_report.json", {
            "ok": True, "applied": True, "phases": {}, "counts": {},
            "purge": {"purged": purged, "skipped": skipped}})
        txn.changes = {"archive_purged": len(purged)}
        txn.commit(wiki.revision())

    return {"ok": True, "applied": not dry_run, "phases": {}, "counts": {},
            "purge": {"retention_days": retention_days,
                      "cutoff": _iso(cutoff),
                      "purged": purged, "skipped": skipped}}
