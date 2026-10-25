"""Planted claims + gold questions for the deterministic benchmark.

Mirrors the golden-corpus planted cases (attribution window correction 30d->7d,
retry conflict 3 vs 5, staging scope) plus three deliberate probes:
- near-duplicate page attempt ("Attribution Window" then "Attribution Window
  Backup") — exercises the extend-don't-create guard (base: created; HEAD:
  refused with exit 3)
- single-origin subject ("feature flag rollout") — lands on page-targets
  waiting list (two-source rule)
- stray page written directly to the filesystem — orphan_page lint target.

Engine-shape notes (verified against wikicore/compile.py + schemas/
claim.schema.json):
- authority types must come from the claim-schema enum; "authoritative_source"
  is the closest valid match for corpus-documented values (and is in
  acceptance_policy.auto_accept_authority, so UNRELATED reconciles auto-accept).
- the memo's correction claim carries "defer": "correction" — pass 1 of
  driver.build_wiki skips deferred claims (stripping the key before staging);
  pass 2 (_apply_correction) stages it alone with relationship CORRECTION
  against the created 30d claim. "CORRECTS" is not in the engine's
  relationship vocabulary (CORRECTION, POLICY_CHANGE, SCOPE_DIFFERENCE,
  CONTRADICTION, CORROBORATION, DUPLICATE, UNRELATED).
- "AUTO:<key>" entries in `supersedes` are fixture-internal markers; the
  driver strips them before staging (the engine appends the real target id
  itself on CORRECTION, and claim.schema.json requires uniqueItems).
"""
CORPUS_DOCS = ["docs/adr-007-retries.md", "docs/adr-019-attribution.md",
               "notes/memo-2026-09-01-window-change.md",
               "config/analytics.prod.json", "config/analytics.sandbox.json",
               "notes/meeting-2026-09-10.md",
               "docs/adr-001-postgres.md"]

# source of the deferred correction claim (pass 2 re-stages from here)
CORRECTION_SOURCE_REL = "notes/memo-2026-09-01-window-change.md"


def _cand(subject, predicate, value, authority="authoritative_source",
          scope=None, valid_from=None, supersedes=(), defer=None):
    cand = {
        "id": "auto", "subject": subject, "predicate": predicate,
        "value": value, "scope": scope or {}, "valid_from": valid_from,
        "valid_to": None, "recorded_at": "2026-09-15T00:00:00Z",
        "status": "candidate", "proposed_by": "bench",
        "authority": {"type": authority},
        "evidence": [{"source_id": "FILLED_BY_DRIVER", "source_version": 1,
                      "locator": {"type": "heading", "value": "H"}}],
        "supersedes": list(supersedes),
    }
    if defer is not None:
        cand["defer"] = defer
    return cand


def _cls(rel="UNRELATED", target=None, valid_from=None):
    return [{"index": 0, "relationship": rel, "target_claim_id": target,
             "valid_from": valid_from, "valid_to": None,
             "authority": {"type": "authoritative_source"}}]


# batches keyed by source; driver fills evidence source ids after ingest
BATCHES = [
    {"source_rel": "docs/adr-019-attribution.md", "claims": [
        _cand("attribution", "window-days", 30, valid_from="2026-01-01"),
        _cand("attribution", "scope", "prod"),
    ]},
    {"source_rel": CORRECTION_SOURCE_REL, "claims": [
        _cand("attribution", "window-days", 7, valid_from="2026-09-01",
              supersedes=["AUTO:attribution|window-days|30"],
              defer="correction"),
    ]},
    {"source_rel": "config/analytics.prod.json", "claims": [
        _cand("attribution", "scope", "prod"),
    ]},
    {"source_rel": "config/analytics.sandbox.json", "claims": [
        _cand("attribution", "scope", "sandbox", scope={"env": "sandbox"}),
    ]},
    {"source_rel": "docs/adr-007-retries.md", "claims": [
        _cand("retry", "max-attempts", 3),
        _cand("retry", "backoff", "exponential"),
    ]},
    {"source_rel": "notes/meeting-2026-09-10.md", "claims": [
        _cand("retry", "max-attempts", 5),
    ]},
    {"source_rel": "docs/adr-001-postgres.md", "claims": [
        _cand("order service", "database", "postgres"),
        _cand("feature flag rollout", "status", "in-progress"),
    ]},
]

# interface alias (task-3 brief interface section names this CANDIDATE_BATCHES)
CANDIDATE_BATCHES = BATCHES

# supersede resolution: the memo claim supersedes the adr-019 30d claim; the
# driver resolves this in pass 2 (_apply_correction) by looking up the created
# 30d claim id and classifying the memo claim CORRECTION against it — claim
# ids are minted at staging, so the AUTO: marker above cannot be resolved
# until pass 1 has run.

GOLD_QUESTIONS = [
    {"id": "q1-order-db", "question": "Which database does the order service use?",
     "expect": [{"subject": "order service", "predicate": "database",
                 "value": "postgres"}]},
    {"id": "q2-window-now", "question": "What is the current attribution window?",
     "expect": [{"subject": "attribution", "predicate": "window-days",
                 "value": 7}]},
    {"id": "q3-window-old", "question": "What was the attribution window before September 2026?",
     "expect": [{"subject": "attribution", "predicate": "window-days",
                 "value": 30}], "as_of": "2026-08-15"},
    {"id": "q4-retries", "question": "How many retry attempts are configured?",
     "expect": [{"subject": "retry", "predicate": "max-attempts", "value": 3},
                {"subject": "retry", "predicate": "max-attempts", "value": 5}]},
    {"id": "q5-sandbox", "question": "Does staging use the same attribution window as prod?",
     "expect": [{"subject": "attribution", "predicate": "scope",
                 "value": "sandbox"}]},
    {"id": "q6-unanswerable", "question": "What is our 2028 pricing plan?",
     "expect": []},
]

NEAR_DUP_SUBJECT = "attribution window backup"
NEAR_DUP_TITLE = "Attribution Window Backup"
STRAY_PAGE_REL = "wiki/concepts/stray-injected-page.md"
STRAY_PAGE_TEXT = ("---\ntype: concept\nsubject: stray injected page\n"
                   "title: Stray Injected Page\n"
                   "created: 2026-09-15T00:00:00Z\n---\n\n"
                   "# Stray Injected Page\n\nNo links.\n")
