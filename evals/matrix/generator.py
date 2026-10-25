"""Seeded synthetic corpus generator for the evaluation matrix.

`synthetic_corpus(n, seed, fault)` writes a deterministic corpus tree:

    <dir>/corpus/{docs,notes,config}/src-*.md   -- N markdown sources
    <dir>/manifest.json                        -- planted claims + gold
                                                  questions + fault pages
    <dir>/faults/                              -- (unused; page-level faults
                                                  travel inside the manifest
                                                  as text the runner injects
                                                  post-build)

Subjects are drawn Zipf-ish from a 60-word topic list so the same head
topics recur (near-duplicate claims, corroboration) while the tail stays
sparse. Every draw comes from one `random.Random(seed)`, so identical
seeds produce byte-identical trees (see `tree_hash`).

Fault modes plant pages that `doctor` flags on a built wiki. The pages
are emitted inline in the manifest (`fault_pages`); the runner applies
them by writing files under `<wiki_root>/.llm-wiki/` after build-pages.
Filesystem injection (not write-page) is what makes them lint findings.
"""
import hashlib
import json
import os
import random

# 60 distinct topic words. The two UNANSWERABLE words are deliberately
# absent so unanswerable gold questions token-match nothing.
TOPICS = [
    "atlas", "beacon", "cache", "drift", "envoy", "fabric", "harbor",
    "ionos", "juno", "kernel", "lattice", "meadow", "nimbus", "orbit",
    "prism", "quartz", "relay", "signal", "tundra", "uplink", "vector",
    "warden", "xenon", "yonder", "zephyr", "anchor", "broker", "cinder",
    "delta", "ember", "fjord", "granite", "helium", "iris", "jigsaw",
    "kelvin", "lumen", "magnet", "nebula", "onyx", "pallet", "quiver",
    "raster", "sable", "tensor", "umbra", "vortex", "wharf", "xylem",
    "yucca", "zenith", "aurora", "boreal", "cobalt", "dynamo", "ether",
    "fresco", "glacier",
]

PREDICATES = ["mode", "level", "window", "owner", "version", "budget",
              "retries", "scope", "region", "tier", "phase", "quota"]

CORPUS_SUBDIRS = ("docs", "notes", "config")

FAULTS = ("near_dup_flood", "orphan_farm", "walled_garden", "stale_ttl_farm")

# Doctor finding code each fault mode is designed to produce on the
# unified engine (spec §7 adversarial fixtures). Older engines emit their
# own vocabulary; the matrix records what each engine actually finds.
EXPECTED_FINDINGS = {
    "near_dup_flood": ["near_duplicate_subject"],
    "orphan_farm": ["orphan_page"],
    "walled_garden": ["walled_garden"],
    "stale_ttl_farm": ["stale_page_ttl"],
}

_FILLER = [
    "This document records the current %s posture for the program.",
    "The %s decision was reviewed by the platform group and accepted.",
    "Operators should treat the %s notes here as authoritative.",
    "Any change to %s handling must be proposed through an ADR first.",
    "Historical context for %s is retained for audit purposes.",
]


def _rng(seed, salt=0):
    return random.Random((seed * 1000003) ^ salt)


def _subjects(rng, n):
    """Zipf-ish topic pairs -> n subject strings (head words recur)."""
    weights = [1.0 / (i + 1) for i in range(len(TOPICS))]
    out = []
    for _ in range(n):
        pair = rng.choices(TOPICS, weights=weights, k=2)
        seen = []
        for w in pair:
            if w not in seen:
                seen.append(w)
        out.append(" ".join(seen))
    return out


def _claim(subject, predicate, value, defer=None, supersedes=()):
    """Candidate in the staged-claim schema the CLI accepts (JSON array
    path bypasses normalize_candidate, so emit the full shape — mirrors
    evals/benchmarks/fixture._cand)."""
    cand = {
        "id": "auto", "subject": subject, "predicate": predicate,
        "value": value, "scope": {}, "valid_from": None,
        "valid_to": None, "recorded_at": "2026-09-15T00:00:00Z",
        "status": "candidate", "proposed_by": "matrix-gen",
        "authority": {"type": "explicit_project_decision"},
        "evidence": [{"source_id": "FILLED_BY_RUNNER",
                      "source_version": 1,
                      "locator": {"type": "heading", "value": "H"}}],
        "supersedes": list(supersedes),
    }
    if defer is not None:
        cand["defer"] = defer
    return cand


