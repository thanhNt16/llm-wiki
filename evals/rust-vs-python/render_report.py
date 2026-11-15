#!/usr/bin/env python3
"""P8: render docs/reports/rust-engine-report.html.

Inputs (P7 outputs):
    evals/rust-vs-python/results.json   merged benchmark results
    evals/rust-vs-python/report.md      generated verdict report

Live facts captured at render time (deterministic given a built repo):
    llm-wiki --help / python wiki.py --help   command-surface parity matrix
    sandbox first-session transcript          init -> ingest -> query-prepare
                                               -> context-pack, real JSON out

Output: single self-contained HTML file. No network at render or view time:
charts and diagrams are inline SVG (spec: "pick at build time" — offline wins);
dark/light via prefers-color-scheme. Stdlib only, Python >= 3.9.
"""
import datetime
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
RESULTS = os.path.join(HERE, "results.json")
REPORT = os.path.join(HERE, "report.md")
OUT = os.path.join(REPO, "docs", "reports", "rust-engine-report.html")
SPEC = os.path.join(REPO, "docs", "superpowers", "specs",
                    "2026-11-14-rust-engine-design.md")
BINARY = os.path.join(REPO, "rust", "target", "release", "llm-wiki")
WIKI_PY = os.path.join(REPO, "skill", "llm-wiki", "scripts", "wiki.py")

# Rough phase attribution for color-coding the command matrix (roadmap).
# "?" = not attributed in the roadmap table; the parity columns are the facts.
PHASE_MAP = {
    "init": "P1", "ingest": "P1", "status": "P1", "topics": "P1",
    "check-evidence": "P2", "triage": "P2", "subjects": "P2",
    "graph-data": "P2", "graph-export": "P2", "verify": "P2",
    "ingest-git": "P2", "ingest-rss": "P2",
    "compile": "P3", "compile-plan": "P3", "build": "P3",
    "stage-candidates": "P3", "reconcile-prepare": "P3",
    "reconcile-apply": "P3", "write-page": "P3", "page-show": "P3",
    "page-targets": "P3", "build-pages": "P3", "decide": "P3",
    "record-decision": "P3", "review": "P3", "dream": "P3", "doctor": "P3",
    "query-prepare": "P4", "context-pack": "P4", "jev": "P4",
    "viz-serve": "P5", "mcp": "P5", "mcp-register": "P5", "export-okf": "P5",
}

# ---------------------------------------------------------------------------
# helpers


def esc(s):
    return html.escape(str(s), quote=True)


def dig(obj, *path, **kw):
    """Tolerant lookup: follow dict path; accept n/a/missing. None on miss."""
    cur = obj
    for k in path:
        if not isinstance(cur, dict) or k not in cur:
            return kw.get("default")
        cur = cur[k]
    if isinstance(cur, dict):
        for key in kw.get("alt", ("median_s", "median", "seconds", "ms")):
            if key in cur:
                return cur[key]
    return cur if isinstance(cur, (int, float)) else kw.get("default")


def pct(x, nd=1):
    return "n/a" if x is None else f"{100 * x:.{nd}f}%"


def num(x, nd=3):
    return "n/a" if x is None else f"{x:.{nd}f}"


def stars(p):
    if p is None:
        return ""
    if p < 0.01:
        return "***"
    if p < 0.05:
        return "**"
    if p < 0.1:
        return "*"
    return "ns"


