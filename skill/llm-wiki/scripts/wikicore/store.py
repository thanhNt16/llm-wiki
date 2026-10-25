"""Wiki handle: path layout, config, state manifest."""
import json
import os

SKILL_VERSION = "0.3.0"

DEFAULT_CONFIG = {
    "acceptance_policy": {
        "auto_accept_authority": [
            "explicit_project_decision",
            "authoritative_source",
            "implementation_verification",
        ],
        "corroboration_auto_accept": 2,
    },
    "context_budget_default": 6000,
    "secret_policy": "warn",
    "allow_cross_project": False,
}

README = """# .llm-wiki

Evidence-backed project memory. Canonical data lives in `sources/`, `claims/`,
`decisions/`, `topics.json`; everything under `wiki/`, `context/`, `views/` is derived and
rebuildable. Do not edit canonical JSON by hand — use the llm-wiki skill
commands (wiki-init, wiki-ingest, wiki-compile, wiki-query, wiki-context,
wiki-review, wiki-doctor).
"""


class Wiki:
    SKILL_VERSION = SKILL_VERSION

    def __init__(self, root: str):
        self.root = os.path.abspath(root)
        self.dot = os.path.join(self.root, ".llm-wiki")

    def p(self, *parts) -> str:
        return os.path.join(self.dot, *parts)

    def exists(self) -> bool:
        return os.path.isfile(self.p("wiki.json"))

    def load_config(self) -> dict:
        cfg = json.loads(json.dumps(DEFAULT_CONFIG))  # deep copy of defaults
        with open(self.p("wiki.json")) as f:
            stored = json.load(f)
        cfg.update(stored)
        pol = dict(DEFAULT_CONFIG["acceptance_policy"])
        pol.update(stored.get("acceptance_policy", {}))
        cfg["acceptance_policy"] = pol
        return cfg

    def save_config(self, cfg: dict) -> None:
        tmp = self.p("wiki.json.tmp")
        with open(tmp, "w") as f:
            json.dump(cfg, f, indent=2, ensure_ascii=False)
            f.write("\n")
        os.replace(tmp, self.p("wiki.json"))

    def state(self) -> dict:
        with open(self.p(".state/manifest.json")) as f:
            return json.load(f)

    def save_state(self, s: dict) -> None:
        tmp = self.p(".state/manifest.json.tmp")
        with open(tmp, "w") as f:
            json.dump(s, f, indent=2, ensure_ascii=False)
            f.write("\n")
        os.replace(tmp, self.p(".state/manifest.json"))

    def revision(self) -> int:
        return self.state()["revision"]

    def content_hash(self) -> str:
        return self.state().get("content_hash", "")

    def load_json(self, rel: str):
        with open(self.p(rel)) as f:
            return json.load(f)

    def read_jsonl(self, rel: str) -> list:
        path = self.p(rel)
        if not os.path.isfile(path):
            return []
        out = []
        with open(path) as f:
            lines = f.read().splitlines()
        last = len(lines) - 1
        while last >= 0 and not lines[last].strip():
            last -= 1  # tolerate trailing blank lines
        for i, line in enumerate(lines):
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                if i == last:
                    continue  # crash-truncated trailing append; drop it
                raise
        return out


def init_wiki(wiki: Wiki, name: str) -> dict:
    """Idempotent, non-destructive scaffold of the .llm-wiki layout."""
    if wiki.exists():
        return {"initialized": False, "revision": wiki.revision(), "root": wiki.root}
    dirs = [
        "raw/files", "raw/urls", "raw/repositories", "raw/sessions", "raw/text",
        "sources", "claims", "decisions", "notes",
        "wiki/concepts", "wiki/entities", "wiki/decisions", "wiki/procedures",
        "wiki/questions", "wiki/changes",
        "context", "views", ".state/locks", ".state/operations", ".state/staging",
    ]
    for d in dirs:
        os.makedirs(wiki.p(d), exist_ok=True)
    # wiki.json is written LAST: exists() checks it, so a crash mid-init can
    # only leave a partial scaffold that a later `init` completes cleanly.
    with open(wiki.p("README.md"), "w") as f:
        f.write(README)
    for rel, body in [
        (".state/aliases.json", {}),
        (".state/dependencies.json", {"edges": {}, "stale": []}),
        (".state/graph.json", {"relations": []}),
    ]:
        tmp = wiki.p(rel + ".tmp")
        with open(tmp, "w") as f:
            json.dump(body, f, indent=2)
            f.write("\n")
        os.replace(tmp, wiki.p(rel))
    # Kind-routing scaffold: empty rules activate synthesize's built-in
    # heuristics and give agents a documented file to extend. Guarded — a
    # pre-existing file (e.g. init completing a partial scaffold) is kept.
    if not os.path.isfile(wiki.p("page-kinds.json")):
        with open(wiki.p("page-kinds.json"), "w") as f:
            json.dump(
                {"rules": [], "default": "concepts",
                 "_comment": "ordered rules; matchers: subject|prefix|suffix|regex|predicate; when.min_claims; empty rules -> built-in heuristics"},
                f, indent=2)
            f.write("\n")
    for rel in [".state/operations.jsonl", ".state/review-queue.jsonl"]:
        open(wiki.p(rel), "a").close()
    wiki.save_state(
        {
            "revision": 0,
            "skill_version": SKILL_VERSION,
            "content_hash": "",
            "compiled": {},
        }
    )
    cfg = json.loads(json.dumps(DEFAULT_CONFIG))
    cfg["project"] = name
    wiki.save_config(cfg)  # atomic; last write = exists() sentinel
    return {"initialized": True, "revision": 0, "root": wiki.root}
