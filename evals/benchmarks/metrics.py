"""Metric collection — reads engine CLI JSON + the built wiki dir.
Runs identically against any engine revision (base or HEAD): every function
drives the engine CLI or the filesystem, never engine internals."""
import glob
import json
import os
import re

# Import both ways so the module works package-style
# (`evals.benchmarks.metrics`) and with evals/benchmarks on sys.path.
try:
    from .driver import Engine
    from .fixture import GOLD_QUESTIONS
except ImportError:
    from driver import Engine
    from fixture import GOLD_QUESTIONS

SECTIONS = ("concepts", "entities", "procedures", "questions",
            "sources", "changes")


def _read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def _triples(matches):
    return {(m.get("subject"), m.get("predicate"), m.get("value"))
            for m in matches}


def retrieval(engine: Engine, wiki_root: str) -> dict:
    hits, gold_total, rr_sum, refusals_ok = 0, 0, 0.0, 0
    for q in GOLD_QUESTIONS:
        args = ["query-prepare", "--question", q["question"]]
        if q.get("as_of"):
            args += ["--as-of", q["as_of"]]
        code, out, _ = engine.json(args, wiki_root)
        matches = (out or {}).get("matches", [])[:5]
        if not q["expect"]:
            refusals_ok += 1 if not matches else 0
            continue
        gold = {(e["subject"], e["predicate"], e["value"]) for e in q["expect"]}
        gold_total += len(gold)
        top = _triples(matches)
        hits += len(gold & top)
        for rank, m in enumerate(matches, 1):
            if (m.get("subject"), m.get("predicate"), m.get("value")) in gold:
                rr_sum += 1.0 / rank
                break
    n = sum(1 for q in GOLD_QUESTIONS if q["expect"])
    return {"precision_at_5": None if not n else round(hits / (5 * n), 3),
            "gold_recall_at_5": None if not gold_total
            else round(hits / gold_total, 3),
            "mrr": None if not n else round(rr_sum / n, 3),
            "refusals_correct": refusals_ok ==
            sum(1 for q in GOLD_QUESTIONS if not q["expect"])}


def context(engine: Engine, wiki_root: str, corpus_dir: str) -> dict:
    """context-pack receipts for the first 4 gold questions: average pack
    size, budget compliance, and corpus-to-pack compression ratio."""
    reqs, ests, compliant = 0, 0, 0
    for q in GOLD_QUESTIONS[:4]:
        code, out, _ = engine.json(
            ["context-pack", "--task", q["question"], "--budget", "6000"],
            wiki_root)
        if code != 0 or not out:
            continue
        receipt = out.get("receipt", {})
        est = receipt.get("budget", {}).get("estimated")
        req = receipt.get("budget", {}).get("requested", 6000)
        if est is None:
            continue
        reqs += 1
        ests += est
        compliant += 1 if est <= req else 0
    corpus_tokens = 0
    for path in glob.glob(os.path.join(corpus_dir, "**", "*"), recursive=True):
        if os.path.isfile(path) and path.endswith((".md", ".json", ".csv")):
            corpus_tokens += len(_read(path).split())
    avg = (ests / reqs) if reqs else 0
    return {"pack_tokens_avg": round(avg, 1),
            "budget_compliance": None if not reqs else compliant == reqs,
            "compression_ratio": None if not avg
            else round(corpus_tokens / avg, 2)}


def _page_files(wiki_root):
    out = []
    for sec in SECTIONS:
        d = os.path.join(wiki_root, ".llm-wiki", "wiki", sec)
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if name.endswith(".md") and name != "index.md":
                out.append((sec, name[:-3]))
    return out


def topology(wiki_root: str) -> dict:
    """Filesystem-only structural metrics over the built wiki dir:
    page count, orphans (not listed in their section index), dead ends (no
    claim/md outbound link), section indexes linked from overview.md, and
    harness-side near-duplicate title pairs (engine-independent count)."""
    wiki = os.path.join(wiki_root, ".llm-wiki", "wiki")
    pages = _page_files(wiki_root)
    orphans, dead_ends, indexes_linked = 0, 0, 0
    titles = {}
    for sec, slug in pages:
        listed = False
        idx = os.path.join(wiki, sec, "index.md")
        if os.path.isfile(idx):
            listed = ("%s.md" % slug) in _read(idx)
        orphans += 0 if listed else 1
        text = _read(os.path.join(wiki, sec, slug + ".md"))
        has_out = re.search(r"\[\[[a-z]+_[0-9A-Za-z]+\]\]", text) or \
            re.search(r"\]\([^)]+\.md\)", text)
        dead_ends += 0 if has_out else 1
        m = re.search(r"^title:\s*(.+)$", text, re.M)
        titles[(sec, slug)] = m.group(1).strip() if m else slug

    overview = os.path.join(wiki, "overview.md")
    ov = _read(overview) if os.path.isfile(overview) else ""
    for sec in SECTIONS:
        if os.path.isfile(os.path.join(wiki, sec, "index.md")):
            indexes_linked += 1 if ("%s/index.md" % sec) in ov else 0

    def toks(t):
        ws = {"the", "a", "an", "of", "and", "or", "to", "for", "in", "backup"}
        return {w for w in re.split(r"[^0-9a-z]+", t.lower())
                if w and w not in ws}

    keys = list(titles)
    near_pairs = 0
    for i in range(len(keys)):
        for j in range(i + 1, len(keys)):
            a, b = toks(titles[keys[i]]), toks(titles[keys[j]])
            if a and b and len(a & b) / len(a | b) >= 0.6:
                near_pairs += 1
    return {"pages": len(pages), "orphan_pages": orphans,
            "dead_end_pages": dead_ends,
            "section_indexes_linked": indexes_linked,
            "near_dup_pairs": near_pairs}


def twosource(engine: Engine, wiki_root: str) -> dict:
    """Two-source rule visibility: page-targets corroborated targets vs
    subjects still waiting for a second independent origin."""
    code, out, _ = engine.json(["page-targets"], wiki_root)
    data = out or {}
    return {"corroborated": len(data.get("targets", [])),
            "waiting": len(data.get("waiting", []))}


def hygiene_and_guard(engine: Engine, wiki_root: str) -> dict:
    """doctor findings grouped by code + whether the injected stray page is
    flagged as an orphan (topology lint present) or slipped through."""
    code, out, _ = engine.json(["doctor"], wiki_root)
    findings = (out or {}).get("findings", [])
    by_code = {}
    for f in findings:
        by_code[f["code"]] = by_code.get(f["code"], 0) + 1
    return {"doctor_findings_by_code": by_code,
            "stray_orphan_detected_by_doctor": by_code.get("orphan_page", 0) > 0}
