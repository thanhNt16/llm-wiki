"""wiki-doctor: static + semantic diagnostics over the wiki (PRD §37, §24)."""
import datetime
import fcntl
import json
import os
import re

from . import claims, deps, review, schema as schema_mod, synthesize
from .hashing import canonical_json
from .transaction import TxnError

_SCHEMAS = os.path.join(os.path.dirname(__file__), "..", "..", "schemas")

_ID_RE = re.compile(r"^[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$")

# Markdown links to .md targets: [label](target.md); wiki links: [[claim_id]].
_MD_LINK_RE = re.compile(r"\[[^\]]*\]\(([^)#\s]+\.md)\)")
_WIKI_LINK_RE = re.compile(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]")

# Wiki sections whose content pages make up the navigation fabric.
_PAGE_SECTIONS = ("concepts", "entities", "procedures", "questions",
                  "sources", "changes")

# Candidates left unreviewed longer than this are flagged as semantic debt.
_CANDIDATE_AGE_DAYS = 14


def _load_schema(name):
    with open(os.path.join(_SCHEMAS, name)) as f:
        return json.load(f)


def run(wiki) -> dict:
    findings = []

    def add(severity, code, detail, fix):
        findings.append({"severity": severity, "code": code, "detail": detail, "fix": fix})

    # --- static: schema validity of every canonical object -----------------
    claim_files = sorted(os.listdir(wiki.p("claims"))) if os.path.isdir(wiki.p("claims")) else []
    # keep successfully parsed claims for the semantic checks below so a
    # single corrupt file cannot abort the whole run
    claim_objs = []
    for name in claim_files:
        if not (name.startswith("claim_") and name.endswith(".json")):
            continue
        path = wiki.p("claims", name)
        try:
            with open(path) as f:
                data = json.load(f)
        except ValueError as e:
            add("error", "schema_invalid", "claims/%s: %s" % (name, e),
                "restore from operations history or delete (derived views rebuild)")
            continue
        claim_objs.append(data)
        errors = schema_mod.validate(data, _load_schema("claim.schema.json"))
        if errors:
            add("error", "schema_invalid", "claims/%s: %s" % (name, "; ".join(errors)),
                "fix via wiki-review actions or correct the JSON to schema")
        if data.get("id") != name[:-5]:
            add("error", "invalid_id", "claims/%s: id mismatch" % name,
                "rename file or fix id field")

    decision_files = (sorted(os.listdir(wiki.p("decisions")))
                      if os.path.isdir(wiki.p("decisions")) else [])
    decision_objs = []
    for name in decision_files:
        if not (name.startswith("decision_") and name.endswith(".json")):
            continue
        # corrupt JSON is a finding, never a crash (same guard as claims)
        try:
            with open(wiki.p("decisions", name)) as f:
                data = json.load(f)
        except ValueError as e:
            add("error", "schema_invalid", "decisions/%s: %s" % (name, e),
                "restore from operations history or correct the JSON")
            continue
        decision_objs.append(data)
        errors = schema_mod.validate(data, _load_schema("decision.schema.json"))
        if errors:
            add("error", "schema_invalid", "decisions/%s: %s" % (name, "; ".join(errors)),
                "fix decision JSON to schema")
        if data.get("id") != name[:-5]:
            add("error", "invalid_id", "decisions/%s: id mismatch" % name,
                "rename file or fix id field")

    auto_stubs = sum(1 for d in decision_objs
                     if d.get("auto_materialized") is True
                     and d.get("status") == "accepted")
    if auto_stubs:
        add("info", "auto_decision_stub",
            "%d auto-materialized decision stub(s) awaiting human review" % auto_stubs,
            "review decisions/ stubs and flesh out or retract via wiki-decide/wiki-review")

    # --- static: broken references ----------------------------------------
    known_sources = set(os.listdir(wiki.p("sources"))) if os.path.isdir(wiki.p("sources")) else set()
    claim_ids = {name[:-5] for name in claim_files if name.endswith(".json")}
    for c in claim_objs:
        for ev in c.get("evidence", []):
            sid = ev.get("source_id", "")
            if sid.split("@")[0] not in known_sources:
                add("error", "broken_reference",
                    "claim %s cites missing source %s" % (c.get("id", "?"), sid),
                    "re-ingest the source or correct the citation")
            elif isinstance(ev.get("source_version"), int):
                try:
                    m = wiki.load_json("sources/%s/manifest.json" % sid)
                    if ev["source_version"] > len(m["versions"]):
                        add("error", "broken_reference",
                            "claim %s cites version %d of %s (only %d exist)"
                            % (c.get("id", "?"), ev["source_version"], sid, len(m["versions"])),
                            "correct the version or re-ingest")
                except Exception:
                    pass
        for sup in c.get("supersedes", []):
            if sup not in claim_ids:
                add("error", "broken_reference",
                    "claim %s supersedes missing claim %s" % (c.get("id", "?"), sup),
                    "correct or remove the supersedes entry")
    terminal_claims = {c["id"] for c in claim_objs
                       if c.get("id") and c.get("status") in claims.TERMINAL}
    for d in decision_objs:
        for cid in d.get("claims", []):
            if cid not in claim_ids:
                add("error", "broken_reference",
                    "decision %s references missing claim %s" % (d.get("id", "?"), cid),
                    "update the decision's claim list")
        for ev in d.get("evidence", []):
            sid = ev.get("source_id", "")
            if sid.split("@")[0] not in known_sources:
                add("error", "broken_reference",
                    "decision %s cites missing source %s" % (d.get("id", "?"), sid),
                    "re-ingest the source or correct the citation")
        if not d.get("claims") and not d.get("evidence"):
            add("warn", "orphan_decision",
                "decision %s (%s) references no claims and cites no evidence"
                % (d.get("id", "?"), d.get("title", "?")),
                "link supporting claims or evidence, or retract the decision")
        stale_refs = [cid for cid in d.get("claims", []) if cid in terminal_claims]
        if d.get("status") in ("proposed", "accepted") and stale_refs:
            add("warn", "stale_decision",
                "decision %s rests on superseded/rejected claim(s) %s"
                % (d.get("id", "?"), ", ".join(sorted(stale_refs))),
                "revisit the decision in light of the newer claims")

    # --- static: unextracted/orphan sources; staging leftovers --------------
    # A source gets page coverage when a wiki page registers a dep on a claim
    # that cites it (pages cite claims, not sources). Split: sources with zero
    # citing claims are unextracted_source; cited-but-uncovered are
    # orphan_source.
    try:
        dep_edges = deps.graph(wiki).get("edges", {})
    except (OSError, ValueError):
        dep_edges = {}
    claims_by_id = {c.get("id"): c for c in claim_objs}
    covered_sources = set()
    for page, page_deps in dep_edges.items():
        if not page.startswith("wiki/") or not os.path.isfile(wiki.p(page)):
            continue
        for d in page_deps:
            c = claims_by_id.get(d.split("@", 1)[0])
            for ev in (c or {}).get("evidence", []):
                if ev.get("source_id"):
                    covered_sources.add(ev["source_id"])
    for sid in known_sources:
        if not _ID_RE.match(sid):
            add("warn", "invalid_id", "sources/%s is not a valid id" % sid, "rename directory")
        elif not _source_cited(claim_objs, sid):
            add("info", "unextracted_source", "source %s is not cited by any claim" % sid,
                "run wiki-compile to extract claims, or leave as raw evidence")
        elif sid not in covered_sources:
            add("info", "orphan_source",
                "source %s is cited by claims but no page covers it" % sid,
                "run wiki page-targets/write-page to synthesize its claims")
    staging = wiki.p(".state/staging")
    if os.path.isdir(staging):
        for entry in sorted(os.listdir(staging)):
            if entry.startswith("run_"):
                add("info", "staging_leftover", "staging run %s still present" % entry,
                    "inspect then delete .state/staging/%s (failed or abandoned run)" % entry)
    # --- transaction atomicity leftovers (R10) ------------------------------
    state_dir = wiki.p(".state")
    if os.path.isdir(state_dir):
        if os.path.isfile(os.path.join(state_dir, "journal.json")):
            add("error", "transaction_journal_leftover",
                "interrupted transaction left .state/journal.json",
                "restore affected files from the newest .state/.backup-<rev>/ dir, "
                "then delete the journal")
        for entry in sorted(os.listdir(state_dir)):
            if entry.startswith(".backup-") and os.path.isdir(os.path.join(state_dir, entry)):
                add("warn", "stale_transaction_backup",
                    "transaction backup dir .state/%s still present" % entry,
                    "verify the corresponding commit landed, then delete .state/%s" % entry)

    # --- commit-lock hygiene ---------------------------------------------
    # transaction.py holds an flock on .state/locks/commit.lock only while a
    # commit runs; a still-held lock after that is a stale-lock condition.
    locks_dir = wiki.p(".state", "locks")
    if os.path.isdir(locks_dir):
        for entry in sorted(os.listdir(locks_dir)):
            if entry != "commit.lock":
                add("warn", "stale_lock",
                    "unexpected file .state/locks/%s" % entry,
                    "inspect then delete .state/locks/%s" % entry)
        commit_lock = os.path.join(locks_dir, "commit.lock")
        if os.path.isfile(commit_lock):
            try:
                fd = open(commit_lock, "r")
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except OSError:
                    add("warn", "stale_lock",
                        ".state/locks/commit.lock is still held",
                        "if no wiki command is running, remove the lock file")
                else:
                    fcntl.flock(fd, fcntl.LOCK_UN)
                fd.close()
            except OSError:
                pass

    # --- static: stale derived artifacts -----------------------------------
    for artifact in deps.stale_artifacts(wiki):
        add("warn", "stale_dependency", "derived artifact %s is stale" % artifact,
            "rebuild via wiki-compile / build-pages")

    # --- unregistered derived pages (R10) -----------------------------------
    edges = deps.graph(wiki).get("edges", {})
    for sub in ("sources", "concepts", "entities", "procedures", "questions",
                "decisions", "changes"):
        subdir = wiki.p("wiki", sub)
        if not os.path.isdir(subdir):
            continue
        for name in sorted(os.listdir(subdir)):
            if not name.endswith(".md"):
                continue
            rel = "wiki/%s/%s" % (sub, name)
            if rel not in edges:
                add("warn", "unregistered_page",
                    "derived page %s has no dependency registration" % rel,
                    "rebuild via build-pages (script-owned) or re-write via "
                    "write-page (agent-authored)")

    # --- dependency-edge integrity (read-only mirror of pages.verify) -----
    # Every registered dep must resolve: claim_X@N / decision_Y@N against the
    # current version, review_* against the queue, wiki/* against registered
    # edges or an existing file.
    known_versions = {c["id"]: c.get("version") for c in claim_objs if c.get("id")}
    known_versions.update({d["id"]: d.get("version") for d in decision_objs
                           if d.get("id")})
    queue_ids = {i.get("id") for i in _queue_items(wiki)}
    for artifact, dep_list in edges.items():
        if not os.path.isfile(wiki.p(artifact)):
            add("error", "missing_artifact",
                "registered artifact %s does not exist" % artifact,
                "rebuild it, or remove its edge from .state/dependencies.json")
        if not isinstance(dep_list, list):
            continue
        for dep in dep_list:
            dep_id, _, ver = str(dep).strip().partition("@")
            unresolved = False
            if dep_id.startswith("wiki/"):
                unresolved = not (dep_id in edges or os.path.isfile(wiki.p(dep_id)))
            elif dep_id.startswith("review_"):
                unresolved = dep_id not in queue_ids
            elif dep_id in known_versions:
                cur = known_versions[dep_id]
                if ver and (not ver.isdigit() or int(ver) != cur):
                    add("warn", "unresolved_dependency",
                        "dependency %s@%s in %s does not match current version %s"
                        % (dep_id, ver, artifact, cur),
                        "rebuild the artifact so its deps pin real versions")
                continue
            else:
                unresolved = True
            if unresolved:
                add("warn", "unresolved_dependency",
                    "unresolved dependency %r in %s" % (dep, artifact),
                    "rebuild the artifact or fix .state/dependencies.json")

    # --- semantic debt (PRD §24) -------------------------------------------
    queue_items = _queue_items(wiki)
    under_review = {a.split("@")[0]
                    for item in queue_items if item.get("status") == "open"
                    for a in item.get("affected", [])}
    for c in claim_objs:
        cid = c.get("id", "?")
        if c.get("status") == "accepted":
            indep = claims.independence(wiki, c)
            if indep == 0 and (c.get("authority") or {}).get("type") == "agent_inference":
                add("warn", "unsupported_accepted",
                    "accepted claim %s has no independent evidence" % cid,
                    "corroborate, demote to candidate, or reject via wiki-review")
            if not c.get("scope"):
                add("info", "missing_scope",
                    "accepted claim %s has empty scope" % cid,
                    "set scope via wiki-review set-scope")
            if not c.get("valid_from"):
                add("info", "missing_effective_date",
                    "accepted claim %s has no valid_from" % cid,
                    "set validity via wiki-review set-validity")
        if c.get("status") == "disputed":
            add("warn", "unresolved_contradiction",
                "claim %s is disputed" % cid, "resolve via wiki-review")
        if not c.get("evidence"):
            add("warn", "orphan_claim",
                "claim %s (%s) has no evidence" % (cid, c.get("subject", "-")),
                "add evidence via wiki-ingest/compile, or reject via wiki-review")
        if c.get("status") == "candidate" and c.get("id") not in under_review \
                and review.age_days(c.get("recorded_at")) > _CANDIDATE_AGE_DAYS:
            add("warn", "stale_candidate",
                "candidate claim %s has been unreviewed for %d days"
                % (cid, review.age_days(c.get("recorded_at"))),
                "accept, reject, or defer via wiki-review")

    # near-identical live claims (same subject, predicate, scope, value) are
    # probable duplicates the reconciler missed — they split evidence and
    # confuse queries
    dup_groups = {}
    for c in claim_objs:
        if c.get("status") in claims.TERMINAL or not c.get("id"):
            continue
        key = (c.get("subject"), c.get("predicate"),
               canonical_json(c.get("scope") or {}),
               canonical_json(c.get("value")))
        dup_groups.setdefault(key, []).append(c["id"])
    for ids_in_group in dup_groups.values():
        if len(ids_in_group) > 1:
            add("info", "possible_duplicate",
                "claims %s share subject/predicate/scope/value"
                % ", ".join(sorted(ids_in_group)),
                "merge or mark_duplicate via wiki-review")

    for item in queue_items:
        if item.get("status") == "open" and item.get("kind") == "possible_contradiction":
            add("warn", "unresolved_contradiction",
                "open review item: %s" % item["problem"], "resolve via wiki-review")

    # --- review queue hygiene (R10) -----------------------------------------
    known_review_ids = set(claim_ids)
    known_review_ids.update(d["id"] for d in decision_objs if d.get("id"))
    queue_path = wiki.p(review.QUEUE)
    if os.path.isfile(queue_path):
        with open(queue_path, encoding="utf-8") as f:
            for lineno, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    item = json.loads(line)
                except ValueError:
                    add("error", "malformed_review_item",
                        "review-queue line %d is not valid JSON" % lineno,
                        "repair or remove .state/review-queue.jsonl line %d "
                        "(queue is transaction-owned; restore from backup)" % lineno)
                    continue
                missing = [k for k in ("id", "created_at", "kind", "status",
                                       "problem", "affected", "risk")
                           if k not in item]
                if missing:
                    add("error", "malformed_review_item",
                        "review item %s missing fields: %s"
                        % (item.get("id", "?"), ", ".join(missing)),
                        "restore .state/review-queue.jsonl from backup")
                    continue
                for aid in item.get("affected") or []:
                    if aid not in known_review_ids:
                        add("warn", "broken_reference",
                            "review item %s affected id %s no longer exists"
                            % (item["id"], aid),
                            "update or resolve the review item via wiki-review")
                if item.get("status") == "deferred" and review.age_days(
                        item.get("deferred_at")) > 7:
                    add("warn", "deferred_aging",
                        "review item %s deferred for %d days"
                        % (item["id"], review.age_days(item.get("deferred_at"))),
                        "reopen or resolve via wiki-review (defer/reopen)")

    # low extraction coverage on text-like sources
    if os.path.isdir(wiki.p("sources")):
        for sid in sorted(known_sources):
            xpath = wiki.p("sources", sid, "extraction.json")
            if not os.path.isfile(xpath):
                continue
            with open(xpath) as f:
                ext = json.load(f)
            cov = (ext.get("extraction_report") or {}).get("coverage", {})
            if cov.get("text") == "not_extracted":
                add("info", "low_extraction_coverage",
                    "source %s text was not extracted" % sid,
                    "ingest with a document parser for semantic extraction")

    # --- two-tier lint (spec §2) ------------------------------------------
    # Tier 1, index fabric: every content page listed in its section index,
    # every section index linked from overview.md (progressive-disclosure
    # navigation). Tier 2, page graph: orphan/dead-end/walled-garden/
    # near-duplicate/TTL over body [[wiki:]] links + page dep edges.
    plan = []

    def plan_add(code, path, action):
        plan.append({"code": code, "path": path, "action": action})

    _index_topology_checks(wiki, add, plan_add)
    _page_graph_checks(wiki, add, plan_add)

    # --- topic registry debt ----------------------------------------------
    try:
        from . import topics as topics_mod
        treg = topics_mod.load(wiki)["topics"]
    except Exception as e:
        treg = {}
        add("error", "topics_registry_corrupt",
            "cannot read topic registry topics.json: %s" % e,
            "fix or remove .llm-wiki/topics.json")
    conflicts_n = sum(1 for e in treg.values() if e.get("status") == "conflict")
    if conflicts_n:
        add("warn", "topic_kind_conflict",
            "%d topic(s) have unresolved kind conflicts" % conflicts_n,
            "resolve via review act set_topic_kind")
    unclaimed = topics_mod.unclaimed(wiki) if treg else []
    if unclaimed:
        add("info", "topic_unclaimed",
            "%d declared topic(s) have no claims: %s" % (len(unclaimed), ", ".join(unclaimed[:5])),
            "extract claims for them or accept thin coverage")
    auto_n = sum(1 for e in treg.values() if e.get("auto_registered"))
    if auto_n:
        add("info", "topics_auto_registered",
            "%d topic(s) auto-registered (kind inferred)" % auto_n,
            "declare kinds explicitly in topics.jsonl")
    from .text import slugify
    for subj, e in treg.items():
        want = topics_mod.PAGE_DIR.get(e.get("kind"))
        if not want or e.get("status") != "resolved":
            continue
        slug = slugify(subj)
        wikip = wiki.p("wiki")
        if not os.path.isdir(wikip):
            continue
        for other in os.listdir(wikip):
            other_p = os.path.join(wikip, other, slug + ".md")
            if other != want and os.path.isfile(other_p) and \
                    other in synthesize.PAGE_KINDS:
                add("error", "topic_page_kind_mismatch",
                    "topic %r is %s but page exists at wiki/%s/%s.md" % (subj, e["kind"], other, slug),
                    "re-write page at wiki/%s/%s.md or fix the topic kind" % (want, slug))


    severity_rank = {"error": 0, "warn": 1, "info": 2}
    findings.sort(key=lambda f: (severity_rank[f["severity"]], f["code"]))
    ok = not any(f["severity"] == "error" for f in findings)
    return {"ok": ok, "findings": findings,
            "counts": {s: sum(1 for f in findings if f["severity"] == s)
                       for s in ("error", "warn", "info")},
            "repair_plan": sorted(plan, key=lambda p: (p["code"], p["path"]))}


