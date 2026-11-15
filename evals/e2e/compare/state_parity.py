#!/usr/bin/env python3
"""Deterministic-layer state parity: run run_e2e.setup_project(mode) for each
build-producing mode under BOTH engines, then diff the resulting .llm-wiki
trees after normalizing the genuinely non-deterministic bits:

- ULIDs (claim_/source_/decision_/run_ + 26-char Crockford ids): renumbered
  in sorted order per engine -> claim#1, source#2, ... Referential structure
  is preserved (same id -> same placeholder) so cross-links still diff.
- ISO timestamps -> <TS>; wallclock fields differ run to run.
- Project absolute path -> <PROJ>.

Exit 0 on parity for every mode; 1 when any normalized diff exists.
The temp parents keep identical basenames (proj-a/proj-b) so no path noise.
"""
import json
import os
import re
import shutil
import sys
import tempfile

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.abspath(os.path.join(_HERE, "..", "..", ".."))
sys.path.insert(0, os.path.join(_REPO, "evals", "e2e"))
sys.path.insert(0, os.path.join(_REPO, "evals"))
sys.path.insert(0, os.path.join(_REPO, "evals", "benchmarks"))

import _engine  # noqa: E402
import assertions  # noqa: E402
import run_e2e  # noqa: E402
from driver import Engine  # noqa: E402

MODES = ("init+inbox", "build", "build_uncorrected")
ID_RE = re.compile(r"\b(claim|source|decision|run)_([0-9A-HJKMNP-TV-Z]{26})\b")
TS_RE = re.compile(r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b")


def engines():
    py = Engine(assertions.engine_path())
    rust = run_e2e._CliEngine(_engine.engine_argv("rust"))
    return (("python", py), ("rust", rust))


def normalize_tree(root):
    """Return {normalized_relpath: normalized_content} for .llm-wiki + inbox."""
    entries = []  # (raw_rel, data bytes-or-None for dirs)
    for base, dirs, files in os.walk(root):
        for d in dirs:
            entries.append((os.path.relpath(os.path.join(base, d), root), None))
        for f in files:
            p = os.path.join(base, f)
            rel = os.path.relpath(p, root)
            with open(p, "rb") as fh:
                entries.append((rel, fh.read()))
    # renumber ids deterministically: sort every id occurrence
    ids = sorted({m.group(0) for rel, data in entries if data is not None
                  for m in ID_RE.finditer(data.decode("utf-8", "replace"))}
                 | {m.group(0) for rel, _ in entries
                    for m in ID_RE.finditer(rel)})
    idmap = {}
    counters = {}
    for raw in ids:
        prefix = raw.split("_", 1)[0]
        counters[prefix] = counters.get(prefix, 0) + 1
        idmap[raw] = "%s#%d" % (prefix, counters[prefix])

    def norm_text(s):
        s = ID_RE.sub(lambda m: idmap[m.group(0)], s)
        s = TS_RE.sub("<TS>", s)
        # macOS: /tmp -> /private/tmp; rust canonicalizes, python does not
        s = s.replace("/private" + root, "<PROJ>")
        s = s.replace(root, "<PROJ>")
        return s

    out = {}
    for rel, data in sorted(entries):
        nrel = norm_text(rel)
        if data is None:
            out[nrel + "/"] = None
            continue
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            out[nrel] = data  # binary: raw compare
            continue
        text = norm_text(text)
        if rel.endswith(".json"):
            try:
                text = json.dumps(json.loads(text), sort_keys=True,
                                  indent=1, ensure_ascii=False)
            except ValueError:
                pass
        elif rel.endswith(".jsonl"):
            rows = []
            ok = True
            for line in text.splitlines():
                if not line.strip():
                    continue
                try:
                    rows.append(json.loads(line))
                except ValueError:
                    ok = False
                    break
            if ok:
                text = json.dumps(rows, sort_keys=True, indent=1,
                                  ensure_ascii=False)
        out[nrel] = text
    return out


def diff_trees(a, b):
    """Yield human-readable diff lines between normalized tree dicts."""
    for k in sorted(set(a) | set(b)):
        if k not in a:
            yield "ONLY-IN-RUST  %s" % k
        elif k not in b:
            yield "ONLY-IN-PYTHON %s" % k
        elif a[k] != b[k]:
            if a[k] is None or b[k] is None or not isinstance(a[k], str):
                yield "DIFF         %s (binary/dirty)" % k
                continue
            import difflib
            dl = list(difflib.unified_diff(
                a[k].splitlines(), b[k].splitlines(),
                "python/" + k, "rust/" + k, lineterm="", n=1))
            yield "DIFF         %s" % k
            for line in dl[:40]:
                yield "    " + line
            if len(dl) > 40:
                yield "    ... (%d more diff lines)" % (len(dl) - 40)


def main():
    report = {}
    any_diff = False
    parent = tempfile.mkdtemp(prefix="e2e-parity-")
    for mode in MODES:
        trees = {}
        for name, eng in engines():
            proj = os.path.join(parent, "%s-%s" % (mode, name))
            os.makedirs(proj)
            shutil.copytree(run_e2e.CORPUS, proj, dirs_exist_ok=True)
            run_e2e.setup_project(mode, eng, proj)
            trees[name] = normalize_tree(proj)
        diffs = list(diff_trees(trees["python"], trees["rust"]))
        report[mode] = {
            "files_python": sum(1 for v in trees["python"].values()
                                if v is not None),
            "files_rust": sum(1 for v in trees["rust"].values()
                              if v is not None),
            "parity": not diffs,
            "diffs": diffs,
        }
        print("[%s] %s  files py=%d rust=%d  diffs=%d"
              % (mode, "PARITY" if not diffs else "DIVERGENCE",
                 report[mode]["files_python"], report[mode]["files_rust"],
                 len(diffs)))
        for d in diffs[:60]:
            print("   " + d)
        any_diff = any_diff or bool(diffs)
    out = os.path.join(_HERE, "state-parity.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print("wrote %s" % out)
    shutil.rmtree(parent, ignore_errors=True)
    return 1 if any_diff else 0


if __name__ == "__main__":
    sys.exit(main())
