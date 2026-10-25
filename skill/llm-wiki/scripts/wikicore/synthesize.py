"""Claims -> prose synthesis: target selection and gated page writes.

The agent authors prose; this module only selects eligible subjects, validates
the proposed page against the dependency graph, and commits atomically.
No prose is invented here — deterministic selection, validation, and commit only.
"""
import hashlib
import json
import os
import re
import time
from typing import Optional

from . import claims, deps, topics
from .text import norm_tokens
from .transaction import Transaction, TxnError
from .yamlite import dumps as yamldumps, loads as yamlloads

PAGE_KINDS = ("concepts", "entities", "procedures", "questions", "sources",
              "changes")
KIND_TYPE = {
    "concepts": "concept",
    "entities": "entity",
    "procedures": "procedure",
    "questions": "question",
    "sources": "source",
    "changes": "change",
}
ELIGIBLE_STATUSES = ("accepted", "provisional")
AUTHORITATIVE = "explicit_project_decision"
# Near-duplicate title/subject token-Jaccard thresholds: write_page REFUSES a
# NEW page at/above NEAR_DUP_WRITE_THRESHOLD (unless --allow-new); doctor's
# lint flags an existing pair at/above the stricter NEAR_DUP_LINT_THRESHOLD.
NEAR_DUP_WRITE_THRESHOLD = 0.60
NEAR_DUP_LINT_THRESHOLD = 0.70

LINK_RE = re.compile(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]")
PAGE_LINK_RE = re.compile(
    r"\[\[wiki:([a-z]+)/([a-z0-9][a-z0-9\-]*)\]\]")
_MD_LINK_RE = re.compile(r"\[[^\]]*\]\(([^)\s]+\.md)\)")
_FRONT_RE = re.compile(r"\A---\s*\n(.*?)^---[ \t]*\n?", re.S | re.M)

KIND_RULES_FILE = "page-kinds.json"


def _load_kind_rules(wiki, warnings: list):
    """Ordered kind-routing rules from .llm-wiki/page-kinds.json.

    Shape: {"rules": [{"kind": "entities", "subject"|"prefix"|"suffix"|
    "regex"|"predicate": <str>, "when": {"min_claims": n}?}...],
    "default": "concepts"}. First match wins; --kind overrides all.
    Extend by appending a rule — no code changes.

    Returns (rules, default, heuristics_active): heuristics run per subject
    AFTER file rules — a matching file rule wins, a non-matching one falls
    through to HEURISTIC_RULES (see _route_kind). Always True; kept in the
    signature so callers/tests can pin it."""
    path = wiki.p(KIND_RULES_FILE)
    if not os.path.isfile(path):
        return [], "concepts", True
    try:
        with open(path) as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        warnings.append("%s ignored: %s" % (KIND_RULES_FILE, e))
        return [], "concepts", True
    if not isinstance(data, dict):
        warnings.append("%s ignored: expected an object" % KIND_RULES_FILE)
        return [], "concepts", True
    default = data.get("default", "concepts")
    if default not in PAGE_KINDS:
        warnings.append("%s: default %r not a page kind — using 'concepts'"
                        % (KIND_RULES_FILE, default))
        default = "concepts"
    raw_rules = data.get("rules")
    if raw_rules is not None and not isinstance(raw_rules, list):
        warnings.append("%s ignored: 'rules' must be a list" % KIND_RULES_FILE)
        raw_rules = []
    rules = []
    for i, r in enumerate(raw_rules or []):
        if not isinstance(r, dict) or r.get("kind") not in PAGE_KINDS:
            warnings.append("%s rule %d ignored: missing/invalid 'kind'"
                            % (KIND_RULES_FILE, i))
            continue
        matchers = [k for k in ("subject", "prefix", "suffix", "regex",
                                "predicate") if r.get(k) is not None]
        if not matchers:
            warnings.append("%s rule %d ignored: no matcher key"
                            % (KIND_RULES_FILE, i))
            continue
        rules.append((i, r, matchers))
    return rules, default, True