def _read(rel, wiki):
    path = wiki.p(rel)
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return None


def _wiki_ref(page_rel):
    """wiki/<kind>/<slug>.md -> [[wiki:<kind>/<slug>]] link target text."""
    return "wiki:" + page_rel[len("wiki/"):-len(".md")]


def _index_topology_checks(wiki, add, plan_add):
    """Index-fabric lint: every content page must be listed in its section
    index, and every section index must be linked from overview.md (the
    overview -> section index -> page ladder is the progressive-disclosure
    fabric). Index and overview pages are exempt structural nodes."""
    overview = _read("wiki/overview.md", wiki) or ""
    for section in _PAGE_SECTIONS:
        sec_dir = wiki.p("wiki", section)
        if not os.path.isdir(sec_dir):
            continue
        index_rel = "wiki/%s/index.md" % section
        index_body = _read(index_rel, wiki) or ""
        index_links = set(_MD_LINK_RE.findall(index_body))
        linked_from_overview = ("%s/index.md" % section) in overview
        if os.path.isfile(wiki.p(index_rel)) and not linked_from_overview:
            add("warn", "section_index_unlinked",
                "wiki/%s/index.md exists but overview.md does not link it"
                % section,
                "rebuild pages (build-pages regenerates overview) or add the "
                "section link to overview.md")
            plan_add("section_index_unlinked", index_rel,
                     "run `wiki.py doctor --fix-indexes` to regenerate "
                     "overview.md and restore the [[%s]] section link"
                     % _wiki_ref(index_rel))
        for name in sorted(os.listdir(sec_dir)):
            if not name.endswith(".md") or name == "index.md":
                continue
            if name not in index_links:
                rel = "wiki/%s/%s" % (section, name)
                add("warn", "unlisted_page",
                    "wiki/%s/%s is not listed in its section index"
                    % (section, name),
                    "rebuild pages (build-pages regenerates section "
                    "indexes) or list the page in its section index")
                plan_add("unlisted_page", rel,
                         "run `wiki.py doctor --fix-indexes` to regenerate "
                         "wiki/%s/index.md so it lists [[%s]]"
                         % (section, _wiki_ref(rel)))