def _source_text(rng, rel, subject, claims):
    stem = os.path.splitext(os.path.basename(rel))[0]
    lines = ["# %s" % stem, ""]
    lines.append("Subject: %s." % subject)
    lines.append("")
    for c in claims:
        lines.append("The %s %s is set to %s."
                     % (c["subject"], c["predicate"], c["value"]))
    lines.append("")
    for tmpl in rng.sample(_FILLER, 3):
        lines.append(tmpl % subject)
    lines.append("")
    return "\n".join(lines)


def _page(subject, title, body, extra_front=""):
    """Minimal concept page with frontmatter doctor can parse."""
    return ("---\ntype: concept\nsubject: %s\ntitle: %s\n%s"
            "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
            % (subject, title, extra_front, title, body))


def _rel(slug):
    return "wiki/concepts/%s.md" % slug


def fault_pages(fault, seed=42):
    """Pages to inject under .llm-wiki/ for one fault mode.

    Deterministic given (fault, seed). `walled_garden` also emits a 5-page
    spine so the 3-page garden is never the largest component. The
    near-dup flood uses a shared hub so its pairs are neither orphans nor
    dead ends — only near_duplicate_subject findings should fire (plus
    unlisted_page, since injected pages bypass the index).
    """
    rng = _rng(seed, salt=7)
    topics = rng.sample(TOPICS, 12)
    pages = []
    if fault == "orphan_farm":
        for i in range(4):
            subject = "orphan farm %s" % topics[i]
            slug = "orphan-farm-%d" % i
            pages.append({"relpath": _rel(slug),
                          "text": _page(subject, subject.title(),
                                        "An isolated page with no inbound "
                                        "or outbound page links.")})
    elif fault == "walled_garden":
        chain = ["wg-spine-%d" % i for i in range(5)]
        for i, slug in enumerate(chain):
            subject = "spine %s" % topics[i]
            nxt = chain[(i + 1) % len(chain)]
            pages.append({"relpath": _rel(slug),
                          "text": _page(subject, subject.title(),
                                        "[[wiki:concepts/%s]]" % nxt)})
        garden = ["wg-garden-%d" % i for i in range(3)]
        for i, slug in enumerate(garden):
            subject = "garden %s" % topics[5 + i]
            nxt = garden[(i + 1) % len(garden)]
            pages.append({"relpath": _rel(slug),
                          "text": _page(subject, subject.title(),
                                        "[[wiki:concepts/%s]]" % nxt)})
    elif fault == "stale_ttl_farm":
        for i in range(3):
            subject = "stale farm %s" % topics[i]
            slug = "stale-farm-%d" % i
            pages.append({"relpath": _rel(slug),
                          "text": _page(subject, subject.title(),
                                        "A page past its staleness budget.",
                                        extra_front="stale_after: 2020-01-0%d\n"
                                        % (i + 1))})
    elif fault == "near_dup_flood":
        hub_links = []
        for i in range(3):
            twin_slugs = ["nd-%s-%d" % (chr(97 + i), k) for k in (1, 2)]
            for slug in twin_slugs:
                hub_links.append("[[wiki:concepts/%s]]" % slug)
        pages.append({"relpath": _rel("nd-hub"),
                      "text": _page("near dup hub", "Near Dup Hub",
                                    " ".join(hub_links))})
        for i, word in enumerate(("alpha", "beta", "gamma")):
            subject = "near dup %s" % word
            for k in (1, 2):
                slug = "nd-%s-%d" % (chr(97 + i), k)
                pages.append({"relpath": _rel(slug),
                              "text": _page(subject, subject.title(),
                                            "[[wiki:concepts/nd-hub]]")})
    else:
        raise ValueError("unknown fault mode: %r (expected one of %s)"
                         % (fault, ", ".join(FAULTS)))
    return pages


