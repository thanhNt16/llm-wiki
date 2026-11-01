"""FTS5 full-text cache for claim retrieval (SP2a rec #9).

Derived cache, same contract as .state/claims-index.json: claim JSON objects
are the only source of truth; this file is rebuildable at any time via
rebuild(wiki) and every reader must tolerate its absence (search -> None).
Staleness is detected by stamping the manifest revision the cache was built
at; a mismatch means readers fall back to the legacy scorer.
"""
import json
import os
import sqlite3

from . import claims

DB_REL = ".state/fts.sqlite"


def body_text(claim: dict) -> str:
    """Everything a query should be able to hit, space-joined."""
    scope = " ".join("%s %s" % kv for kv in sorted((claim.get("scope") or {}).items()))
    return " ".join(str(x) for x in (
        claim.get("subject", ""), claim.get("predicate", ""),
        claim.get("value", ""), claim.get("summary", ""), claim.get("title", ""),
        scope) if x)


def _connect(path):
    conn = sqlite3.connect(path)
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS claims_fts USING fts5("
        "claim_id UNINDEXED, body, tokenize='porter unicode61')")
    conn.execute(
        "CREATE TABLE IF NOT EXISTS fts_meta (k TEXT PRIMARY KEY, v TEXT)")
    return conn


def rebuild(wiki, revision=None) -> dict:
    """Regenerate the cache from claim objects; atomic via tmp+replace.

    revision: stamp to write. Defaults to the current manifest revision;
    commit passes the revision it is about to create (current + 1) so the
    fresh cache is not instantly stale when the manifest advances.
    """
    rev = wiki.revision() if revision is None else revision
    tmp = wiki.p(DB_REL + ".tmp")
    if os.path.exists(tmp):
        os.remove(tmp)
    conn = _connect(tmp)
    rows = [(c["id"], body_text(c)) for c in claims.list_claims(wiki)]
    conn.executemany(
        "INSERT INTO claims_fts(claim_id, body) VALUES (?, ?)", rows)
    conn.execute("INSERT OR REPLACE INTO fts_meta(k, v) VALUES ('revision', ?)",
                 (str(rev),))
    conn.commit()
    conn.close()
    os.replace(tmp, wiki.p(DB_REL))
    return {"rows": len(rows), "revision": rev}


def apply_txn(wiki, staged_ops, new_revision: int) -> dict:
    """Apply claim upserts/deletes derived from a txn's staged op list.

    staged_ops: the txn's ("write"|"delete", rel, src) triples; only
    claims/<cid>.json entries matter. Runs inside commit's flock AFTER the
    JSON apply, so claim bodies are read from their live destinations (the
    staging src paths no longer exist). A raised exception aborts the commit
    loudly; without conn.commit() sqlite discards partial rows, the JSON
    restore in commit's except-block keeps objects authoritative, and any
    stamp/content mismatch sends readers to the legacy path until
    doctor --rebuild-fts.
    """
    path = wiki.p(DB_REL)
    conn = _connect(path)
    try:
        for op, rel, _src in staged_ops:
            if not rel.startswith("claims/") or not rel.endswith(".json"):
                continue
            cid = rel[len("claims/"):-len(".json")]
            conn.execute("DELETE FROM claims_fts WHERE claim_id = ?", (cid,))
            if op == "write":
                with open(wiki.p(rel), encoding="utf-8") as f:
                    claim = json.load(f)
                conn.execute(
                    "INSERT INTO claims_fts(claim_id, body) VALUES (?, ?)",
                    (cid, body_text(claim)))
        conn.execute(
            "INSERT OR REPLACE INTO fts_meta(k, v) VALUES ('revision', ?)",
            (str(new_revision),))
        conn.commit()
    finally:
        conn.close()
    cur = sqlite3.connect(path).execute("SELECT COUNT(*) FROM claims_fts")
    return {"rows": cur.fetchone()[0], "revision": new_revision}


def restamp(wiki, new_revision: int) -> None:
    """Advance the cache stamp when a commit changed no claim content.
    Caller must have verified the cache is usable at the current revision."""
    conn = _connect(wiki.p(DB_REL))
    try:
        conn.execute(
            "INSERT OR REPLACE INTO fts_meta(k, v) VALUES ('revision', ?)",
            (str(new_revision),))
        conn.commit()
    finally:
        conn.close()


def health(wiki) -> dict:
    path = wiki.p(DB_REL)
    expected = wiki.revision()
    if not os.path.isfile(path):
        return {"usable": False, "rows": 0, "revision": None,
                "expected_revision": expected, "reason": "missing"}
    try:
        conn = sqlite3.connect(path)
        rev = conn.execute(
            "SELECT v FROM fts_meta WHERE k='revision'").fetchone()
        rows = conn.execute("SELECT COUNT(*) FROM claims_fts").fetchone()[0]
        conn.close()
    except sqlite3.Error as e:
        return {"usable": False, "rows": 0, "revision": None,
                "expected_revision": expected, "reason": "corrupt:%s" % e}
    if not rev or int(rev[0]) != expected:
        return {"usable": False, "rows": rows,
                "revision": int(rev[0]) if rev else None,
                "expected_revision": expected, "reason": "stale_revision"}
    return {"usable": True, "rows": rows, "revision": expected,
            "expected_revision": expected, "reason": "ok"}


def search(wiki, question: str, limit: int = 30):
    """[(claim_id, bm25)] best-first, or None when the cache is unusable.

    MATCH terms are double-quoted OR-joined: recall-first, bm25 orders.
    """
    from .text import query_tokens
    if not health(wiki)["usable"]:
        return None
    toks = query_tokens(question)
    if not toks:
        return None
    match = " OR ".join('"%s"' % t for t in toks)
    try:
        conn = sqlite3.connect(wiki.p(DB_REL))
        rows = conn.execute(
            "SELECT claim_id, bm25(claims_fts) FROM claims_fts "
            "WHERE claims_fts MATCH ? ORDER BY bm25(claims_fts) LIMIT ?",
            (match, limit)).fetchall()
        conn.close()
    except sqlite3.Error:
        return None
    return rows