def _rule_matches(rule: dict, matchers: list, subject: str,
                  clist: list) -> bool:
    """All supplied matcher keys must match (AND)."""
    for m in matchers:
        v = rule[m]
        if m == "subject":
            if subject != v:
                return False
        elif m == "prefix":
            if not subject.startswith(v):
                return False
        elif m == "suffix":
            if not subject.endswith(v):
                return False
        elif m == "regex":
            try:
                if not re.search(v, subject):
                    return False
            except re.error:
                return False
        elif m == "predicate":
            try:
                if not any(re.search(v, str(c.get("predicate", "")))
                           for c in clist):
                    return False
            except re.error:
                return False
    when = rule.get("when") or {}
    if "min_claims" in when and len(clist) < int(when["min_claims"]):
        return False
    return True


# Built-in kind-routing heuristics, evaluated in order when page-kinds.json
# gives no usable file rules (absent/unparseable/empty). First hit wins;
# rule 5 is "else -> concepts" handled by _route_kind's default return.
HEURISTIC_RULES = (
    # 1. procedural claims -> procedures
    ("procedures", ("predicate",),
     re.compile(r"^(step|procedure|runbook|how[ _-]?to)")),
    # 2. ticket ids (stc-97) and dotted component names (x.api/.service/.ui/.db/.cli) -> entities
    ("entities", ("subject",),
     re.compile(r"^[a-z]+-\d+$|\.(api|service|ui|db|cli)$")),
    # 3. single-token code identifier (snake/kebab/camel, no spaces)
    #    with >=2 claims -> entities  (min_claims checked separately)
    ("entities", ("subject",),
     re.compile(r"[a-z0-9_]+(?:[A-Z][a-z0-9]*)*|[a-z0-9][a-z0-9_\-]*")),
    # 4. open-question markers on subject or predicate -> questions
    ("questions", ("subject", "predicate"),
     re.compile(r"open[ _-]?question|unresolved|tbd")),
)


def _heuristic_kind(subject: str, clist: list):
    """Evaluate HEURISTIC_RULES 1-4 in order.

    Returns (kind, "heuristic:N") on a match, (None, None) otherwise."""
    # 1. any claim predicate looks procedural
    if any(HEURISTIC_RULES[0][2].search(str(c.get("predicate") or ""))
           for c in clist):
        return "procedures", "heuristic:1"
    # 2. ticket-id or dotted component subject
    if HEURISTIC_RULES[1][2].search(subject):
        return "entities", "heuristic:2"
    # 3. code-identifier subject with corroborating claims
    if len(clist) >= 2 and HEURISTIC_RULES[2][2].fullmatch(subject):
        return "entities", "heuristic:3"
    # 4. open-question marker in subject or any predicate
    q = HEURISTIC_RULES[3][2]
    if q.search(subject) or any(q.search(str(c.get("predicate") or ""))
                                for c in clist):
        return "questions", "heuristic:4"
    return None, None


def _route_kind(subject: str, clist: list, rules: list,
                default: str, heuristics_active: bool):
    """File rules first, then built-in heuristics (when active), then the
    default. Returns (kind, matched_rule) where matched_rule is a rule
    index int, "heuristic:N", or None for the default."""
    for i, rule, matchers in rules:
        if _rule_matches(rule, matchers, subject, clist):
            return rule["kind"], i
    if heuristics_active:
        kind, matched = _heuristic_kind(subject, clist)
        if kind is not None:
            return kind, matched
    return default, None



def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


from .text import slugify  # noqa: F401  (canonical home: text.py)

def extract_page_links(body) -> list:
    """[[wiki:<kind>/<slug>]] links -> ["<kind>/<slug>", ...], deduped in
    first-seen order (doctor's page-link lint consumes this)."""
    out = []
    for m in PAGE_LINK_RE.finditer(body):
        ref = "%s/%s" % (m.group(1), m.group(2))
        if ref not in out:
            out.append(ref)
    return out


def page_links(body):
    """[[wiki:<kind>/<slug>]] links -> wiki/<kind>/<slug>.md dep paths."""
    return {"wiki/%s.md" % ref for ref in extract_page_links(body)}


_TITLE_STOPWORDS = {"the", "a", "an", "of", "and", "or", "to", "for", "in",
                    "on", "is", "are"}

def _normalize_tokens(text):
    """Title/subject token set for near-duplicate comparison (doctor's
    near-dup lint): lowercase, split on non-alnum, drop short tokens, strip
    one conservative suffix (windows -> window)."""
    return norm_tokens(str(text or ""))


def _norm_title_tokens(text: str) -> set:
    toks = re.split(r"[^0-9a-z]+", text.lower())
    return {t for t in toks if t and t not in _TITLE_STOPWORDS}


