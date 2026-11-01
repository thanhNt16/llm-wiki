"""SP2c: built-in ingestion connectors — git history and RSS/Atom feeds.

Zero third-party deps: git via subprocess, feeds via xml.etree.ElementTree,
HTML tag stripping via html.parser. Every item funnels through
``sources.ingest`` (sha256 dedup, secret gate, versioning) exactly like
``--file``/``--url``, so compile treats connector output as ordinary
evidence with ordinary provenance.

``sources`` is imported lazily inside the ingest functions: the
transaction<->claims import cycle requires claims/transaction to load
first when this module is imported standalone.
"""
import json
import os
import re
import subprocess
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from html.parser import HTMLParser

from . import claims  # noqa: F401 — import-order guard: transaction<->claims cycle
from .transaction import TxnError

PARSER = {"name": "wikicore-connector", "version": "1.0"}


# ---------------------------------------------------------------- git ----

def _git(repo: str, *args: str) -> str:
    p = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True)
    if p.returncode != 0:
        raise TxnError("git connector: %s: %s" % (
            args[0], p.stderr.strip()[:200]))
    return p.stdout


def git_commits(repo: str, max_commits: int = 200) -> list:
    """One record per commit: sha/author/date/subject/body + changed files.
    Oldest-first so a bounded ingest keeps the start of history."""
    _git(repo, "rev-parse", "--git-dir")  # fail fast: not a repo
    log = _git(repo, "log", "--reverse", "--max-count=%d" % max_commits,
               "--date=iso-strict",
               "--pretty=format:%H%x1f%an%x1f%ae%x1f%ad%x1f%s%x1f%b%x1e")
    files = _git(repo, "log", "--reverse", "--max-count=%d" % max_commits,
                 "--name-only", "--pretty=format:%x1e%H")
    by_sha = {}
    for chunk in files.split("\x1e"):
        lines = [l for l in chunk.strip("\n").splitlines() if l.strip()]
        if lines:
            by_sha.setdefault(lines[0].strip(), []).extend(lines[1:])
    commits = []
    for rec in log.split("\x1e"):
        f = rec.strip("\n").split("\x1f")
        if len(f) < 6:
            continue
        sha, name, email, date, subject, body = (s.strip() for s in f[:6])
        commits.append({"sha": sha, "name": name, "email": email,
                        "date": date, "subject": subject, "body": body,
                        "files": by_sha.get(sha, [])})
    return commits


def ingest_git(wiki, repo: str, max_commits: int = 200, force: bool = False) -> dict:
    from . import sources
    repo = os.path.abspath(str(repo))
    out = {"connector": "git", "repo": repo, "items": 0, "ingested": 0,
           "deduplicated": 0, "sources": []}
    for c in git_commits(repo, max_commits):
        doc = "# git commit %s\n\n- author: %s <%s>\n- date: %s\n\n## subject\n\n%s\n" % (
            c["sha"], c["name"], c["email"], c["date"], c["subject"])
        if c["body"]:
            doc += "\n## message\n\n%s\n" % c["body"]
        if c["files"]:
            doc += "\n## changed files\n\n%s\n" % "\n".join(
                "- " + f for f in c["files"])
        r = sources.ingest(wiki, "git_commit", "git:%s@%s" % (repo, c["sha"]),
                           doc.encode("utf-8"), normalized_content=doc,
                           parser_name=PARSER["name"],
                           root_origin="git:%s" % repo, force=force)
        out["items"] += 1
        out["deduplicated"] += 1 if r.get("deduplicated") else 0
        out["ingested"] += 0 if r.get("deduplicated") else 1
        out["sources"].append(r["source_id"])
    return out


# --------------------------------------------------------------- feeds ----

