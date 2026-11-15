#!/usr/bin/env python3
"""Deterministic performance corpus for the rust-vs-python eval (lane B).

Generates plain markdown "service runbook" documents — no candidate labels
(lane A owns labeled corpora). Sizes are mixed across three tiers
(~0.5 KB / ~4 KB / ~20 KB) with a fixed-seed jitter so output bytes are
identical run to run for a given doc count.

The same module supplies the deterministic facts (service name, owner,
status) used to mint candidates and query questions downstream, so the
eval never re-parses document bodies.

Stdlib only, Python >= 3.9.
"""
import argparse
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

SEED = 20261115
TIERS = ((520, 900), (3400, 6200), (14000, 21000))  # bytes, cycled per doc
OWNERS = ("platform", "billing", "search", "infra", "growth", "secops")
STATUSES = ("ga", "beta", "deprecated", "incubating")
TEAMS = ("atlas", "boreal", "cinder", "delta", "ember", "fjord")

_WORDS = (
    "replica shard latency quorum failover backoff retry circuit breaker "
    "quota tenancy rollout canary telemetry tracing cardinality backlog "
    "partition compaction snapshot retention throttle ingress egress mesh "
    "sidecar scheduler allocator ledger checkpoint idempotent graceful "
    "handoff drain warm standby regional zonal envelope checksum cursor "
).split()


def service_name(i):
    return "svc-%04d" % i


def owner_of(i):
    return OWNERS[i % len(OWNERS)]


def status_of(i):
    return STATUSES[(i * 7 + 3) % len(STATUSES)]


def team_of(i):
    return TEAMS[(i * 5 + 1) % len(TEAMS)]


def _filler(rng, nbytes):
    """Deterministic prose sized to ~nbytes."""
    parts = []
    n = 0
    word = " ".join(_WORDS)
    while n < nbytes:
        words = [rng.choice(_WORDS) for _ in range(12)]
        sentence = " ".join(words).capitalize() + ".\n\n"
        parts.append(sentence)
        n += len(sentence)
        if n < nbytes:
            para = "- `%s-%s`: %s %s %s\n" % (
                rng.choice(_WORDS), rng.choice(_WORDS),
                rng.choice(_WORDS), rng.choice(_WORDS), word[:80])
            parts.append(para)
            n += len(para)
    return "".join(parts)


def doc_text(i, rng):
    """Markdown body for document i (~0.5-20 KB depending on tier)."""
    lo, hi = TIERS[i % len(TIERS)]
    target = rng.randint(lo, hi)
    head = (
        "# %s runbook\n\n"
        "## Overview\n\n"
        "Service `%s` is owned by the `%s` team (owner alias `%s`). "
        "Current rollout status: `%s`. Primary endpoint "
        "`https://%s.internal.example/v2`. Escalation goes to "
        "`oncall-%s`.\n\n"
        "## Details\n\n"
    ) % (service_name(i), service_name(i), team_of(i), owner_of(i),
         status_of(i), service_name(i), team_of(i))
    return head + _filler(rng, target - len(head))


def gen_docs(n_docs):
    """[(filename, text)] for n_docs documents, byte-deterministic."""
    rng = random.Random(SEED)
    out = []
    for i in range(n_docs):
        name = "%s.md" % service_name(i)
        out.append((name, doc_text(i, rng)))
    return out


def write_corpus(out_dir, n_docs):
    os.makedirs(out_dir, exist_ok=True)
    docs = gen_docs(n_docs)
    for name, text in docs:
        with open(os.path.join(out_dir, name), "w", encoding="utf-8") as f:
            f.write(text)
    return docs


def questions(n_docs):
    """Deterministic question list (one per service, cycled phrasing)."""
    qs = []
    for i in range(n_docs):
        svc = service_name(i)
        variant = i % 3
        if variant == 0:
            qs.append("who owns %s" % svc)
        elif variant == 1:
            qs.append("what is the rollout status of %s" % svc)
        else:
            qs.append("which team is on call for %s" % svc)
    return qs


def candidates_for(source_id, version, i):
    """Deterministic candidate claims for doc i's ingested source."""
    svc = service_name(i)
    facts = (("owner", owner_of(i)),
             ("status", status_of(i)),
             ("oncall-team", team_of(i)))
    cands = []
    for k, (pred, val) in enumerate(facts):
        cands.append({
            "id": "auto", "subject": svc, "predicate": pred, "value": val,
            "scope": {}, "valid_from": None, "valid_to": None,
            "recorded_at": "2026-11-01T00:00:00Z", "status": "candidate",
            "proposed_by": "perf-lane-b",
            "authority": {"type": "authoritative_source"},
            "evidence": [{"source_id": source_id,
                          "source_version": version,
                          "locator": {"type": "heading", "value": "Overview"}}],
            "supersedes": [],
        })
    return cands


def report_for(source_id, version):
    return {"source_id": source_id, "source_version": version,
            "coverage": {"text": "complete"}, "warnings": [],
            "parser": {"name": "perf-lane-b", "version": "1"}}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", required=True, help="output directory")
    ap.add_argument("--docs", type=int, default=150, help="document count")
    args = ap.parse_args()
    docs = write_corpus(args.out, args.docs)
    sizes = [len(t.encode("utf-8")) for _, t in docs]
    manifest = {"docs": len(docs), "seed": SEED,
                "bytes_total": sum(sizes),
                "bytes_min": min(sizes), "bytes_max": max(sizes)}
    print(json.dumps(manifest, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