def _jaccard(a: set, b: set) -> float:
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def build_token_index(entries: dict) -> dict:
    """Inverted index over key -> token-set entries: token -> sorted list of
    keys containing it. Pairs sharing >=1 token are exactly the
    candidate_pairs — sub-quadratic near-dup enumeration."""
    index = {}
    for key, toks in entries.items():
        for tok in toks:
            index.setdefault(tok, []).append(key)
    for keys in index.values():
        keys.sort()
    return index


def candidate_pairs(index: dict) -> set:
    """All unordered key-pairs sharing >=1 token, each as a sorted
    (a, b) tuple — the lossless superset of every pair whose Jaccard could
    be nonzero."""
    pairs = set()
    for keys in index.values():
        for i, a in enumerate(keys):
            for b in keys[i + 1:]:
                pairs.add((a, b))
    return pairs


def similar_pages(wiki, kind: str, subject: str, title: str,
                  exclude_slug: Optional[str] = None) -> list:
    """Near-duplicate pages for a proposed new page: token-Jaccard >=
    NEAR_DUP_WRITE_THRESHOLD on title/subject vs every existing page's
    title/subject in ALL sections (cross-section twins are the worst kind).
    Lesson: 'extend, don't create' — the default failure mode is near-duplicate
    pages."""
    want = _norm_title_tokens(title) | _norm_title_tokens(subject)
    norm_title = (title or "").strip().lower()
    # Pass 1: collect every existing page (sections then sorted slugs —
    # output order) with its token set; exact-title matches are looked up
    # by normalized title in pass 2. Only pages sharing >=1 want-token (or
    # title-equal) get a Jaccard — zero-overlap pages can never reach the
    # threshold, so the inverted index skips them.
    order, entries, title_map = [], {}, {}
    for sec in PAGE_KINDS:
        sec_dir = wiki.p("wiki", sec)
        if not os.path.isdir(sec_dir):
            continue
        for name in sorted(os.listdir(sec_dir)):
            if not name.endswith(".md") or name == "index.md":
                continue
            slug = name[:-3]
            if exclude_slug is not None and slug == exclude_slug \
                    and sec == kind:
                continue
            path = wiki.p("wiki", sec, name)
            try:
                with open(path, encoding="utf-8") as f:
                    text = f.read()
            except OSError:
                continue
            front, _ = _split_front(text)
            key = "%s/%s" % (sec, slug)
            order.append(key)
            entries[key] = (
                front,
                _norm_title_tokens(str(front.get("title") or ""))
                | _norm_title_tokens(str(front.get("subject") or "")))
            title_map.setdefault(
                str(front.get("title") or "").strip().lower(),
                []).append(key)
    index = build_token_index({k: toks for k, (_, toks) in entries.items()})
    cands = {k for tok in want for k in index.get(tok, ())}
    cands.update(title_map.get(norm_title, ()))
    out = []
    for key in order:
        if key not in cands:
            continue
        sec, slug = key.split("/", 1)
        front, have = entries[key]
        sim = _jaccard(want, have)
        if sim >= NEAR_DUP_WRITE_THRESHOLD or (
                str(front.get("title") or "").strip().lower() == norm_title):
            out.append({"section": sec, "slug": slug,
                        "title": front.get("title") or "",
                        "similarity": round(sim, 3)})
    return out


def _is_reserved(artifact: str) -> bool:
    a = artifact.strip("/")
    return (
        a in ("wiki/index.md", "wiki/overview.md", "wiki/review.md", "wiki/log.md")
        or a.startswith("wiki/decisions/")
        or a.endswith("/index.md")
    )


def _split_front(text: str):
    """Return (frontmatter dict, body) for a markdown page."""
    m = _FRONT_RE.match(text)
    if not m:
        return {}, text
    return yamlloads("---\n%s\n---" % m.group(1)), text[m.end():]


def _detail(c: dict) -> dict:
    return {
        "id": c["id"],
        "version": c["version"],
        "predicate": c["predicate"],
        "value": c.get("value"),
        "scope": c.get("scope") or {},
        "status": c["status"],
        "authority.type": (c.get("authority") or {}).get("type"),
        "valid_from": c.get("valid_from"),
        "valid_to": c.get("valid_to"),
    }




