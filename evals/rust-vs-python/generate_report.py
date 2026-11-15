#!/usr/bin/env python3
"""P7 integration: merge lane outputs -> results.json + report.md (verdict).

Inputs (lane runners):
    quality-rust.json quality-python.json quality-baseline.json  (lane A)
    quality.json                                                   (lane A significance)
    perf.json install_ux.json                                      (lane B)

Outputs:
    results.json  - merged artifact (P8 render input)
    report.md     - metric tables, deltas, significance, go/no-go verdict
                    on the phase-6 flip (P7 sign-off gate)

Verdict rules (spec "Benchmark & evaluation requirements"):
    go := no quality regression vs python (F1 not materially lower AND
          exact/semantic/citation not materially lower) AND wins delivered
          (install time-to-working measured + speed wins on >=2 of
          {ingest, compile, query} medians)
    else no-go with named blockers (user may waive explicitly)

Deterministic given inputs (only generated_at varies). Stdlib, Py >= 3.9.
"""
import datetime
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(HERE, "results.json")
REPORT = os.path.join(HERE, "report.md")

SCHEMA = "rust-vs-python/1"
QUALITY_LANES = ("rust", "python", "baseline")
PP_TOLERANCE = 2.0  # percentage points


def _load(name):
    p = os.path.join(HERE, name)
    if not os.path.isfile(p):
        return None
    with open(p, encoding="utf-8") as f:
        return json.load(f)


def _fmt(x, nd=3):
    return "n/a" if x is None else f"{x:.{nd}f}"


def _med(d, *path):
    cur = d
    for k in path:
        if not isinstance(cur, dict) or k not in cur:
            return None
        cur = cur[k]
    if isinstance(cur, dict):
        for key in ("median_s", "median_ms", "median", "seconds", "ms"):
            if key in cur:
                return cur[key]
    return cur if isinstance(cur, (int, float)) else None


def build_results():
    quality = {l: _load(f"quality-{l}.json") for l in QUALITY_LANES}
    quality = {k: v for k, v in quality.items() if v is not None}
    sig = _load("quality.json") or {}
    if "significance" in sig:
        quality["significance"] = sig["significance"]

    # derived per-lane token savings vs baseline context bytes
    base_bytes = None
    if "baseline" in quality:
        base_bytes = quality["baseline"].get("aggregates", {}).get(
            "context_bytes_mean")
    if isinstance(base_bytes, (int, float)) and base_bytes > 0:
        for lane in ("rust", "python"):
            agg = quality.get(lane, {}).get("aggregates", {})
            cb = agg.get("context_bytes_mean")
            if isinstance(cb, (int, float)):
                agg["token_savings"] = round(1.0 - cb / base_bytes, 4)

    perf = _load("perf.json")
    install = _load("install_ux.json")

    results = {
        "schema": SCHEMA,
        "generated_at": datetime.datetime.now(datetime.timezone.utc)
        .isoformat(timespec="seconds"),
        "quality": quality,
        "performance": perf or {},
        "install_ux": install or {},
    }
    binary = os.path.join(
        HERE, "..", "..", "rust", "target", "release", "llm-wiki")
    if os.path.isfile(binary):
        results["repo"] = {"rust_binary_bytes": os.path.getsize(binary)}
    results["verdict"] = verdict(quality, perf, install)
    return results


