#!/usr/bin/env python3
"""Side-by-side diff of two bench JSON files. Exit 0 always (report tool)."""
import json
import sys

BETTER_UP = {"gold_recall_at_5", "precision_at_5", "mrr", "compression_ratio",
             "section_indexes_linked", "corroborated"}
BETTER_DOWN = {"pack_tokens_avg", "orphan_pages", "dead_end_pages",
               "near_dup_pairs", "waiting"}


def flat(d, prefix=""):
    out = {}
    for k, v in d.items():
        key = "%s.%s" % (prefix, k) if prefix else k
        if isinstance(v, dict):
            out.update(flat(v, key))
        else:
            out[key] = v
    return out


def main(argv=None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if len(argv) != 2:
        print("usage: compare.py <before.json> <after.json>", file=sys.stderr)
        return 0
    try:
        with open(argv[0]) as f:
            before_doc = json.load(f)
        with open(argv[1]) as f:
            after_doc = json.load(f)
    except (OSError, ValueError) as e:
        print("compare: cannot read bench files: %s" % e, file=sys.stderr)
        return 0

    before, after = flat(before_doc), flat(after_doc)
    keys = sorted(set(before) | set(after))
    rows = []
    for k in keys:
        # doctor_findings_by_code is a nested dict: flat() expands its codes
        # into per-code keys — they get their own table below, not rows here
        if k.startswith("hygiene.doctor_findings_by_code"):
            continue
        b, a = before.get(k), after.get(k)
        if isinstance(b, dict) or isinstance(a, dict):
            continue  # any other dict-valued metric: compared separately
        leaf = k.rsplit(".", 1)[-1]
        if b is None or a is None or b == a:
            arrow, verdict = "=", "unchanged/new"
        elif leaf in BETTER_UP:
            arrow, verdict = ("↑", "better") if a > b else ("↓", "worse")
        elif leaf in BETTER_DOWN:
            arrow, verdict = ("↓", "better") if a < b else ("↑", "worse")
        else:
            arrow, verdict = "=", "informational"
        rows.append((k, b, a, arrow, verdict))
    print("| metric | before | after | delta | verdict |")
    print("|---|---|---|---|---|")
    for k, b, a, arrow, verdict in rows:
        print("| `%s` | %s | %s | %s | %s |" % (k, b, a, arrow, verdict))
    bf = before_doc.get("hygiene", {}).get("doctor_findings_by_code", {})
    af = after_doc.get("hygiene", {}).get("doctor_findings_by_code", {})
    print("\n| doctor code | before | after |")
    print("|---|---|---|")
    for code in sorted(set(bf) | set(af)):
        print("| `%s` | %s | %s |" % (code, bf.get(code, 0), af.get(code, 0)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