def page_targets(wiki, kind: Optional[str] = None, stale_only: bool = False,
                 min_claims: int = 2, min_sources: int = 2) -> dict:
    """Group eligible claims by subject; report synthesis targets, subjects
    still waiting, and stale derived artifacts under wiki/."""
    if kind is not None and kind not in PAGE_KINDS:
        raise TxnError("unknown page kind: %s (expected one of %s)"
                       % (kind, ", ".join(PAGE_KINDS)))
    groups = {}
    for c in claims.list_claims(wiki):
        if c.get("status") in ELIGIBLE_STATUSES:
            groups.setdefault(c["subject"], []).append(c)
    warnings = []
    rules, default_kind, heuristics_active = _load_kind_rules(wiki, warnings)
    targets = []
    waiting = []
    topic_map = topics.load(wiki)["topics"]
    for subject in sorted(groups):
        cs = groups[subject]
        n = len(cs)
        # distinct origins across the WHOLE claim set (set-union): two claims
        # sharing one origin corroborate once, not twice.
        origins = set()
        for c in cs:
            origins |= _claim_origins(wiki, c)
        origins = len(origins)
        tentry = topic_map.get(subject)
        declared_kind = (topics.PAGE_DIR.get(tentry["kind"])
                         if tentry and tentry.get("status") == "resolved"
                         else None)
        if (tentry and tentry.get("status") == "resolved"
                and tentry["kind"] == "decision"):
            continue  # decision topics never emit pages (receipts/census only)
        if declared_kind is not None:
            reason = "declared"
        elif n >= min_claims and origins >= min_sources:
            reason = "corroborated"
        elif any((c.get("authority") or {}).get("type") == AUTHORITATIVE for c in cs):
            reason = "authoritative"
        elif any(c.get("predicate") == "open_question" for c in cs):
            # open questions are definitionally awaiting humans — exempt from
            # BOTH corroboration thresholds or they starve in waiting[]
            reason = "open_question"
        else:
            waiting.append({
                "subject": subject,
                "claim_count": n,
                "distinct_origins": origins,
                "reason": "below_threshold",
            })
            continue
        if kind is not None:
            k, matched = kind, None
        elif declared_kind is not None:
            k, matched = declared_kind, None
        else:
            k, matched = _route_kind(subject, cs, rules, default_kind,
                                     heuristics_active)
        slug = slugify(subject)
        rel = "wiki/%s/%s.md" % (k, slug)
        prefix = subject.rsplit(".", 1)[0] if "." in subject else subject
        related = []
        for sib, e in topic_map.items():
            if sib == subject or e.get("status") != "resolved":
                continue
            if sib.rsplit(".", 1)[0] == prefix:
                related.append(sib)
        targets.append({
            "subject": subject,
            "matched_rule": matched,
            "kind": k,
            "slug": slug,
            "artifact": rel,
            "claim_ids": sorted("%s@%d" % (c["id"], c["version"]) for c in cs),
            "claim_count": n,
            "distinct_origins": origins,
            "reason": reason,
            "related": sorted(related)[:8],
            "exists": os.path.isfile(wiki.p(rel)),
            "stale": deps.is_stale(wiki, rel),
            "claim_details": [_detail(c) for c in cs],
        })

    # resolved declared topics with no eligible claims still surface as
    # waiting rows so the agent can see them pending
    for subject in sorted(topic_map):
        e = topic_map[subject]
        if (subject not in groups
                and e.get("status") == "resolved"
                and e["kind"] != "source"
                and topics.PAGE_DIR.get(e["kind"]) is not None):
            waiting.append({
                "subject": subject,
                "claim_count": 0,
                "distinct_origins": 0,
                "reason": "declared_no_claims",
            })

    # slug collisions: same (kind, slug) from distinct subjects — the first
    # subject (sorted order) keeps the base slug; the rest get a deterministic
    # "-<md5(subject)[:6]>" suffix (mirrored by write_page's slug check)
    seen = {}
    for t in targets:
        key = (t["kind"], t["slug"])
        if key not in seen:
            seen[key] = t
            continue
        suffix = hashlib.md5(t["subject"].encode("utf-8")).hexdigest()[:6]
        new_slug = "%s-%s" % (t["slug"], suffix)
        warnings.append("slug collision wiki/%s/%s.md: subject %r -> %s"
                        % (t["kind"], t["slug"], t["subject"], new_slug))
        t["slug"] = new_slug
        t["artifact"] = "wiki/%s/%s.md" % (t["kind"], new_slug)
        t["stale"] = deps.is_stale(wiki, t["artifact"])

    # extend-don't-create: NEW targets that near-duplicate an existing page
    # are routed extend-first (existing pages are refreshed, never re-created)
    for t in targets:
        if t["exists"]:
            continue
        sim = similar_pages(wiki, t["kind"], t["subject"], t["subject"].title())
        if sim:
            t["similar_existing"] = sim
            t["routing"] = "extend-first"

    if stale_only:
        targets = [t for t in targets if t["stale"]]
    targets.sort(key=lambda t: (not t["stale"], -t["claim_count"], t["subject"]))
    return {
        "targets": targets,
        "waiting": waiting,
        "stale_artifacts": [a for a in deps.stale_artifacts(wiki) if a.startswith("wiki/")],
        "warnings": warnings,
    }