def synthetic_corpus(n, seed=42, fault=None, dest=None):
    """Write a seeded corpus + manifest; return the output dir path.

    `fault` optionally names one of FAULTS; planted pages are embedded in
    manifest.json["fault_pages"] for the runner to inject post-build.
    """
    if fault is not None and fault not in FAULTS:
        raise ValueError("fault must be one of %s (got %r)"
                         % (", ".join(FAULTS), fault))
    rng = _rng(seed)
    if dest is None:
        import tempfile
        dest = tempfile.mkdtemp(prefix="llmwiki-synth-%d-%d-" % (n, seed))
    corpus = os.path.join(dest, "corpus")
    subjects = _subjects(rng, n)

    # ~24 claim-bearing sources spread across the corpus; every source
    # still gets ingested, the rest are text-only scale.
    stride = max(1, n // 24)
    planted = [i for i in range(n) if i % stride == 0]

    sources = {}
    questions = []
    legacy = {}  # (subject, predicate) -> legacy value for forbid probes
    for i in range(n):
        sub = CORPUS_SUBDIRS[i % len(CORPUS_SUBDIRS)]
        rel = "%s/src-%03d.md" % (sub, i)
        subject = subjects[i]
        claims = []
        if i in planted:
            predicate = PREDICATES[i % len(PREDICATES)]
            value = "%s-%04d" % (predicate, i)
            if i == planted[0]:
                # one correction pair per corpus: stage 'legacy' live in
                # pass 1, defer 'current' to a CORRECTION reconcile — the
                # runner's forbid probe then has a real stale value.
                old = "%s-%04d-legacy" % (predicate, i)
                claims.append(_claim(subject, predicate, old))
                claims.append(_claim(
                    subject, predicate, value, defer="correction",
                    supersedes=["AUTO:%s|%s|%s" % (subject, predicate, old)]))
                legacy[rel] = old
                qid = "q%03d-corrected" % i
            else:
                claims.append(_claim(subject, predicate, value))
                qid = "q%03d" % i
            q = {"id": qid,
                 "question": "What is the %s %s?" % (subject, predicate),
                 "expect": [{"subject": subject, "predicate": predicate,
                             "value": value}]}
            if rel in legacy:
                q["forbid"] = [legacy[rel]]
            questions.append(q)
        sources[rel] = {"claims": claims}
        path = os.path.join(corpus, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(_source_text(rng, rel, subject, claims))

    # unanswerable probes use words outside the topic/predicate space
    questions.append({"id": "q-unanswerable-1", "expect": [],
                      "question": "What is the unobtainium retention?"})
    questions.append({"id": "q-unanswerable-2", "expect": [],
                      "question": "Which hyperdrive powers the canteen?"})

    manifest = {
        "seed": seed, "n": n, "fault": fault,
        "corpus_dir": "corpus",
        "sources": sources,
        "gold_questions": questions,
        "fault_pages": fault_pages(fault, seed) if fault else [],
        "expected_findings": EXPECTED_FINDINGS.get(fault, []),
    }
    with open(os.path.join(dest, "manifest.json"), "w",
              encoding="utf-8") as f:
        json.dump(manifest, f, indent=2, sort_keys=True)
        f.write("\n")
    return dest


def tree_hash(dirpath):
    """SHA-256 over sorted (relpath, content-sha256) — the determinism
    check's canonical corpus fingerprint."""
    entries = []
    for dirpath_w, dirnames, filenames in os.walk(dirpath):
        dirnames.sort()
        for name in sorted(filenames):
            p = os.path.join(dirpath_w, name)
            rel = os.path.relpath(p, dirpath)
            h = hashlib.sha256()
            with open(p, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 16), b""):
                    h.update(chunk)
            entries.append((rel.replace(os.sep, "/"), h.hexdigest()))
    h = hashlib.sha256()
    for rel, digest in entries:
        h.update(("%s %s\n" % (digest, rel)).encode("utf-8"))
    return h.hexdigest()
