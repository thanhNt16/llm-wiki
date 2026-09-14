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
`decisions/`; everything under `wiki/`, `context/`, `views/` is derived and
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
        with open(self.p("wiki.json"), "w") as f:
            json.dump(cfg, f, indent=2, ensure_ascii=False)
            f.write("\n")

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
            for line in f:
                line = line.strip()
                if line:
                    out.append(json.loads(line))
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
    cfg = json.loads(json.dumps(DEFAULT_CONFIG))
    cfg["project"] = name
    wiki.save_config(cfg)
    with open(wiki.p("README.md"), "w") as f:
        f.write(README)
    for rel, body in [
        (".state/aliases.json", {}),
        (".state/dependencies.json", {"edges": {}, "stale": []}),
        (".state/graph.json", {"relations": []}),
    ]:
        with open(wiki.p(rel), "w") as f:
            json.dump(body, f, indent=2)
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
    return {"initialized": True, "revision": 0, "root": wiki.root}