def _claim_origins(wiki, c: dict) -> set:
    """Root origins behind one claim's evidence — mirrors claims.independence
    but returns the origin set so subjects can union across claims."""
    origins = set()
    for ev in c.get("evidence", []):
        ro = ev.get("root_origin")
        if not ro:
            try:
                manifest = wiki.load_json("sources/%s/manifest.json" % ev["source_id"])
                ro = manifest.get("root_origin", ev["source_id"])
            except Exception:
                ro = ev["source_id"]
        origins.add(ro)
    return origins


def _claim_exists(wiki, cid: str) -> bool:
    # file existence, not load_claim: link resolution must tolerate ids that
    # predate ULID validation (and test fixtures use short ids)
    return os.path.isfile(wiki.p("claims/%s.json" % cid))


def _load_named(wiki, subdir: str, oid: str) -> dict:
    """Load claims/<id>.json / decisions/<id>.json without ULID validation —
    link/dep resolution must tolerate pre-ULID and fixture ids — while still
    refusing traversal."""
    if not oid or "/" in oid or "\\" in oid or ".." in oid:
        raise TxnError("dep id invalid: %r" % oid)
    path = wiki.p("%s/%s.json" % (subdir, oid))
    if not os.path.isfile(path):
        raise TxnError("dep %r does not exist" % oid)
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        raise TxnError("dep %r unreadable: %s" % (oid, e))


def _page_links(wiki, artifact: str, body: str) -> list:
    """Resolve OKF-style markdown page links in the body to wiki/ rel paths.

    Accepts bundle-relative targets (`wiki/entities/x.md`) and same-dir or
    upward-relative targets (`./y.md`, `../entities/y.md`) resolved against
    the artifact's own directory. External URLs and non-.md links are
    ignored. Bundle-internal .md links that don't resolve are rejected."""
    out = []
    for target in _MD_LINK_RE.findall(body):
        if "://" in target:
            continue
        if target.startswith("wiki/"):
            rel = target
        else:
            resolved = os.path.normpath(os.path.join(os.path.dirname(artifact), target))
            rel = resolved.replace(os.sep, "/")
        if not rel.startswith("wiki/"):
            continue  # links outside the bundle are the author's business
        if rel == artifact:
            continue
        if not os.path.isfile(wiki.p(rel)):
            raise TxnError("unresolved page link %r -> %s in %s"
                           % (target, rel, artifact))
        if rel not in out:
            out.append(rel)
    return out


def _normalize_deps(wiki, dep_list) -> list:
    """Normalize typed deps to canonical strings: "claim_X@N" / "decision_Y@N"
    (version must match current; rejected/superseded claims refused) and
    "wiki/<dir>/<slug>.md" artifact paths (must exist on disk). Context
    receipts and unknown ids are rejected."""
    out = []
    for dep in dep_list or []:
        dep = str(dep).strip()
        if dep.startswith("wiki/"):
            if not dep.endswith(".md") or ".." in dep.split("/"):
                raise TxnError("dep %r is not a wiki/<kind>/<slug>.md path" % dep)
            if not os.path.isfile(wiki.p(dep)):
                raise TxnError("dep %r does not exist on disk" % dep)
            d = dep
        elif dep.startswith("decision_"):
            did, _, ver = dep.partition("@")
            dec = _load_named(wiki, "decisions", did)
            if ver:
                if not ver.isdigit() or int(ver) != dec["version"]:
                    raise TxnError("dep %s@%s does not match current version %d"
                                   % (did, ver, dec["version"]))
            d = "%s@%d" % (did, dec["version"])
        else:
            cid, _, ver = dep.partition("@")
            c = _load_named(wiki, "claims", cid)
            if ver:
                if not ver.isdigit() or int(ver) != c["version"]:
                    raise TxnError("dep %s@%s does not match current version %d"
                                   % (cid, ver, c["version"]))
            if c["status"] in claims.TERMINAL:
                raise TxnError("dep %s is %s; cite a current claim" % (cid, c["status"]))
            d = "%s@%d" % (cid, c["version"])
        if d not in out:
            out.append(d)
    return out


