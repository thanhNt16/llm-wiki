-- .llm-wiki/.state/index.sqlite — derived, disposable index (spec §10.1).
-- PRAGMA user_version pins the schema; mismatch → drop + rebuild.

CREATE TABLE IF NOT EXISTS meta (
    k TEXT PRIMARY KEY,
    v TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS claims_fts USING fts5(
    claim_id UNINDEXED,
    body,
    tokenize='porter unicode61'
);

CREATE TABLE IF NOT EXISTS claims_index (
    id TEXT PRIMARY KEY,
    subject TEXT,
    predicate TEXT,
    status TEXT,
    version INTEGER,
    path TEXT
);

CREATE TABLE IF NOT EXISTS dep_edges (
    artifact TEXT,
    dep TEXT,
    rank INTEGER
);

CREATE TABLE IF NOT EXISTS jev_answers (
    key TEXT PRIMARY KEY,
    answers_json TEXT,
    created_at TEXT
);

CREATE TABLE IF NOT EXISTS dir_cursors (
    path TEXT PRIMARY KEY,
    mtime_ns INTEGER,
    size INTEGER
);