def verdict(quality, perf, install):
    """Structured go/no-go per the spec rules. Pure function of inputs."""
    checks, blockers = [], []
    ra = quality.get("rust", {}).get("aggregates", {})
    pa = quality.get("python", {}).get("aggregates", {})

    def m(a, key):
        v = a.get(key)
        return v if isinstance(v, (int, float)) else None

    # --- quality parity (rust must not regress vs python) -------------------
    f1_r, f1_p = m(ra, "f1"), m(pa, "f1")
    tests = {}
    cmp_ = (quality.get("significance") or {}).get("rust_vs_python") or {}
    if isinstance(cmp_, dict):
        tests = cmp_.get("tests") or {}
    cc = tests.get("correct_current") or {}
    p_worse = cc.get("p_first_is_worse")

    if f1_r is None or f1_p is None:
        checks.append(("quality_f1_parity", "unknown",
                       "F1 missing for one lane"))
        blockers.append("F1 not comparable (missing lane data)")
    else:
        dpp = round(100.0 * (f1_r - f1_p), 2)
        materially_lower = dpp < -PP_TOLERANCE
        sig_worse = isinstance(p_worse, (int, float)) and p_worse < 0.05
        if materially_lower and (sig_worse or p_worse is None):
            status = "fail"
            blockers.append(
                f"rust F1 lower than python by {-dpp:.2f}pp "
                f"(p_worse={_fmt(p_worse)})")
        else:
            status = "pass"
        checks.append(("quality_f1_parity", status,
                       f"rust-python F1 delta {dpp:+.2f}pp, "
                       f"p(rust worse)={_fmt(p_worse)}"))

    for key, label in (("exact_match_pct", "exact match"),
                       ("mean_semantic", "semantic match"),
                       ("citation_coverage_pct", "citation coverage"),
                       ("token_savings", "token savings")):
        vr, vp = m(ra, key), m(pa, key)
        if vr is None or vp is None:
            checks.append((f"quality_{key}", "unknown", f"{label} missing"))
            continue
        dpp = round(100.0 * (vr - vp), 2)
        status = "fail" if dpp < -PP_TOLERANCE else "pass"
        if status == "fail":
            blockers.append(f"{label} {dpp:+.2f}pp below tolerance")
        checks.append((f"quality_{key}", status, f"delta {dpp:+.2f}pp"))

    # --- speed wins ---------------------------------------------------------
    wins = 0
    for name, path in (
            ("ingest", ("engines", "rust", "ingest", "per_file_s")),
            ("compile", ("engines", "rust", "compile", "total_s")),
            ("query", ("engines", "rust", "query"))):
        tr = _med(perf or {}, *path)
        tp = _med(perf or {}, *(path[:1] + ("python",) + path[2:]))
        if tr is None or tp is None:
            checks.append((f"speed_{name}", "unknown", "timing missing"))
            continue
        ratio = round(tp / tr, 2) if tr else None
        status = "pass" if (ratio or 0) > 1.0 else "fail"
        wins += status == "pass"
        checks.append((f"speed_{name}", status,
                       f"python/rust = {ratio}x "
                       f"(rust {_fmt(tr)}s vs python {_fmt(tp)}s)"))
    if wins < 2:
        blockers.append(f"speed wins on {wins}/3 core ops (need >=2)")

    # --- install win --------------------------------------------------------
    ttw = None
    if isinstance(install, dict):
        b = install.get("binary_install") or {}
        if isinstance(b, dict):
            ttw = b.get("time_to_working_s")
            ttw = ttw if isinstance(ttw, (int, float)) else None
    checks.append(("install_time_to_working",
                   "pass" if ttw is not None else "unknown",
                   f"{_fmt(ttw, 1)}s"))
    if ttw is None:
        blockers.append("install time-to-working not measured")

    go = not blockers
    return {
        "go_no_go": "go" if go else "no-go",
        "checks": [{"name": n, "status": s, "detail": d}
                   for n, s, d in checks],
        "blockers": blockers,
        "waiver_note": ("no waiver needed" if go else
                        "explicit user-approved waiver required to proceed"),
    }