def write_page(wiki, artifact: str, markdown_text: str, dep_list: Optional[list],
               base_revision: int, allow_new: bool = False) -> dict:
    """Validate an agent-authored page and commit it atomically with its
    dependency registration and stale-flag clear.

    dep_list=None auto-derives deps from the body's [[claim_*]] links at their
    current versions."""
    rel = artifact.strip("/")
    if _is_reserved(rel):
        raise TxnError("reserved artifact cannot be written by write_page: %s" % rel)
    parts = rel.split("/")
    if (len(parts) != 3 or parts[0] != "wiki" or parts[1] not in PAGE_KINDS
            or not parts[2].endswith(".md")):
        raise TxnError("artifact must be wiki/<kind>/<slug>.md: %s" % artifact)

    front, body = _split_front(markdown_text)
    ftype = front.get("type")
    if not ftype:
        raise TxnError("frontmatter type missing/empty")
    if ftype != KIND_TYPE[parts[1]]:
        raise TxnError("frontmatter type %r does not match artifact kind %r"
                       % (ftype, KIND_TYPE[parts[1]]))
    subject = front.get("subject")
    if not subject or not str(subject).strip():
        raise TxnError("frontmatter subject missing/empty")
    if not front.get("title"):
        raise TxnError("frontmatter title missing/empty")
    subject = str(subject)
    base_slug = slugify(subject)
    want = parts[2][:-3]
    if want != base_slug and want != "%s-%s" % (
            base_slug, hashlib.md5(subject.encode("utf-8")).hexdigest()[:6]):
        raise TxnError("frontmatter subject %r does not match artifact slug %r"
                       % (subject, want))

    linked = set(LINK_RE.findall(body))
    unresolved = sorted(c for c in linked if not _claim_exists(wiki, c))
    if unresolved:
        raise TxnError("unresolved [[claim]] links: %s" % ", ".join(unresolved))
    # [[wiki:]] page links: self-links never allowed (the target of a
    # self-link cannot exist yet, so check before dep normalization), and
    # every link target must exist as a page on disk.
    pl_paths = sorted(page_links(body))
    for pl_path in pl_paths:
        if pl_path == rel:
            raise TxnError("self-link not allowed: page cannot link to "
                           "itself: %s" % rel)
        if not os.path.isfile(wiki.p(pl_path)):
            raise TxnError("unresolved [[wiki:%s]] link: %s"
                           % (pl_path[5:-3], pl_path))
    page_deps = _page_links(wiki, rel, body)  # OKF markdown links, validated
    if dep_list is None:
        # auto-derive: claim links pin to current versions; [[wiki:]] page
        # links join as wiki/<kind>/<slug>.md artifact deps
        dep_list = sorted(linked | set(pl_paths))
    dep_list = list(dep_list or []) + [p for p in page_deps
                                       if p not in dep_list]
    dep_set = _normalize_deps(wiki, dep_list)
    if not dep_set and ftype != "question":
        raise TxnError("deps required: cite at least one dep"
                       " (only type: question pages may have empty deps)")
    claim_dep_ids = {d.split("@", 1)[0] for d in dep_set if d.startswith("claim_")}
    missing = linked - claim_dep_ids
    if missing:
        raise TxnError("linked claims missing from deps: %s" % ", ".join(sorted(missing)))
    for pl_path in pl_paths:
        if pl_path not in dep_set:
            raise TxnError("linked page missing from deps: %s" % pl_path)
    for d in dep_set:
        did = d.split("@", 1)[0]
        if did.startswith("claim_") and _load_named(wiki, "claims", did)["status"] == "disputed":
            if front.get("disputed") is not True:
                raise TxnError("dep %s is disputed; set frontmatter disputed: true"
                               " to cite it" % did)

    rel_path = wiki.p(rel)
    is_new = not os.path.isfile(rel_path)
    twins = []
    old_body = None
    if not is_new:
        with open(rel_path) as f:
            old_front, old_body = _split_front(f.read())
        if str(old_front.get("subject") or "") != subject:
            raise TxnError("subject is immutable on refresh: existing %r != %r"
                           % (old_front.get("subject"), subject))
        if not front.get("created"):
            front["created"] = old_front.get("created") or _now()
    else:
        created = str(front.get("created") or "")
        if not re.match(r"^\d{4}-\d{2}-\d{2}T", created):
            raise TxnError("frontmatter created must be an ISO datetime"
                           " (YYYY-MM-DDT...) on new pages")
        twins = similar_pages(wiki, parts[1], subject,
                              str(front.get("title") or ""))
        if twins and not allow_new:
            tw = twins[0]
            names = ", ".join("wiki/%s/%s.md" % (t["section"], t["slug"])
                              for t in twins[:3])
            raise TxnError(
                "near-duplicate page: wiki/%s/%s.md (sim=%s, title='%s') "
                "already covers this — extend it, or pass --allow-new; "
                "twins: %s"
                % (tw["section"], tw["slug"], tw["similarity"],
                   tw["title"], names))

    if is_new and allow_new and twins:
        front["forced_new"] = True  # forced past the near-duplicate guard

    # OKF niceties: `owner`/`tags` pass through untouched when authored;
    # `owner` is backfilled from the first dep claim's independent origin.
    if not front.get("owner"):
        for d in dep_set:
            did = d.split("@", 1)[0]
            if did.startswith("claim_"):
                # _load_named (not load_claim): dep_set was already validated,
                # and test fixtures may use non-ULID ids
                c = _load_named(wiki, "claims", did)
                origins = [c.get("root_origin")] + [
                    ev.get("root_origin") for ev in c.get("evidence") or []]
                origin = next((o for o in origins if o), None)
                if origin:
                    front["owner"] = origin
                    break
    front["deps"] = dep_set
    front["updated"] = _now()
    front["stale"] = False
    try:
        text = "---\n%s\n---\n\n%s" % (yamldumps(front), body.strip() + "\n")
    except ValueError as e:
        raise TxnError("frontmatter invalid: %s" % e)

    txn = Transaction(wiki, "wiki-write-page")
    txn.stage_write(rel, text)
    deps.replace(txn, rel, dep_set)
    deps.clear_stale(txn, [rel])
    # A new page or a changed body invalidates every artifact that deps on
    # this page (transitively, via deps.invalidate's artifact-name cascade).
    if old_body is None or old_body.strip() != body.strip():
        deps.invalidate(wiki, [rel], txn=txn)
    txn.changes = {"page": rel}
    txn.commit(base_revision)
    return {"written": rel, "deps": dep_set,
            "claims_cited": len(linked)}