class _Strip(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts = []

    def handle_data(self, d):
        self.parts.append(d)


def _text(el) -> str:
    if el is None:
        return ""
    s = _Strip()
    s.feed(el.text or "")
    s.close()
    return re.sub(r"\s+", " ", "".join(s.parts)).strip()


def _tag(el, name):
    for child in el:
        if child.tag == name or child.tag.endswith("}" + name):
            return child
    return None


def feed_items(data: bytes, limit: int = 50) -> list:
    """Parse RSS 2.0 / RDF / Atom. Raises TxnError on malformed XML."""
    try:
        root = ET.fromstring(data)
    except ET.ParseError as e:
        raise TxnError("feed connector: malformed XML: %s" % e)
    ns = "{http://www.w3.org/2005/Atom}"
    if root.tag == "rss" or root.tag.endswith("}rss"):
        chan = _tag(root, "channel")
        items = [el for el in (chan or root) if el.tag == "item"]
    elif root.tag == ns + "feed":
        items = [el for el in root if el.tag == ns + "entry"]
    elif root.tag.endswith("}RDF"):
        items = [el for el in root.iter() if el.tag.endswith("}item")]
    else:
        raise TxnError("feed connector: not an RSS/Atom feed (root %r)" % root.tag)
    out = []
    for it in items[:limit]:
        guid = _text(_tag(it, "guid")) or _text(_tag(it, "id"))
        link = _text(_tag(it, "link"))
        if not link:
            lh = it.find(ns + "link")
            link = (lh.get("href") or "") if lh is not None else ""
        out.append({
            "title": _text(_tag(it, "title")),
            "link": link,
            "guid": guid or link,
            "updated": _text(_tag(it, "updated")) or _text(_tag(it, "pubDate"))
                       or _text(_tag(it, "published")),
            "content": _text(_tag(it, "description")) or _text(_tag(it, "summary"))
                       or _text(_tag(it, "content")),
        })
    return out


def ingest_feed(wiki, ref: str, data: bytes, limit: int = 50,
                force: bool = False) -> dict:
    from . import sources
    out = {"connector": "feed", "feed": ref, "items": 0, "ingested": 0,
           "deduplicated": 0, "sources": [], "warnings": 0}
    for it in feed_items(data, limit):
        doc = "# %s\n\n- link: %s\n- updated: %s\n\n%s\n" % (
            it["title"] or it["guid"], it["link"], it["updated"], it["content"])
        r = sources.ingest(wiki, "feed_item",
                           "feed:%s#%s" % (ref, it["guid"] or it["title"]),
                           doc.encode("utf-8"), normalized_content=doc,
                           parser_name=PARSER["name"],
                           root_origin="feed:%s" % ref, force=force)
        out["items"] += 1
        out["deduplicated"] += 1 if r.get("deduplicated") else 0
        out["ingested"] += 0 if r.get("deduplicated") else 1
        out["warnings"] += len(r.get("warnings") or [])
        out["sources"].append(r["source_id"])
    return out

# ------------------------------------------------------------- presets ----
#
# SP3-T4: declarative JSON-API connectors. A preset names where the item
# array lives in the response body, how to map each item to a candidate
# shaped record (subject/predicate/value + the raw text carried via an
# s: locator, never a dead `quote` field), and which env var — if any —
# supplies a bearer token. Token values never enter the result dict,
# stored content, or raised messages: they exist only in the headers
# passed to `fetch`.


def _map_github_issue(item: dict) -> dict:
    """GitHub issue JSON -> candidate-shaped record. Pure; no I/O."""
    body = str(item.get("body") or "").strip()
    cand = {"subject": "issue #%s: %s" % (item.get("number"),
                                          item.get("title")),
            "predicate": "state",
            "value": str(item.get("state") or "")}
    if body:
        cand["locator"] = "s:" + body  # body preserved as verbatim evidence
    return cand


def _map_rest_item(item: dict) -> dict:
    """Generic REST item {id,title,body} -> candidate-shaped record."""
    body = str(item.get("body") or "").strip()
    cand = {"subject": str(item.get("title") or ""),
            "predicate": "item",
            "value": str(item.get("id"))}
    if body:
        cand["locator"] = "s:" + body
    return cand


def _origin(url: str):
    """(scheme, hostname, port) with the scheme's default port filled in."""
    p = urllib.parse.urlsplit(url)
    port = p.port
    if port is None:
        port = {"http": 80, "https": 443}.get(p.scheme.lower())
    return (p.scheme.lower(), (p.hostname or "").lower(), port)



PRESETS = {
    "github-issues": {
        "env": "GITHUB_TOKEN",
        "items": None,               # response is a top-level array
        "required": ("number", "title", "state"),
        "map": _map_github_issue,
        "accept": "application/vnd.github+json",
        # /issues returns PRs too — items with a 'pull_request' key
        "skip": lambda i: "pull_request" in i,
        # bearer only to GitHub API / GHE hosts, never off-origin
        "auth_hosts": (".github.com", ".ghe.com"),
    },
    "rest": {
        "env": None,                 # generic JSON endpoint, no auth
        "items": "items",            # response is {"items": [...]}
        "required": ("title", "body", "id"),
        "map": _map_rest_item,
        "accept": "application/json",
    },
}


def list_presets() -> list:
    return sorted(PRESETS)


class _HttpOnlyRedirect(urllib.request.HTTPRedirectHandler):
    """Same guard as ingest-url: refuse redirects off http/https."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme.lower() not in ("http", "https"):
            raise TxnError("preset fetch: refusing redirect to %r" % newurl)
        if _origin(req.full_url) != _origin(newurl):
            # credentials must not cross origins
            for k in [k for k in req.headers
                      if k.lower() == "authorization"]:
                del req.headers[k]
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _default_fetch(url: str, headers: dict) -> bytes:
    if urllib.parse.urlsplit(url).scheme.lower() not in ("http", "https"):
        raise TxnError("preset fetch: only http/https refs allowed (got %r)"
                       % url)
    req = urllib.request.Request(url, headers=headers)
    opener = urllib.request.build_opener(_HttpOnlyRedirect())
    with opener.open(req, timeout=60) as resp:
        return resp.read(50 * 1024 * 1024)


def _preset_headers(cfg: dict, ref: str) -> dict:
    """Headers handed to fetch. The token lives only in the returned dict —
    never in any string produced by ingest_preset itself. When the preset
    declares ``auth_hosts``, the bearer is attached only to refs whose
    hostname ends in one of those suffixes."""
    headers = {"Accept": cfg.get("accept") or "application/json",
               "User-Agent": "wikicore-connector/%s" % PARSER["version"]}
    env = cfg.get("env")
    if env:
        token = os.environ.get(env)
        allowed = cfg.get("auth_hosts")
        host = (urllib.parse.urlsplit(str(ref)).hostname or "").lower()
        if token and (allowed is None
                      or any(host.endswith(s) for s in allowed)):
            headers["Authorization"] = "Bearer %s" % token
    return headers


def _preset_doc(item_id: str, cand: dict, item: dict) -> str:
    body = str(item.get("body") or "").strip()
    doc = "# %s\n\n- preset item id: %s\n- predicate: %s\n- value: %s\n" % (
        cand["subject"], item_id, cand["predicate"], cand["value"])
    if body:
        doc += "\n## body\n\n%s\n" % body
    return doc


def ingest_preset(wiki, preset: str, ref: str, limit: int = 50,
                  force: bool = False, fetch=None) -> dict:
    """Fetch a preset JSON endpoint; ingest each item as a source.

    Same dedup/secret/injection path as ingest_git/ingest_feed — items go
    through sources.ingest as kind ``feed_item`` keyed on
    ``preset:<preset>:<ref>#<item-id>`` so re-runs dedup per item.
    `fetch` is callable(url, headers) -> bytes; the default is urllib.
    `skipped` counts malformed items plus deduplicated re-runs; the
    returned token-free dict carries source_id/version of the first item
    (None when nothing was ingested) plus the usual connector tallies.
    """
    from . import sources
    cfg = PRESETS.get(preset)
    if cfg is None:
        raise TxnError("unknown connector preset: %r — valid: %s"
                       % (preset, ", ".join(list_presets())))
    fetch = fetch or _default_fetch
    try:
        raw = fetch(str(ref), _preset_headers(cfg, ref))
        data = json.loads(raw.decode("utf-8") if isinstance(raw, bytes) else raw)
    except TxnError:
        raise
    except Exception as e:
        raise TxnError("preset %r fetch/parse failed for %r: %s"
                       % (preset, ref, e))
    if cfg["items"]:
        if not isinstance(data, dict):
            raise TxnError(
                "preset %r: expected JSON object with %r key in %r"
                % (preset, cfg["items"], ref))
        items = data.get(cfg["items"], [])
    else:
        items = data
    if not isinstance(items, list):
        raise TxnError("preset %r: expected an item list at %r in %r"
                       % (preset, cfg["items"] or "<root>", ref))
    out = {"connector": "preset", "preset": preset, "ref": ref,
           "items": 0, "ingested": 0, "deduplicated": 0, "warnings": 0,
           "source_id": None, "version": None, "sources": [],
           "new_in_store": 0, "skipped": 0}
    for item in items[:limit]:
        out["items"] += 1
        if not isinstance(item, dict):
            out["skipped"] += 1
            continue
        skip = cfg.get("skip")
        if (skip is not None and skip(item)
                or any(item.get(k) is None for k in cfg["required"])):
            out["skipped"] += 1
            continue
        cand = cfg["map"](item)
        item_id = item.get("id") or item.get("number")
        if not item_id:
            raise TxnError("preset %r: item in %r has no usable id/number"
                           % (preset, ref))
        doc = _preset_doc(item_id, cand, item)
        r = sources.ingest(
            wiki, "feed_item",
            "preset:%s:%s#%s" % (preset, ref, item_id),
            doc.encode("utf-8"), normalized_content=doc,
            parser_name=PARSER["name"],
            root_origin="preset:%s:%s" % (preset, ref), force=force)
        out["sources"].append(r["source_id"])
        if out["source_id"] is None:
            out["source_id"], out["version"] = r["source_id"], r["version"]
        if r.get("deduplicated"):
            out["deduplicated"] += 1
            out["skipped"] += 1
        else:
            out["ingested"] += 1
            out["new_in_store"] += 1
        out["warnings"] += len(r.get("warnings") or [])
    return out
