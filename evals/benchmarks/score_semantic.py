#!/usr/bin/env python3
"""Mechanical scorer for two-arm semantic results (no LLM judgment)."""
import glob
import json
import os
import re
import sys


REFUSAL_RE = re.compile(
    r"cannot (?:determine|answer|locate)"
    r"|not (?:documented|available|answerable|recorded)"
    r"|no evidence",
    re.I)


def _cite_ok(cite, corpus_dir, wiki_root):
    if cite.startswith("claim_"):
        return wiki_root and bool(glob.glob(
            os.path.join(wiki_root, ".llm-wiki", "claims", cite + ".json")))
    path = cite.split(":")[0]
    return os.path.isfile(os.path.join(corpus_dir, path))


def main(jsonl_path, corpus_dir, wiki_root=None):
    rows = [json.loads(l) for l in open(jsonl_path) if l.strip()]
    out = []
    for r in rows:
        citations = r.get("citations", [])
        cites_ok = all(_cite_ok(c, corpus_dir, wiki_root) for c in citations)
        # Answerable rows must cite at least one resolvable source; an empty
        # citation list only vacuously passes when the row is a refusal.
        checks = {"citation_validity": cites_ok and
                  (bool(citations) or not r.get("answerable"))}
        a = r.get("answer", "")
        sc = r.get("scenario", "")
        if sc == "correction":
            checks["value_correct"] = bool(re.search(r"\b7[-\s]*d(?:ays?)?\b", a))
        elif sc == "historical":
            checks["value_correct"] = bool(re.search(r"\b30[-\s]*d(?:ays?)?\b", a))
        elif sc == "conflict":
            checks["both_sides"] = bool(re.search(r"\b3\b", a) and re.search(r"\b5\b", a))
        elif sc == "unanswerable":
            checks["refused"] = (not r.get("answerable")) or \
                bool(REFUSAL_RE.search(a))
        out.append({**r, "checks": checks, "pass": all(checks.values())})
    summary = {}
    for arm in ("grep", "wiki"):
        arm_rows = [r for r in out if r.get("arm") == arm]
        if arm_rows:
            summary[arm] = {
                "n": len(arm_rows),
                "pass_rate": round(sum(1 for r in arm_rows if r["pass"]) / len(arm_rows), 3),
                "by_check": {k: sum(1 for r in arm_rows if r["checks"].get(k))
                             for k in ("citation_validity", "value_correct",
                                       "both_sides", "refused")},
            }
    return {"rows": out, "summary": summary}


if __name__ == "__main__":
    print(json.dumps(main(sys.argv[1], sys.argv[2],
                          sys.argv[3] if len(sys.argv) > 3 else None), indent=2))