def page_show(wiki, artifact: str) -> dict:
    """Frontmatter, body, fresh claim details, staleness and registered deps
    for one derived page."""
    rel = artifact.strip("/")
    parts = rel.split("/")
    if (not rel.startswith("wiki/") or not rel.endswith(".md")
            or any(not p or p in (".", "..") for p in parts)):
        raise TxnError("artifact must be a path under wiki/: %s" % artifact)
    try:
        edges = deps.graph(wiki).get("edges", {})
    except (OSError, ValueError):
        edges = {}
    if rel not in edges:
        raise TxnError("artifact is not a registered wiki page: %s" % rel)
    if not os.path.isfile(wiki.p(rel)):
        raise TxnError("no such artifact: %s" % rel)
    with open(wiki.p(rel)) as f:
        front, body = _split_front(f.read())
    claim_details = []
    for dep in front.get("deps", []) or []:
        did = dep.split("@", 1)[0]
        if did.startswith("claim_"):
            try:
                claim_details.append(_detail(claims.load_claim(wiki, did)))
            except TxnError:
                pass
    return {
        "artifact": rel,
        "front": front,
        "body": body,
        "claim_details": claim_details,
        "stale": deps.is_stale(wiki, rel),
        "deps_registered": deps.graph(wiki).get("edges", {}).get(rel, []),
    }