def _source_cited(claim_objs: list, sid: str) -> bool:
    for c in claim_objs:
        for ev in c.get("evidence", []):
            if ev.get("source_id") == sid:
                return True
    return False

def _page_graph_checks(wiki, add, plan_add, pages=None):
    """Page-graph lint (all findings warn) over `pages` — the agent-authored
    section pages. When `pages` is None the universe is every
    wiki/<kind>/<slug>.md on disk (kind in synthesize.PAGE_KINDS, index.md
    excluded) plus any such page with registered dep edges.

    Outbound edges per page = body [[wiki:kind/slug]] links
    (synthesize.extract_page_links) UNION page-dep tokens — frontmatter
    deps as `wiki:<kind>/<slug>` and the dep registry's canonical
    `wiki/<kind>/<slug>.md` paths — restricted to edges whose target is
    itself a valid page. Pages with malformed frontmatter emit an
    error-severity finding and are skipped by every graph check."""
    edges = deps.graph(wiki).get("edges", {})
    if pages is None:
        # Page universe: agent-authored section pages. The filter matches
        # the write_page domain (kind in PAGE_KINDS, not index.md) so
        # script-owned views (wiki/decisions/*.md, section indexes) are
        # excluded.
        arts = [a for a in edges
                if a.startswith("wiki/") and a.count("/") == 2
                and a.endswith(".md")
                and a.split("/")[1] in synthesize.PAGE_KINDS
                and a.rsplit("/", 1)[1] != "index.md"]
        for kind in synthesize.PAGE_KINDS:
            d = wiki.p("wiki", kind)
            if os.path.isdir(d):
                for n in os.listdir(d):
                    rel = "wiki/%s/%s" % (kind, n)
                    if n.endswith(".md") and n != "index.md" \
                            and rel not in arts:
                        arts.append(rel)
        pages = sorted(arts)  # sorted: findings stay deterministic

    # Parse every page once. Malformed frontmatter is an error finding and
    # removes the page from the graph — diagnostics never crash on it.
    fronts, bodies, valid = {}, {}, []
    for a in sorted(pages):
        try:
            with open(wiki.p(a), encoding="utf-8") as f:
                text = f.read()
        except OSError:
            continue
        try:
            front, body = synthesize._split_front(text)
        except (ValueError, TxnError):
            front = None
        # synthesize._split_front never raises on an unterminated fence —
        # it just finds no frontmatter. "Starts with ---" without a closing
        # fence is malformed.
        if front is None or (text.startswith("---")
                             and not synthesize._FRONT_RE.match(text)):
            add("error", "malformed_frontmatter",
                "%s has malformed frontmatter" % a,
                "fix the --- frontmatter block or rewrite via write-page")
            plan_add("malformed_frontmatter", a,
                     "fix the --- frontmatter block of [[%s]] or rewrite "
                     "the page via write-page" % _wiki_ref(a))
            continue
        fronts[a], bodies[a] = front, body
        valid.append(a)
    page_set = set(valid)

    def _front_deps(front):
        ds = front.get("deps")
        if isinstance(ds, str):
            ds = [ds]
        if not isinstance(ds, list):
            return []
        return [str(d) for d in ds]

    out_e = {a: set() for a in valid}
    for a in valid:
        body = bodies[a]
        for ref in synthesize.extract_page_links(body):
            out_e[a].add("wiki/%s.md" % ref)
        for d in _front_deps(fronts[a]):
            if d.startswith("wiki:"):
                out_e[a].add("wiki/%s.md" % d[5:])
            elif d.startswith("wiki/"):
                out_e[a].add(d.split("@", 1)[0])
        for d in edges.get(a, []) or []:
            if isinstance(d, str):
                d = d.split("@", 1)[0]
                if d.startswith("wiki:"):
                    d = "wiki/%s.md" % d[5:]
                if d.startswith("wiki/"):
                    out_e[a].add(d)
    for a in valid:
        out_e[a] = sorted(d for d in out_e[a]
                          if d in page_set and d != a)
    in_e = {a: [] for a in valid}
    for a, ds in out_e.items():
        for d in ds:
            in_e[d].append(a)
    for ds in in_e.values():
        ds.sort()

    for a in valid:
        # An orphan needs a graph to be orphaned from: never flag a
        # single-page wiki.
        if len(valid) > 1 and not in_e[a]:
            add("warn", "orphan_page", "%s has no inbound page links" % a,
                "link it from a related page or a question page")
            plan_add("orphan_page", a,
                     "add a [[%s]] link to a related page's See-also "
                     "section and deps" % _wiki_ref(a))
        if not out_e[a]:
            cites = bool(_WIKI_LINK_RE.search(bodies[a])) or any(
                d.split("@", 1)[0].startswith("claim_")
                for d in _front_deps(fronts[a]))
            hint = ("cites claims but has no outbound [[wiki:]] page links"
                    if cites else "links to no other page")
            add("warn", "dead_end_page", "%s %s" % (a, hint),
                "add a See also section with [[wiki:kind/slug]] links")
            plan_add("dead_end_page", a,
                     "add a See-also section to [[%s]] with "
                     "[[wiki:kind/slug]] links to related pages and "
                     "register them in deps" % _wiki_ref(a))

    # walled gardens: undirected connected components of >=3 pages —
    # clusters with no edge (in either direction) to the rest of the
    # graph. The single largest component is the wiki's main body and is
    # never a garden; a wiki whose pages form one component is just
    # well-connected. (Spec §2)
    undir = {a: set(ds) for a, ds in out_e.items()}
    for a, ds in in_e.items():
        undir[a].update(ds)
    comps, seen = [], set()
    for a in sorted(valid):
        if a in seen:
            continue
        comp, stack = set(), [a]
        while stack:
            x = stack.pop()
            if x not in comp:
                comp.add(x)
                stack.extend(undir[x] - comp)
        comps.append(sorted(comp))
        seen |= comp
    if len(comps) > 1:
        # Deterministic largest: size first, member list as tiebreak.
        main = max(comps, key=lambda c: (len(c), c))
        for members in comps:
            if len(members) >= 3 and members != main:
                add("warn", "walled_garden",
                    "%d pages only link to each other: %s"
                    % (len(members), ", ".join(members)),
                    "cross-link the cluster to the rest of the wiki")
                plan_add("walled_garden", members[0],
                         "link [[%s]] into the main component by adding a "
                         "[[wiki:kind/slug]] edge to a related page "
                         "outside the cluster" % _wiki_ref(members[0]))

    # near-duplicate subjects: title+subject token Jaccard at/above
    # NEAR_DUP_LINT_THRESHOLD, only for pairs not already linked in either
    # direction. candidate_pairs over the shared inverted index yields only
    # token-sharing pairs — zero-overlap pairs can't reach any positive
    # threshold, so skipping them is lossless. Sorted -> deterministic
    # pair order (identical to the old naive double loop: valid is sorted).
    tokens = {a: (synthesize._normalize_tokens(fronts[a].get("title"))
                  | synthesize._normalize_tokens(fronts[a].get("subject")))
              for a in valid}
    for a, b in sorted(
            synthesize.candidate_pairs(synthesize.build_token_index(tokens))):
        jac = synthesize._jaccard(tokens[a], tokens[b])
        if jac >= synthesize.NEAR_DUP_LINT_THRESHOLD \
                and b not in out_e[a] and a not in out_e[b]:
            add("warn", "near_duplicate_subject",
                "%s and %s have near-identical subjects (Jaccard %.2f)"
                % (a, b, jac),
                "merge the pages or link them explicitly")
            plan_add("near_duplicate_subject", a,
                     "extend [[%s]] to cover the topic or differentiate "
                     "[[%s]]'s subject" % (_wiki_ref(b), _wiki_ref(a)))

    today = datetime.date.today()
    for a in valid:
        raw = fronts[a].get("stale_after")
        if not isinstance(raw, str):
            continue
        try:
            cutoff = datetime.date.fromisoformat(raw.strip())
        except ValueError:
            continue
        if cutoff < today:
            add("warn", "stale_page_ttl",
                "%s passed its stale_after date %s" % (a, raw),
                "refresh the page or bump stale_after")
            plan_add("stale_page_ttl", a,
                     "refresh [[%s]] against current claims or bump its "
                     "stale_after date" % _wiki_ref(a))


def _queue_items(wiki) -> list:
    """Parsed review items, skipping unparseable lines (flagged separately)."""
    out = []
    path = wiki.p(review.QUEUE)
    if not os.path.isfile(path):
        return out
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except ValueError:
                continue
    return out