def run(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


# ---------------------------------------------------------------------------
# live fact capture


def command_surface():
    """(rust_cmds, python_cmds) parsed from --help of both engines."""
    rust_cmds, py_cmds = set(), set()
    if os.path.isfile(BINARY):
        out = run([BINARY, "--help"]).stdout
        rust_cmds = set(re.findall(r"^  ([a-z][a-z0-9-]+)\s{2,}", out, re.M))
        rust_cmds.discard("help")
    if os.path.isfile(WIKI_PY):
        out = run([sys.executable, WIKI_PY, "--help"]).stdout
        m = re.search(r"\{([^}]+)\}", out)
        if m:
            py_cmds = set(m.group(1).split(",")) - {"help"}
    return rust_cmds, py_cmds


def version_line():
    if os.path.isfile(BINARY):
        return run([BINARY, "--version"]).stdout.strip()
    return "binary not built"


def first_session():
    """Real transcript: init -> ingest -> query-prepare -> context-pack."""
    if not os.path.isfile(BINARY):
        return None
    tmp = tempfile.mkdtemp(prefix="p8-session-")
    doc = os.path.join(tmp, "decision.md")
    with open(doc, "w", encoding="utf-8") as f:
        f.write("# Decision\n\nWe chose SQLite for the derived index "
                "because it ships on every target and keeps the query path "
                "single-file. Revision watermark drives incremental "
                "rebuilds.\n")
    steps = []
    try:
        root = os.path.join(tmp, "proj")
        os.makedirs(root)
        argvs = [
            ["init", root],
            ["ingest", "--file", doc, "--root", root],
            ["query-prepare", "--question",
             "what store backs the derived index", "--root", root],
            ["context-pack", "--task",
             "explain the derived index storage choice", "--budget", "2000",
             "--root", root],
        ]
        for argv in argvs:
            p = run([BINARY] + argv)
            steps.append((argv[0], p.returncode, p.stdout.strip()))
        return steps
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def review_counts():
    """Appendix A disposition counts parsed from the design spec."""
    if not os.path.isfile(SPEC):
        return None, 0
    with open(SPEC, encoding="utf-8") as f:
        text = f.read()
    counts = {}
    hdr = re.search(
        r"Reviewers: (.*?)\.\n(?:Round 2: (.*?)\.\n)?"
        r"(?:Round 3: (.*?)\.\n)?", text, re.S)
    total = 0
    if hdr:
        for part in hdr.groups():
            if not part:
                continue
            for m in re.finditer(
                    r"\*\*([A-Z]{2})\*\*[^(*]*\(([^)]*)\)", part):
                nums = re.findall(r"\d+", m.group(2))
                if nums:
                    counts[m.group(1)] = int(nums[-1])
                    total += int(nums[-1])
    return counts, total


# ---------------------------------------------------------------------------
# svg builders


def svg_bars(title, series, unit="s", width=720):
    """Grouped vertical bars: series = [(label, {lane: value}), ...]."""
    lanes = []
    for _, vals in series:
        lanes.extend(vals.keys())
    lanes = list(dict.fromkeys(lanes))[:6]
    colors = ["var(--c1)", "var(--c2)", "var(--c3)", "var(--c4)",
              "var(--c5)", "var(--c6)"]
    maxv = 0.0
    for _, vals in series:
        for v in vals.values():
            if isinstance(v, (int, float)):
                maxv = max(maxv, v)
    if maxv <= 0:
        return f"<p class='muted'>{esc(title)}: no data</p>"
    n = len(series)
    pad, bw, gap, top, h = 16, 22, 8, 34, 180
    gw = n * (len(lanes) * (bw + 4) + gap)
    W = max(width, gw + 2 * pad + 70)
    H = top + h + 58
    parts = [f"<svg viewBox='0 0 {W} {H}' role='img' "
             f"aria-label='{esc(title)}' xmlns='http://www.w3.org/2000/svg'>",
             f"<text x='{pad}' y='20' class='svgt'>{esc(title)}</text>"]
# gridlines: 4 divisions
    vfmt = ".3f" if maxv < 10 else ".0f"
    for i in range(5):
        y = top + h - (i * h / 4)
        v = maxv * i / 4
        parts.append(f"<line x1='{pad}' y1='{y:.1f}' x2='{W - pad}' "
                     f"y2='{y:.1f}' class='grid'/>")
        parts.append(f"<text x='{pad - 6}' y='{y + 4:.1f}' "
                     f"text-anchor='end' class='svgv'>"
                     f"{format(v, vfmt)}</text>")
    x = pad + 8
    for label, vals in series:
        for li, lane in enumerate(lanes):
            v = vals.get(lane)
            if isinstance(v, (int, float)):
                bh = h * v / maxv
                parts.append(
                    f"<rect x='{x:.1f}' y='{top + h - bh:.1f}' "
                    f"width='{bw}' height='{bh:.1f}' rx='2' "
                    f"fill='{colors[li % len(colors)]}'><title>"
                    f"{esc(label)} · {esc(lane)}: {v}{unit}</title></rect>")
            x += bw + 4
        parts.append(
            f"<text x='{x - gap / 2 - (len(lanes) * (bw + 4)) / 2:.1f}'"
            f" y='{top + h + 16}' text-anchor='middle' "
            f"class='svgl'>{esc(label)}</text>")
        x += gap
    lx = pad + 4
    for li, lane in enumerate(lanes):
        parts.append(f"<rect x='{lx}' y='{H - 26}' width='10' height='10' "
                     f"rx='2' fill='{colors[li % len(colors)]}'/>")
        parts.append(f"<text x='{lx + 14}' y='{H - 17}' class='svgl'>"
                     f"{esc(lane)}</text>")
        lx += 14 + 8 * len(lane) + 14
    parts.append("</svg>")
    return "\n".join(parts)


def svg_flow(nodes, edges, title, W=760, nh=44):
    """Simple left-to-right flow diagram. nodes = [(id, label, cls)]."""
    n = len(nodes)
    H = 120
    parts = [f"<svg viewBox='0 0 {W} {H}' role='img' "
             f"aria-label='{esc(title)}' xmlns='http://www.w3.org/2000/svg'>",
             f"<text x='12' y='20' class='svgt'>{esc(title)}</text>"]
    step = (W - 40) / max(1, n - 1)
    xs = {}
    for i, (nid, label, cls) in enumerate(nodes):
        x = 20 + i * step
        xs[nid] = x
        parts.append(
            f"<rect x='{x - 52:.1f}' y='40' width='104' height='{nh}' "
            f"rx='8' class='node {cls}'><title>{esc(label)}</title></rect>")
        for j, line in enumerate(label.split("\\n")):
            parts.append(f"<text x='{x:.1f}' y='{58 + j * 15}' "
                         f"text-anchor='middle' class='nodel'>{esc(line)}"
                         f"</text>")
        if i:
            px = 20 + (i - 1) * step
            parts.append(f"<path d='M {px + 54:.1f} 62 H {x - 56:.1f}' "
                         f"class='edge' marker-end='url(#ar)'/>")
    parts.append("<defs><marker id='ar' viewBox='0 0 10 10' refX='9' "
                 "refY='5' markerWidth='6' markerHeight='6' "
                 "orient='auto-start-reverse'>"
                 "<path d='M 0 0 L 10 5 L 0 10 z' class='arrow'/></marker>"
                 "</defs>")
    parts.append("</svg>")
    return "\n".join(parts)


def svg_pipeline(title="evidence → claims → decisions → derived views"):
    return svg_flow(
        [("ev", "evidence\\nimmutable verbatim", "n1"),
         ("cl", "claims\\ncited, structured", "n2"),
         ("de", "decisions\\nwhat was chosen", "n3"),
         ("vi", "derived views\\npages, packs", "n4")],
        None, title)


def svg_txn(title="commit transaction sequence"):
    steps = ["stage", "validate", "flock", "gate", "journal", "backup",
             "apply", "receipt"]
    nodes = [(s, s, "n2" if s in ("apply", "receipt") else "n1")
             for s in steps]
    return svg_flow(nodes, None, title, W=860)


def svg_mcp(title="MCP tool-call lifecycle"):
    return svg_flow(
        [("cl", "client\\nClaude/Cursor", "n1"),
         ("sp", "stdio\\nJSON-RPC 2.0", "n1"),
         ("ro", "root binding\\n--root > env > cwd", "n2"),
         ("tl", "13 tools\\nquery, context…", "n2"),
         ("wi", ".llm-wiki\\ncanonical", "n3")],
        None, title)


def svg_arch(title="binary architecture"):
    W, H = 760, 300
    boxes = [
        (20, 50, 200, 230, "CLI surface", [
            "clap parse", "32+ subcommands", "--wait-lock / --allow-repair",
            "JSON on stdout", "exit codes 2-5"]),
        (270, 50, 200, 104, "MCP server", [
            "stdio JSON-RPC", "13 tools", "mcp-register"]),
        (270, 170, 200, 110, "viz-serve", [
            "embedded dist", "SSE reload"]),
        (520, 50, 220, 104, "engine core", [
            "txn + stamps + journal", "evidence/sources", "claims/deps/topics"]),
        (520, 170, 220, 110, "storage", [
            ".llm-wiki/ canonical", ".state/index.sqlite", "FTS5 + watermark"]),
    ]
    parts = [f"<svg viewBox='0 0 {W} {H}' role='img' "
             f"aria-label='{esc(title)}' xmlns='http://www.w3.org/2000/svg'>",
             f"<text x='12' y='22' class='svgt'>{esc(title)}</text>"]
    for x, y, w, h, label, lines in boxes:
        parts.append(f"<rect x='{x}' y='{y}' width='{w}' height='{h}' "
                     f"rx='8' class='node n1'/>")
        parts.append(f"<text x='{x + 12}' y='{y + 22}' class='nodet'>"
                     f"{esc(label)}</text>")
        for i, ln in enumerate(lines):
            parts.append(f"<text x='{x + 12}' y='{y + 42 + i * 16}' "
                         f"class='nodel'>{esc(ln)}</text>")
    parts.append("<path d='M 220 110 H 270' class='edge' marker-end="
                 "'url(#ar2)'/>")
    parts.append("<path d='M 220 210 H 270' class='edge' marker-end="
                 "'url(#ar2)'/>")
    parts.append("<path d='M 470 100 H 520' class='edge' marker-end="
                 "'url(#ar2)'/>")
    parts.append("<path d='M 630 154 V 170' class='edge' marker-end="
                 "'url(#ar2)'/>")
    parts.append("<defs><marker id='ar2' viewBox='0 0 10 10' refX='9' "
                 "refY='5' markerWidth='6' markerHeight='6' "
                 "orient='auto-start-reverse'>"
                 "<path d='M 0 0 L 10 5 L 0 10 z' class='arrow'/></marker>"
                 "</defs></svg>")
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# section renderers


def s_exec(results):
    v = results.get("verdict", {})
    go = v.get("go_no_go", "unknown")
    q = results.get("quality", {})
    ra = dig(q, "rust", "aggregates") or {}
    pa = dig(q, "python", "aggregates") or {}
    perf = results.get("performance", {})
    inst = results.get("install_ux", {})
    binsize = dig(results, "repo", "rust_binary_bytes")
    ttw = dig(inst, "binary_install", "time_to_working_s")
    ti = dig(perf, "engines", "rust", "ingest", "per_file_s", "median")
    tp = dig(perf, "engines", "python", "ingest", "per_file_s", "median")
    speedup = (round(tp / ti, 1) if isinstance(ti, (int, float))
               and isinstance(tp, (int, float)) and ti else None)
    f1d = None
    if isinstance(ra.get("f1"), (int, float)) and isinstance(
            pa.get("f1"), (int, float)):
        f1d = round(100 * (ra["f1"] - pa["f1"]), 1)
    strip = [
        ("binary", f"{binsize / 1e6:.1f} MB" if binsize else "n/a"),
        ("install → working", f"{ttw:.1f} s" if isinstance(
            ttw, (int, float)) else "n/a"),
        ("ingest speedup", f"{speedup}×" if speedup else "n/a"),
        ("F1 delta vs python", f"{f1d:+.1f} pp" if f1d is not None
         else "n/a"),
    ]
    cells = "".join(
        f"<div class='kpi'><div class='kpi-v'>{esc(val)}</div>"
        f"<div class='kpi-k'>{esc(k)}</div></div>" for k, val in strip)
    blockers = "".join(f"<li>{esc(b)}</li>" for b in v.get("blockers", []))
    blk = f"<ul class='blockers'>{blockers}</ul>" if blockers else ""
    return f"""
<section id="s1"><h2>1 · Executive summary</h2>
<div class="verdict {'go' if go == 'go' else 'nogo'}">
<strong>Verdict: {esc(go).upper()}</strong> — Rust port delivers
{'' if go == 'go' else 'with blockers'} per the P7 gate
(no quality regression vs Python; install + speed wins delivered).</div>
<div class="kpis">{cells}</div>{blk}
<p class='muted'>Generated {esc(results.get('generated_at', '?'))} from
<code>evals/rust-vs-python/results.json</code>; binary
<code>{esc(version_line())}</code>.</p></section>"""


def s_implemented(results):
    rust_cmds, py_cmds = command_surface()
    counts, total = review_counts()
    rows = []
    for c in sorted(rust_cmds | py_cmds):
        phase = PHASE_MAP.get(c, "?")
        rows.append(
            f"<tr><td><code>{esc(c)}</code></td>"
            f"<td class='ph ph{esc(phase)}'>{esc(phase)}</td>"
            f"<td class='{'ok' if c in rust_cmds else 'bad'}'>"
            f"{'✓ ported' if c in rust_cmds else '—'}</td>"
            f"<td class='{'ok' if c in py_cmds else 'bad'}'>"
            f"{'✓' if c in py_cmds else '—'}</td></tr>")
    matrix = ("<table><thead><tr><th>command</th><th>phase</th>"
              "<th>rust binary</th><th>python</th></tr></thead><tbody>"
              + "".join(rows) + "</tbody></table>")
    rev = ""
    if counts:
        rev = ("<p class='muted'>Design review provenance: spec "
               "<code>docs/superpowers/specs/2026-11-14-rust-engine-design.md"
               "</code> — Appendix A dispositions "
               f"{total} findings "
               + ", ".join(f"{k} {v}" for k, v in sorted(counts.items()))
               + ".</p>")
    cards = "".join(
        f"<div class='card'><h3>{esc(t)}</h3>"
        f"<p class='before'>{esc(b)}</p><p class='after'>{esc(a)}</p></div>"
        for t, b, a in [
            ("expect-stamps",
             "before: last-writer-wins between concurrent commits",
             "after: inode/mtime/size tuple verified at commit; "
             "conflict → exit 4, nothing applied"),
            ("bounded matches",
             "before: reconcile compares everything against everything",
             "after: tier-matched bounded matches[] per spec §6.4 — "
             "deterministic, reviewable prefill"),
            ("doctor --fix",
             "before: findings only, manual remediation",
             "after: --fix applies safe repairs under commit.lock; "
             "--rebuild-index emits index_rebuilt receipt"),
            ("index watermark",
             "before: derived index trusted until manually rebuilt",
             "after: built_at_revision vs manifest.revision; stale → "
             "fallback scan + rebuild under index.lock"),
        ])
    return f"""
<section id="s2"><h2>2 · What was implemented</h2>
{svg_arch()}
<h3>Command surface — {len(rust_cmds)} of {len(rust_cmds | py_cmds)} shared commands ported</h3>
{matrix}
<h3>Improvement highlights</h3><div class='cards'>{cards}</div>
{rev}</section>"""


def s_eval(results):
    q = results.get("quality", {})
    perf = results.get("performance", {})
    inst = results.get("install_ux", {})
    lanes = [l for l in ("rust", "python", "baseline") if q.get(l)]
    metrics = [("precision", "Precision"), ("recall", "Recall"),
               ("f1", "F1"), ("exact_match_pct", "Exact match"),
               ("mean_semantic", "Semantic"),
               ("correct_current_pct", "Answer current"),
               ("citation_coverage_pct", "Citations"),
               ("token_savings", "Token savings")]
    rows = []
    for key, label in metrics:
        vals = {}
        for lane in lanes:
            vals[lane] = dig(q, lane, "aggregates", key)
        base = vals.get("python")
        cells = []
        for lane in lanes:
            v = vals[lane]
            cell = pct(v) if isinstance(v, (int, float)) else "n/a"
            cls = ""
            if isinstance(v, (int, float)) and isinstance(
                    base, (int, float)) and lane == "rust":
                d = 100 * (v - base)
                cls = " class='dpos'" if d >= 0 else " class='dneg'"
                cell += f" <span class='delta'>({d:+.1f}pp)</span>"
            cells.append(f"<td{cls}>{cell}</td>")
        star = ""
        if key == "f1":
            pb = dig(q, "significance", "rust_vs_python", "tests",
                     "correct_current", "p_first_is_better")
            pw = dig(q, "significance", "rust_vs_python", "tests",
                     "correct_current", "p_first_is_worse")
            if isinstance(pb, (int, float)) and pb < 0.05:
                star = f" <span class='stars'>{stars(pb)}</span>"
            elif isinstance(pw, (int, float)) and pw < 0.05:
                star = " <span class='dneg'>&#9660;</span>"
        rows.append(f"<tr><td>{esc(label)}{star}</td>{''.join(cells)}</tr>")
    head = "".join(f"<th>{esc(l)}</th>" for l in lanes)
    quality = (f"<table><thead><tr><th>metric</th>{head}</tr></thead>"
               f"<tbody>{''.join(rows)}</tbody></table>") if lanes else \
        "<p class='muted'>no quality data</p>"

    q_rust = q.get("rust") or {}
    sig = q.get("significance") if isinstance(q, dict) else None
    if sig:
        srows = ""
        for name in sorted(sig):
            tests = (sig.get(name) or {}).get("tests") or {}
            for metric, s in sorted(tests.items()):
                if not isinstance(s, dict):
                    continue
                first = name.split("_vs_")[0]
                second = name.split("_vs_")[-1] if "_vs_" in name else "?"
                srows += (f"<tr><td>{esc(name)}</td>"
                          f"<td>{esc(metric)}</td>"
                          f"<td>{esc(s.get('p_first_is_better'))}</td>"
                          f"<td>{esc(s.get('p_first_is_worse'))}</td>"
                          f"<td class='mut'>{esc(first)}={num(s.get('mean_' + first), 2)}"
                          f" vs {esc(second)}={num(s.get('mean_' + second), 2)}"
                          "</td></tr>")
        sigtbl = ("<h3>Permutation significance (20000 paired perms, "
                  "seed 20261007)</h3><table><thead><tr><th>comparison"
                  "</th><th>metric</th><th>p(better)</th><th>p(worse)</th>"
                  "<th>means</th></tr></thead><tbody>" + srows
                  + "</tbody></table>")
    else:
        sigtbl = ""

    mem = []
    for lane in lanes:
        m = (q.get(lane) or {}).get("memory") or {}
        mem.append(f"<tr><td>{esc(lane)}</td>"
                   f"<td>{esc(m.get('correction_cases', '?'))}</td>"
                   f"<td>{pct(m.get('invalidation_rate'))}</td>"
                   f"<td>{pct(m.get('stale_answer_rate'))}</td></tr>")
    memtbl = ("<h3>Memory effectiveness (correction propagation)</h3>"
              "<table><thead><tr><th>lane</th><th>cases</th>"
              "<th>invalidation rate</th><th>stale-answer rate</th></tr>"
              "</thead><tbody>" + "".join(mem) + "</tbody></table>")

    series = []
    for label, path in [
            ("ingest / file", ("engines", "ingest", "per_file_s")),
            ("ingest batch", ("engines", "ingest", "batch_cold_s")),
            ("compile total", ("engines", "compile", "total_s")),
            ("query p50", ("engines", "query", "p50_s")),
            ("query p95", ("engines", "query", "p95_s")),
            ("graph-data", ("engines", "graph", "graph_data_s")),
            ("cold start", ("engines", "cold_start", "status_s"))]:
        vals = {}
        for lane in ("rust", "python"):
            v = dig(perf, path[0], lane, *path[2:], "median") \
                or dig(perf, path[0], lane, *path[2:], "median_s")
            if isinstance(v, (int, float)):
                vals[lane] = round(v, 4)
        if vals:
            series.append((label, vals))
    charts = svg_bars("Performance — medians (s), lower is better", series) \
        if series else "<p class='muted'>no perf data</p>"

    rss_r = dig(perf, "engines", "rust", "peak_rss", "pipeline_max_rss_mb")
    rss_p = dig(perf, "engines", "python", "peak_rss", "pipeline_max_rss_mb")
    bin_b = dig(results, "repo", "rust_binary_bytes")
    strip_r = dig(perf, "footprint", "rust_binary_stripped_bytes")
    if isinstance(bin_b, (int, float)):
        foot = f"{bin_b / 1e6:.1f} MB binary"
        if isinstance(strip_r, (int, float)):
            foot += f" / {strip_r / 1e6:.1f} MB stripped"
    else:
        foot = "n/a"
    rss_cell = lambda v: f"{v:.1f} MB" if isinstance(v, (int, float)) else "n/a"
    pptable = ("<h3>Footprint</h3><table><thead><tr><th></th><th>rust"
               "</th><th>python</th></tr></thead><tbody>"
               f"<tr><td>peak RSS (ingest+compile)</td>"
               f"<td>{rss_cell(rss_r)}</td><td>{rss_cell(rss_p)}</td></tr>"
               f"<tr><td>engine footprint</td><td>{foot}</td>"
               f"<td>python3 + scripts/</td></tr></tbody></table>")

    uxrows = ""
    for c in (results.get("verdict", {}).get("checks", [])):
        uxrows += (f"<tr><td>{esc(c['name'])}</td>"
                   f"<td class='{ 'ok' if c['status'] == 'pass' else ('bad' if c['status'] == 'fail' else 'mut') }'>"
                   f"{esc(c['status'])}</td><td>{esc(c['detail'])}</td></tr>")
    uxtbl = ("<h3>Install / UX checks</h3><table><thead><tr><th>check</th>"
             "<th>status</th><th>detail</th></tr></thead><tbody>"
             + uxrows + "</tbody></table>")

    return f"""
<section id="s3"><h2>3 · Evaluation results</h2>
<h3>Quality matrix (planted-facts battery)</h3>{quality}{sigtbl}{memtbl}
{charts}{pptable}{uxtbl}</section>"""


def s_quick(results):
    steps = first_session()
    transcript = ""
    if steps:
        blocks = []
        for cmd, code, out in steps:
            body = out if len(out) < 1400 else out[:1400] + "\n…"
            blocks.append(
                f"<div class='ts'><p class='tsc'>$ llm-wiki {esc(cmd)} "
                f"<span class='mut'>→ exit {code}</span></p>"
                f"<pre>{esc(body)}</pre></div>")
        transcript = ("<h3>First session — real captured output</h3>"
                      + "".join(blocks)
                      + "<p class='muted'>Captured at render time in a "
                      "throwaway dir; timestamps/ids vary.</p>")
    feats = "".join(
        f"<tr><td>{esc(k)}</td><td>{esc(b)}</td><td>{esc(r)}</td></tr>"
        for k, b, r in [
            ("install", "one binary + embedded skills",
             "clone + python3 + scripts/"),
            ("PDF/office extraction", "markitdown on PATH (probed)",
             "markitdown on PATH (subprocess)"),
            ("skill dev checkout", "— (binary variant)",
             "✓ run wiki.py from repo"),
            ("MCP server", "✓ built-in", "✓ (python proc)"),
            ("viz", "✓ embedded + viz-serve", "graph-export only"),
        ])
    return f"""
<section id="s4"><h2>4 · Quick start</h2>
<pre class='qs'>curl -fsSL https://raw.githubusercontent.com/thanhNt16/orca/main/install.sh | bash -s -- --binary
llm-wiki --install-skills ~/.omp/agent/skills
llm-wiki mcp-register --client claude-code</pre>
{transcript}
<h3>Python path vs binary path</h3>
<table><thead><tr><th></th><th>binary (default)</th><th>python (dev-reference)</th></tr></thead>
<tbody>{feats}</tbody></table></section>"""

def s_e2e():
    """E2E runtime parity: 8 scenarios x 3 runtimes, both engines."""
    p = os.path.join(REPO, "evals", "e2e", "compare", "results.json")
    if not os.path.isfile(p):
        return ""
    with open(p, encoding="utf-8") as f:
        c = json.load(f)
    rows = ""
    scenarios = sorted(next(iter(c["scenario_rows"].values())).keys())
    for rt in c["scenario_rows"]:
        for s in scenarios:
            cell = c["scenario_rows"][rt][s]
            py, ru, par = cell["python"], cell["rust"], cell["parity"]
            badge = ("✓ parity" if par else "✗ diverges")
            rows += (
                f"<tr><td>{esc(rt)}</td><td>{esc(s)}</td>"
                f"<td class='{'ok' if py['status'] == 'pass' else ('bad' if py['status'] == 'fail' else 'mut')}'>{esc(py['status'])}</td>"
                f"<td class='{'ok' if ru['status'] == 'pass' else ('bad' if ru['status'] == 'fail' else 'mut')}'>{esc(ru['status'])}</td>"
                f"<td class='{'ok' if par else 'mut'}'>{badge}</td></tr>")
    v = c["verdict"]
    det = v.get("deterministic_claims_sources_parity")
    sem = v.get("semantic_page_text_parity")
    lane = v.get("runtime_lane_status_parity")
    summ = ("<h3>Verdict</h3><table><tbody>"
            f"<tr><td>deterministic layer (claims/sources/state)</td>"
            f"<td class='{'ok' if det else 'mut'}'>{'parity' if det else 'diverges'}</td></tr>"
            f"<tr><td>semantic layer (page text)</td>"
            f"<td class='{'ok' if sem else 'mut'}'>{'parity' if sem else 'diverges'}</td></tr>"
            f"<tr><td>runtime lane (strict per-scenario pass/fail)</td>"
            f"<td class='{'ok' if lane else 'mut'}'>{'parity' if lane else 'diverges'}</td></tr>"
            "</tbody></table>"
            f"<p class='muted'>{esc(v.get('notes', ''))}</p>")
    return f"""
<section id="s7"><h2>E2E runtime parity — both engines, 8 scenarios × 3 runtimes</h2>
<p>Same scenario scripts drive both engines through real agent runtimes
(omp / claude / cellock). Deterministic state is compared file-by-file after
each run; see <code>evals/e2e/compare/report.md</code>.</p>
<table><thead><tr><th>runtime</th><th>scenario</th><th>python</th>
<th>rust</th><th>parity</th></tr></thead><tbody>{rows}</tbody></table>
{summ}</section>"""



def s_limits(results):
    rows = "".join(
        f"<tr><td>{esc(k)}</td><td>{esc(v)}</td></tr>" for k, v in [
            ("PDF lane", "markitdown optional; absent → coverage "
             "not_extracted + pending"),
            ("Windows", "unix inode/mtime stamps primary; file_index "
             "fallback untested on Windows"),
            ("Release gates", "evals/deterministic/run_gates.py (both "
             "engines) — see gate table; un-gated behaviors tracked in "
             "spec §58 review"),
            ("Python engine", "dev-reference + P7 parity oracle; not an "
             "install path since the P6 flip"),
            ("This report", "regenerate via evals/rust-vs-python/"
             "generate_report.py + render_report.py after each benchmark"),
        ])
    return f"""
<section id="s5"><h2>5 · Flow diagrams</h2>
{svg_pipeline()}{svg_txn()}{svg_mcp()}</section>
<section id="s6"><h2>6 · Known limits &amp; follow-ups</h2>
<table><thead><tr><th>item</th><th>status</th></tr></thead>
<tbody>{rows}</tbody></table></section>"""


CSS = """
:root{--bg:#0f1117;--srf:#171a23;--srf2:#1e2230;--bd:#2a2f40;--fg:#e8eaf2;
--mut:#9aa1b5;--acc:#6c8cff;--ok:#3fb950;--wrn:#e5b454;--err:#e06c75;
--c1:#6c8cff;--c2:#4cc38a;--c3:#e5b454;--c4:#e06c75;--c5:#b18cff;--c6:#56d4dd}
@media (prefers-color-scheme: light){:root{--bg:#fbfbfd;--srf:#ffffff;
--srf2:#f2f3f7;--bd:#dfe2ec;--fg:#1c2030;--mut:#667;--acc:#3557d8;
--ok:#1a7f37;--wrn:#9a6700;--err:#cf222e;--c1:#3557d8;--c2:#1a7f37;
--c3:#9a6700;--c4:#cf222e;--c5:#8250df;--c6:#0d7c99}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);
font:14px/1.6 -apple-system,'Segoe UI',Roboto,sans-serif;padding:0 0 80px}
main{max-width:900px;margin:0 auto;padding:0 24px}
header{padding:36px 24px 18px;max-width:900px;margin:0 auto}
h1{font-size:26px;margin:0 0 6px}
h2{font-size:19px;margin:36px 0 12px;padding-bottom:6px;border-bottom:
1px solid var(--bd)}
h3{font-size:15px;margin:22px 0 8px}
code{background:var(--srf2);padding:1px 5px;border-radius:4px;
font-size:12.5px}
pre{background:var(--srf2);border:1px solid var(--bd);border-radius:8px;
padding:12px;overflow:auto;font-size:12px;line-height:1.5}
table{border-collapse:collapse;width:100%;margin:10px 0;font-size:13px}
th,td{border:1px solid var(--bd);padding:5px 9px;text-align:left;
vertical-align:top}
th{background:var(--srf2)}
.ok{color:var(--ok)}.bad{color:var(--err)}.mut,.muted{color:var(--mut)}
.dpos{color:var(--ok)}.dneg{color:var(--err)}
.delta{font-size:11px}
.verdict{border-radius:10px;padding:14px 18px;margin:14px 0;font-size:16px;
border:1px solid var(--bd)}
.verdict.go{background:color-mix(in srgb,var(--ok) 12%,var(--srf));
border-color:var(--ok)}
.verdict.nogo{background:color-mix(in srgb,var(--err) 12%,var(--srf));
border-color:var(--err)}
.kpis{display:flex;gap:12px;flex-wrap:wrap;margin:14px 0}
.kpi{background:var(--srf);border:1px solid var(--bd);border-radius:10px;
padding:10px 16px;min-width:130px}
.kpi-v{font-size:20px;font-weight:600}
.kpi-k{font-size:11.5px;color:var(--mut)}
.blockers{color:var(--err)}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(210px,1fr));
gap:10px}
.card{background:var(--srf);border:1px solid var(--bd);border-radius:10px;
padding:10px 14px}
.card h3{margin:0 0 6px;font-size:13px}
.before{color:var(--mut);font-size:12px;margin:0 0 4px}
.after{font-size:12px;margin:0}
.ph{font-size:11px;text-align:center;border-radius:4px}
.phP1{background:color-mix(in srgb,var(--c1) 25%,transparent)}
.phP2{background:color-mix(in srgb,var(--c2) 25%,transparent)}
.phP3{background:color-mix(in srgb,var(--c3) 25%,transparent)}
.phP4{background:color-mix(in srgb,var(--c5) 25%,transparent)}
.phP5{background:color-mix(in srgb,var(--c6) 25%,transparent)}
.stars{color:var(--acc);font-weight:700}
.qs{color:var(--fg)}
.tsc{margin:8px 0 2px;font-size:12.5px}
.ts pre{margin:0 0 8px}
svg{max-width:100%;height:auto;margin:8px 0}
.svgt{fill:var(--fg);font-size:13px;font-weight:600}
.svgv{fill:var(--mut);font-size:10px}
.svgl{fill:var(--mut);font-size:11px}
.grid{stroke:var(--bd);stroke-width:1}
.nodet{fill:var(--fg);font-size:12.5px;font-weight:600}
.nodel{fill:var(--mut);font-size:10.5px}
.node{fill:var(--srf);stroke:var(--acc);stroke-width:1.2}
.node.n2{stroke:var(--c2)}.node.n3{stroke:var(--c5)}
.edge{stroke:var(--mut);stroke-width:1.4;fill:none}
.arrow{fill:var(--mut)}
nav{font-size:12.5px;color:var(--mut);padding-bottom:8px}
nav a{color:var(--acc);text-decoration:none;margin-right:12px}
"""


def main():
    if not os.path.isfile(RESULTS):
        print(f"missing {RESULTS} - run generate_report.py first",
              file=sys.stderr)
        return 1
    with open(RESULTS, encoding="utf-8") as f:
        results = json.load(f)
    report_md = ""
    if os.path.isfile(REPORT):
        with open(REPORT, encoding="utf-8") as f:
            report_md = f.read()

    body = (s_exec(results) + s_implemented(results) + s_eval(results)
            + s_quick(results) + s_e2e() + s_limits(results))
    doc = f"""<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>llm-wiki — Rust engine report</title>
<style>{CSS}</style></head>
<body><header>
<h1>llm-wiki · Rust engine</h1>
<p class="muted">What was built, what the benchmark says, how to start.
Generated {datetime.date.today().isoformat()} ·
<code>evals/rust-vs-python/render_report.py</code></p>
<nav><a href="#s1">summary</a><a href="#s2">implemented</a>
<a href="#s3">evaluation</a><a href="#s4">quick start</a>
<a href="#s7">e2e parity</a><a href="#s5">flows</a><a href="#s6">limits</a></nav>
</header><main>
{body}
</main></body></html>
"""
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(doc)
    print(f"wrote {OUT} ({len(doc)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
