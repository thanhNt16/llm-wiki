"""Deterministic OKF v0.2 export (SP2a #10).

Model-free and byte-stable for a given store: every timestamp comes from
claim objects (max recorded_at/updated_at), never the wall clock. Layout
mirrors the bundles openwiki's visualize consumed in the SP benchmarks
(root index.md declaring okf_version, concepts/ pages with type: concept
front matter, .claims/ provenance sidecars, relative internal links).

Front matter is hand-emitted (double-quoted okf_version, nested generated
block): yamlite.dumps cannot express nesting, and derived OKF pages target
openwiki's fixture shape, not yamlite's scalar style.
"""
import json
import os
import re
import shutil
import tempfile

from . import claims
from .transaction import TxnError
from .yamlite import _dump_scalar

_LINK_RE = re.compile(r"\]\(([^)#]+\.md)\)")


def _slug(subject: str) -> str:
    return (subject.strip().lower().replace("/", "-").replace(".", "-")
            .replace(" ", "-"))


def _source_ids(cs) -> list:
    out = set()
    for c in cs:
        for e in c.get("evidence") or []:
            sid = e.get("source_id") if isinstance(e, dict) else str(e)
            if sid:
                out.add(sid)
    return sorted(out)


def _generated_at(all_claims) -> str:
    # manifest carries no timestamp (revision/skill_version/content_hash
    # only), so the export clock = latest claim write, content-derived
    ts = [c.get("updated_at") or c.get("recorded_at") for c in all_claims]
    ts = [t for t in ts if t]
    return max(ts) if ts else "1970-01-01T00:00:00Z"


def _front_matter(pairs) -> str:
    """Emit flat scalars, scalar lists, and one-level nested mappings."""
    lines = []
    for key, value in pairs:
        if isinstance(value, dict):
            lines.append("%s:" % key)
            for k, v in value.items():
                lines.append("  %s: %s" % (k, v))
        elif isinstance(value, list):
            if not value:
                lines.append("%s: []" % key)
            else:
                lines.append("%s:" % key)
                for item in value:
                    lines.append("  - %s" % item)
        elif isinstance(value, str):
            # '"0.2"'-style values pass through verbatim (fixture shape);
            # everything else reuses yamlite's quoting rules
            lines.append("%s: %s" % (key, value if value.startswith('"')
                                     else _dump_scalar(value)))
        else:
            lines.append("%s: %s" % (key, value))
    return "\n".join(lines)


def _subject_of(by_subject, cid):
    for subject, cs in by_subject.items():
        if any(c["id"] == cid for c in cs):
            return subject
    return None


def export(wiki, out_dir: str) -> dict:
    all_claims = claims.list_claims(wiki)
    if not all_claims:
        raise TxnError("wiki has no claims: nothing to export")
    tmp = tempfile.mkdtemp(prefix="okf-export-")
    try:
        os.makedirs(os.path.join(tmp, "concepts"))
        os.makedirs(os.path.join(tmp, ".claims"))
        by_subject = {}
        for c in all_claims:
            by_subject.setdefault(c["subject"], []).append(c)
        slugs = {_slug(s) for s in by_subject}
        generated = {"by": "llm-wiki", "at": _generated_at(all_claims)}
        links = 0
        index_links = []
        for subject in sorted(by_subject):
            slug = _slug(subject)
            cs = sorted(by_subject[subject], key=lambda c: c["id"])
            body = ["# %s" % subject, ""]
            for c in cs:
                body.append("- `%s` = **%s** — status: %s, version: %s"
                            % (c["predicate"], c.get("value"), c["status"],
                               c.get("version", 1)))
            for c in cs:  # supersession cross-links between exported concepts
                for ref in (c.get("superseded_by"), *(c.get("supersedes") or [])):
                    if not ref:
                        continue
                    target = ref
                    if ref.startswith("claim_"):
                        subj = _subject_of(by_subject, ref)
                        if not subj:
                            continue
                        target = subj
                    tgt = _slug(target)
                    if tgt in slugs and tgt != slug:
                        body.append("- supersedes chain: [%s](%s.md)" % (tgt, tgt))
                        links += 1
            front = _front_matter([("type", "concept"), ("title", subject),
                                   ("generated", generated),
                                   ("sources", _source_ids(cs))])
            with open(os.path.join(tmp, "concepts", slug + ".md"), "w") as f:
                f.write("---\n%s\n---\n\n%s\n" % (front, "\n".join(body)))
            with open(os.path.join(tmp, ".claims", slug + ".json"), "w") as f:
                json.dump([{"id": c["id"], "predicate": c["predicate"],
                            "value": c.get("value"), "status": c["status"],
                            "sources": _source_ids([c]),
                            "valid_from": c.get("valid_from"),
                            "valid_to": c.get("valid_to")}
                           for c in cs], f, indent=2)
                f.write("\n")
            index_links.append("- [%s](concepts/%s.md)" % (subject, slug))
        root_front = _front_matter([
            ("okf_version", '"0.2"'), ("type", "index"), ("generated", generated)])
        with open(os.path.join(tmp, "index.md"), "w") as f:
            f.write("---\n%s\n---\n\n# Wiki index\n\n%s\n"
                    % (root_front, "\n".join(sorted(index_links))))
        # link-validation pass: an orphan link means the export lied
        for fn in sorted(os.listdir(os.path.join(tmp, "concepts"))):
            text = open(os.path.join(tmp, "concepts", fn)).read()
            for m in _LINK_RE.findall(text):
                tgt = os.path.normpath(os.path.join(tmp, "concepts", m))
                if not os.path.isfile(tgt):
                    raise TxnError("orphan link in %s: %s" % (fn, m))
        # atomic replace: previous bundle moves aside, new one renames in;
        # a failed rename restores the old bundle — no data loss
        stale = out_dir + ".stale-%d" % os.getpid()
        had_old = os.path.exists(out_dir)
        if had_old:
            os.rename(out_dir, stale)
        try:
            os.rename(tmp, out_dir)
        except Exception:
            if had_old:
                os.rename(stale, out_dir)
            raise
        if had_old:
            shutil.rmtree(stale, ignore_errors=True)
        return {"pages": len(by_subject), "links": links,
                "bytes": sum(os.path.getsize(os.path.join(r, f))
                             for r, _, fs in os.walk(out_dir) for f in fs)}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
