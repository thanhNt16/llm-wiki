"""Deterministic corpus generator: no RNG, stable keys, mixed verdicts."""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import compile as wc_compile, sources  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402

SUBJECT_POOL = ["acme.svc%02d" % i for i in range(40)]
PREDICATES = ("status", "owner", "summary", "limit", "endpoint")


def _cand(source_id, version, subject, predicate, value):
    return {
        "id": "auto", "subject": subject, "predicate": predicate, "value": value,
        "scope": {}, "valid_from": None, "valid_to": None,
        "recorded_at": "2026-10-31T00:00:00Z", "status": "candidate",
        "proposed_by": "bench", "authority": {"type": "authoritative_source"},
        "evidence": [{"source_id": source_id, "source_version": version,
                      "locator": {"type": "heading", "value": "B"}}],
        "supersedes": [],
    }


def _report(source_id, version):
    return {"source_id": source_id, "source_version": version,
            "coverage": {"text": "complete"}, "warnings": [],
            "parser": {"name": "bench", "version": "1"}}


def build(root, n_docs, per_doc=10):
    """Ingest n_docs sources and stage per_doc candidates for each.

    Every stage_candidates() call parks a run dir under .state/staging
    (the same layout cmd_reconcile_prepare consumes); callers enumerate
    run ids from there.
    """
    wiki = Wiki(root)
    init_wiki(wiki, "perf")
    for n in range(n_docs):
        r = sources.ingest(wiki, "text", "perf-doc-%06d" % n,
                           ("bench body %d" % n).encode())
        cands = []
        for k in range(per_doc):
            subj = SUBJECT_POOL[(n * per_doc + k) % len(SUBJECT_POOL)]
            pred = PREDICATES[(n + k) % len(PREDICATES)]
            value = "v%d" % ((n + k) % 2)
            cands.append(_cand(r["source_id"], r["version"], subj, pred, value))
        wc_compile.stage_candidates(wiki, r["source_id"], r["version"], cands,
                                    _report(r["source_id"], r["version"]))
    return wiki
