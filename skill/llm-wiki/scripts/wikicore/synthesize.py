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

from . import claims, deps
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

LINK_RE = re.compile(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]")
_FRONT_RE = re.compile(r"\A---\s*\n(.*?)\n?---\s*\n?", re.S)

KIND_RULES_FILE = "page-kinds.json"


def _load_kind_rules(wiki, warnings: list) -> list:
    """Ordered kind-routing rules from .llm-wiki/page-kinds.json.

    Shape: {"rules": [{"kind": "entities", "subject"|"prefix"|"suffix"|
    "regex"|"predicate": <str>, "when": {"min_claims": n}?}...],
    "default": "concepts"}. First match wins; --kind overrides all.
    Extend by appending a rule — no code changes."""
    path = wiki.p(KIND_RULES_FILE)
    if not os.path.isfile(path):
        return [], "concepts"
    try:
        with open(path) as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        warnings.append("%s ignored: %s" % (KIND_RULES_FILE, e))
        return [], "concepts"
    if not isinstance(data, dict):
        warnings.append("%s ignored: expected an object" % KIND_RULES_FILE)
        return [], "concepts"
    default = data.get("default", "concepts")
    if default not in PAGE_KINDS:
        warnings.append("%s: default %r not a page kind — using 'concepts'"
                        % (KIND_RULES_FILE, default))
        default = "concepts"
    rules = []
    for i, r in enumerate(data.get("rules") or []):
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
    return rules, default


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


def _route_kind(subject: str, clist: list, rules: list,
                default: str):
    """First matching rule wins. Returns (kind, matched_rule_index|None)."""
    for i, rule, matchers in rules:
        if _rule_matches(rule, matchers, subject, clist):
            return rule["kind"], i
    return default, None



def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def slugify(subject: str) -> str:
    """Subject -> slug: lowercase, non-alphanumeric runs -> '-', <=60 chars cut
    at a '-' boundary, trimmed."""
    slug = re.sub(r"[^a-z0-9]+", "-", subject.lower()).strip("-")
    if len(slug) > 60:
        slug = slug[:60]
        if "-" in slug:
            slug = slug[: slug.rindex("-")]
        slug = slug.strip("-")
    return slug or "untitled"


def _is_reserved(artifact: str) -> bool:
    a = artifact.strip("/")
    return (
        a in ("wiki/index.md", "wiki/overview.md")
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
    rules, default_kind = _load_kind_rules(wiki, warnings)
    targets = []
    waiting = []
    for subject in sorted(groups):
        cs = groups[subject]
        n = len(cs)
        origins = sum(claims.independence(wiki, c) for c in cs)
        if n >= min_claims and origins >= min_sources:
            reason = "corroborated"
        elif any((c.get("authority") or {}).get("type") == AUTHORITATIVE for c in cs):
            reason = "authoritative"
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
        else:
            k, matched = _route_kind(subject, cs, rules, default_kind)
        slug = slugify(subject)
        rel = "wiki/%s/%s.md" % (k, slug)
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
            "exists": os.path.isfile(wiki.p(rel)),
            "stale": deps.is_stale(wiki, rel),
            "claim_details": [_detail(c) for c in cs],
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

    if stale_only:
        targets = [t for t in targets if t["stale"]]
    targets.sort(key=lambda t: (not t["stale"], -t["claim_count"], t["subject"]))
    return {
        "targets": targets,
        "waiting": waiting,
        "stale_artifacts": [a for a in deps.stale_artifacts(wiki) if a.startswith("wiki/")],
        "warnings": warnings,
    }


def _claim_exists(wiki, cid: str) -> bool:
    try:
        claims.load_claim(wiki, cid)
        return True
    except TxnError:
        return False


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
            dec = claims.load_decision(wiki, did)  # TxnError on unknown id
            if ver:
                if not ver.isdigit() or int(ver) != dec["version"]:
                    raise TxnError("dep %s@%s does not match current version %d"
                                   % (did, ver, dec["version"]))
            d = "%s@%d" % (did, dec["version"])
        else:
            cid, _, ver = dep.partition("@")
            c = claims.load_claim(wiki, cid)  # TxnError on unknown id
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
               base_revision: int) -> dict:
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
    if dep_list is None:
        dep_list = sorted(linked)  # auto-derive: bare ids pin to current version
    dep_set = _normalize_deps(wiki, dep_list)
    if not dep_set and ftype != "question":
        raise TxnError("deps required: cite at least one dep"
                       " (only type: question pages may have empty deps)")
    claim_dep_ids = {d.split("@", 1)[0] for d in dep_set if d.startswith("claim_")}
    missing = linked - claim_dep_ids
    if missing:
        raise TxnError("linked claims missing from deps: %s" % ", ".join(sorted(missing)))
    for d in dep_set:
        did = d.split("@", 1)[0]
        if did.startswith("claim_") and claims.load_claim(wiki, did)["status"] == "disputed":
            if front.get("disputed") is not True:
                raise TxnError("dep %s is disputed; set frontmatter disputed: true"
                               " to cite it" % did)

    rel_path = wiki.p(rel)
    if os.path.isfile(rel_path):
        with open(rel_path) as f:
            old_front, _ = _split_front(f.read())
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
    txn.changes = {"page": rel}
    txn.commit(base_revision)
    return {"written": rel, "deps": dep_set,
            "claims_cited": len(linked)}


def page_show(wiki, artifact: str) -> dict:
    """Frontmatter, body, fresh claim details, staleness and registered deps
    for one derived page."""
    rel = artifact.strip("/")
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