def render_report(results):
    v = results["verdict"]
    q = results["quality"]
    perf = results.get("performance", {})
    inst = results.get("install_ux", {})
    L = []

    def w(s=""):
        L.append(s)

    w("# rust-vs-python evaluation report (P7)")
    w()
    w(f"Generated: {results['generated_at']}  ")
    w(f"Schema: `{SCHEMA}`  ")
    w(f"**Verdict: {v['go_no_go'].upper()}**"
      + ("" if v["go_no_go"] == "go" else
         f" — blockers: {'; '.join(v['blockers'])}"))
    w()

    lanes = [l for l in QUALITY_LANES if q.get(l)]
    w("## Quality (planted-facts battery, "
      + " / ".join(f"{l}: n={q[l].get('aggregates', {}).get('questions', '?')}"
                   for l in lanes) + ")")
    w()
    METRICS = (("precision", "Precision"), ("recall", "Recall"),
               ("f1", "F1"), ("exact_match_pct", "Exact match"),
               ("mean_semantic", "Semantic"), ("correct_current_pct",
                                               "Answer current"),
               ("citation_coverage_pct", "Citations"),
               ("token_savings", "Token savings vs baseline"))
    w("| Metric | " + " | ".join(lanes) + " |")
    w("|---|" + "---|" * len(lanes))
    for key, label in METRICS:
        row = []
        for lane in lanes:
            val = q[lane].get("aggregates", {}).get(key)
            if not isinstance(val, (int, float)):
                row.append("n/a")
            elif key in ("context_bytes_mean",):
                row.append(f"{val:.0f}")
            else:
                row.append(f"{100 * val:.2f}%"
                           if val <= 1.00001 else f"{val:.3f}")
        w(f"| {label} | " + " | ".join(row) + " |")
    k = q.get("rust", {}).get("aggregates", {}).get("k")
    if k:
        w(f"_match = expected claim in top-{k} returned; "
          f"context bytes method: "
          f"{q.get('rust', {}).get('aggregates', {}).get('context_bytes_method', '?')}_")
    w()

    sig = q.get("significance") or {}
    if sig:
        w("Permutation significance (one-sided paired, 20000 perms, "
          "seed 20261007):")
        w()
        w("| Comparison | metric | p(better) | p(worse) | means |")
        w("|---|---|---|---|---|")
        for name in sorted(sig):
            t = (sig.get(name) or {}).get("tests") or {}
            for metric, s in sorted(t.items()):
                if not isinstance(s, dict):
                    continue
                first = name.split("_vs_")[0]
                second = name.split("_vs_")[-1] if "_vs_" in name else "?"
                w(f"| {name} | {metric} | {s.get('p_first_is_better')} "
                  f"| {s.get('p_first_is_worse')} "
                  f"| {first}={_fmt(s.get('mean_' + first))} "
                  f"vs {second}={_fmt(s.get('mean_' + second))} |")
        w()

    w("## Memory effectiveness (correction propagation)")
    w()
    w("| Lane | cases | invalidation rate | stale-answer rate |")
    w("|---|---|---|---|")
    for lane in lanes:
        mem = q.get(lane, {}).get("memory") or {}
        w(f"| {lane} | {mem.get('correction_cases') or '?'} "
          f"| {100 * (mem.get('invalidation_rate') or 0):.0f}% "
          f"| {100 * (mem.get('stale_answer_rate') or 0):.0f}% |")
    w()

    w("## Performance (medians, k reps — lane B)")
    w()
    w("```json")
    w(json.dumps(perf, indent=2, sort_keys=True))
    w("```")
    w()

    w("## Install / UX (lane B)")
    w()
    w("```json")
    w(json.dumps(inst, indent=2, sort_keys=True))
    w("```")
    w()

    w("## Verdict checks")
    w()
    w("| Check | Status | Detail |")
    w("|---|---|---|")
    for c in v["checks"]:
        w(f"| {c['name']} | {c['status']} | {c['detail']} |")
    w()
    if v["blockers"]:
        w("Blockers:")
        for b in v["blockers"]:
            w(f"- {b}")
        w()
    w(v["waiver_note"] + ".")
    w()
    return "\n".join(L)


def main():
    results = build_results()
    with open(RESULTS, "w", encoding="utf-8") as f:
        json.dump(results, f, indent=2, sort_keys=True)
        f.write("\n")
    with open(REPORT, "w", encoding="utf-8") as f:
        f.write(render_report(results))
    v = results["verdict"]
    print(f"results.json + report.md written; verdict: {v['go_no_go']}")
    for b in v["blockers"]:
        print(f"  blocker: {b}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
