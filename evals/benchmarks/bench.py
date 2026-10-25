#!/usr/bin/env python3
"""Deterministic before/after benchmark.

Usage: python3 bench.py --engine <path/to/wiki.py> --label <name> \
                        --corpus <golden-corpus-dir> [--out dir]
Builds the planted wiki via the CLI, injects the stray page, seeds the
original "Attribution Window" page, attempts the near-duplicate write,
collects metrics, writes bench-<label>.json.
"""
import argparse
import glob
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import driver  # noqa: E402
import fixture  # noqa: E402
import metrics  # noqa: E402


def _current_claim_link(wiki_root: str) -> str:
    """First live (non-superseded/rejected) attribution claim id, '' if none.

    A current claim keeps write-page deps validation satisfied on every
    engine revision, so the near-duplicate guard is the only check that can
    refuse the probe write on HEAD."""
    for cp in sorted(glob.glob(os.path.join(
            wiki_root, ".llm-wiki", "claims", "claim_*.json"))):
        with open(cp, encoding="utf-8") as f:
            c = json.load(f)
        if (c.get("subject") == "attribution"
                and c.get("status") not in ("superseded", "rejected")):
            return "[[%s]]" % c["id"]
    return ""


def _concept_md(subject: str, title: str, link: str) -> str:
    """Concept-page markdown that passes write-page validation: type concept,
    ISO created timestamp, subject matching the slug (spaces -> hyphens)."""
    return ("---\ntype: concept\nsubject: %s\ntitle: %s\n"
            "created: 2026-09-15T00:00:00Z\n---\n\n# %s\n\n%s\n"
            % (subject, title, title, link or ""))


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", required=True, help="path to wiki.py under test")
    ap.add_argument("--label", required=True)
    ap.add_argument("--corpus", default=os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "golden-corpus"))
    ap.add_argument("--out", default=os.path.dirname(os.path.abspath(__file__)))
    args = ap.parse_args(argv)

    engine = driver.Engine(os.path.abspath(args.engine))
    root = tempfile.mkdtemp(prefix="llmwiki-bench-")
    corpus = os.path.abspath(args.corpus)
    try:
        build = driver.build_wiki(engine, corpus, root)
        if not build.get("ok"):
            print(json.dumps({"label": args.label, "error": "build failed",
                              "steps": build.get("steps", [])}, indent=2))
            return 1

        # probe 1: stray page (filesystem-level, engine-independent)
        stray_path = os.path.join(root, ".llm-wiki", fixture.STRAY_PAGE_REL)
        os.makedirs(os.path.dirname(stray_path), exist_ok=True)
        with open(stray_path, "w", encoding="utf-8") as f:
            f.write(fixture.STRAY_PAGE_TEXT)

        # probe 2: near-duplicate write attempt via the engine. The original
        # page is seeded first ("Attribution Window" then "Attribution Window
        # Backup", per the fixture): the extend-don't-create guard can only
        # refuse a NEW page when a similar page already exists. If no live
        # attribution claim was found the link is empty and the write fails
        # deps validation instead — recorded via the exit codes.
        link = _current_claim_link(root)
        orig_subject = " ".join(fixture.NEAR_DUP_SUBJECT.split()[:-1])
        orig_title = " ".join(fixture.NEAR_DUP_TITLE.split()[:-1])
        seed_code, _, _ = engine.run(
            ["write-page", "--text", _concept_md(orig_subject, orig_title, link),
             "--artifact", "wiki/concepts/%s.md" % orig_subject.replace(" ", "-")],
            root)
        nd_code, _, _ = engine.run(
            ["write-page", "--text",
             _concept_md(fixture.NEAR_DUP_SUBJECT, fixture.NEAR_DUP_TITLE, link),
             "--artifact", "wiki/concepts/%s.md"
             % fixture.NEAR_DUP_SUBJECT.replace(" ", "-")],
            root)
        near_dup_blocked = (nd_code == 3)

        hyg = metrics.hygiene_and_guard(engine, root)
        result = {
            "label": args.label,
            "engine": os.path.abspath(args.engine),
            "build_steps": build["steps"],
            "guard": {
                "near_dup_blocked": near_dup_blocked,
                "stray_orphan_detected_by_doctor":
                    hyg["stray_orphan_detected_by_doctor"],
                "seed_write_exit": seed_code,
                "near_dup_write_exit": nd_code,
            },
            "retrieval": metrics.retrieval(engine, root),
            "context": metrics.context(engine, root, corpus),
            "topology": metrics.topology(root),
            "twosource": metrics.twosource(engine, root),
            "hygiene": {"doctor_findings_by_code":
                        hyg["doctor_findings_by_code"]},
        }
        out_path = os.path.join(args.out, "bench-%s.json" % args.label)
        with open(out_path, "w", encoding="utf-8") as f:
            json.dump(result, f, indent=2, sort_keys=True)
        print(out_path)
        return 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
