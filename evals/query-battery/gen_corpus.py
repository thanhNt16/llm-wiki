#!/usr/bin/env python3
"""Deterministic query-battery corpus: 240 claims over 24 subjects, plus 24
questions (4 per kind: KEY/SEM/ABBR/REL/HIST/DEC). Regenerating yields
identical claim content (subject i claims are a pure function of i; only
ULID ids and timestamps vary). Built via the real Transaction commit path so
every derived cache (claims index, FTS) sees production semantics.

Question kinds are designed to separate ranking behaviors:
  KEY  - exact lexical terms in subject+predicate
  SEM  - gerund/inflection paraphrase ("using" vs predicate "uses"):
         legacy norm_tokens cannot stem -ing to the same stem as "uses";
         porter can ("use"). Recall@1 within-subject discrimination.
  ABBR - unique 3+-char service codes ("svc007") living only in a value
  REL  - predecessor relation via the replacer's own claim
  HIST - as-of phrasing over retry-policy facts
  DEC  - "owns" vs predicate "owner" (porter stems both to "own")
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "skill", "llm-wiki", "scripts"))

from wikicore import claims, ids  # noqa: E402
from wikicore.store import Wiki, init_wiki  # noqa: E402
from wikicore.transaction import Transaction  # noqa: E402

DOMAINS = ["orders", "billing", "shipping", "auth", "search", "notify"]
KINDS = ["service", "worker", "queue", "store"]


def subject(i):
    return "%s.%s.%02d" % (DOMAINS[i % 6], KINDS[(i // 6) % 4], i)


def value(i):
    return ["PostgreSQL", "Redis", "Kafka", "S3", "Datadog", "PagerDuty"][i % 6]


def claim_dicts(i):
    return [
        {"subject": subject(i), "predicate": "uses database", "value": value(i)},
        {"subject": subject(i), "predicate": "alerting channel", "value": "#team-%d" % (i % 3)},
        {"subject": subject(i), "predicate": "deployed in region", "value": "eu-west-%d" % (i % 3)},
        {"subject": subject(i), "predicate": "retry policy", "value": "%d attempts" % (i % 4 + 1)},
        {"subject": subject(i), "predicate": "owner team", "value": "team-%c" % chr(ord("A") + i % 5)},
        {"subject": subject(i), "predicate": "max latency ms", "value": str(50 + i * 7 % 900)},
        {"subject": subject(i), "predicate": "data retention days", "value": str(30 + i * 13 % 330)},
        {"subject": subject(i), "predicate": "language", "value": ["Python", "Go", "TypeScript"][i % 3]},
        {"subject": subject(i), "predicate": "abbrev", "value": "svc%03d" % i},
        {"subject": subject(i), "predicate": "predecessor", "value": subject((i + 23) % 24) if i else "none"},
    ]


def build(root):
    w = Wiki(root)
    init_wiki(w, "query-battery")
    txn = Transaction(w, "battery-seed")
    for i in range(24):
        for d in claim_dicts(i):
            claims.save_claim(txn, {
                "id": ids.new("claim"), "status": "accepted", "evidence": [],
                "authority": {"type": "authoritative_source"}, **d})
    txn.commit(w.revision())
    return w


QUESTIONS = []


def _q(qid, kind, question, i_exp):
    QUESTIONS.append({"qid": qid, "kind": kind, "question": question,
                      "expected_subjects": [subject(i) for i in i_exp]})


for i in range(4):   # KEY: exact terms
    _q("K%02d" % i, "KEY", "Which database does %s use?" % subject(i), [i])
for i in range(4):   # SEM: gerund paraphrase (porter-only stem bridge)
    _q("S%02d" % i, "SEM",
       "What is %s using for storage?" % subject(i).replace(".", " "), [i])
for i in range(4):   # ABBR: unique service code in a value
    _q("A%02d" % i, "ABBR", "What does svc%03d depend on for storage?" % (i + 8), [i + 8])
for i in range(4):   # REL: replacer's own predecessor claim answers it
    _q("R%02d" % i, "REL",
       "%s replaced which service?" % subject(i + 4).replace(".", " "), [i + 4])
for i in range(4):   # HIST: as-of phrasing over retry policy
    _q("H%02d" % i, "HIST",
       "As of today what retry policy governs %s?" % subject(i + 12), [i + 12])
for i in range(4):   # DEC: owns vs owner (porter-only stem bridge)
    _q("D%02d" % i, "DEC", "Who owns %s?" % subject(i + 16), [i + 16])


if __name__ == "__main__":
    root = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "corpus")
    build(root)
    with open(os.path.join(HERE, "questions.json"), "w") as f:
        json.dump(QUESTIONS, f, indent=2)
    print("built", root, "questions:", len(QUESTIONS))
