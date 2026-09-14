"""Dependency graph between canonical knowledge and derived artifacts.

Every derived artifact records what it was built from (PRD §17); when a
dependency changes, only its descendants are marked stale — never a global
rebuild (PRD §15). Deps are "<id>@<version>"; invalidation matches ANY
version of a changed id.
"""
from .transaction import Transaction

DEPS_FILE = ".state/dependencies.json"


def register(txn: Transaction, artifact: str, deps_list: list) -> None:
    def _upd(data):
        edges = data.setdefault("edges", {})
        existing = edges.get(artifact, [])
        merged = list(existing)
        for d in deps_list:
            if d not in merged:
                merged.append(d)
        edges[artifact] = merged

    txn.stage_state(DEPS_FILE, _upd)


def graph(wiki) -> dict:
    return wiki.load_json(DEPS_FILE)


def invalidate(wiki, changed_ids: list) -> list:
    """Mark artifacts depending (directly or transitively) on changed ids stale.

    Returns the list of newly stale artifacts. Persists to dependencies.json
    via its own internal transaction.
    """
    g = graph(wiki)
    edges = g.get("edges", {})
    stale = set(g.get("stale", []))

    def depends_on(dep: str) -> bool:
        dep_id = dep.split("@", 1)[0]
        return dep_id in changed_ids

    # BFS: seeds = artifacts directly depending on changed ids
    frontier = [a for a, ds in edges.items() if any(depends_on(d) for d in ds)]
    newly = []
    while frontier:
        artifact = frontier.pop()
        if artifact in stale:
            continue
        stale.add(artifact)
        newly.append(artifact)
        # transitively: artifacts that depend on this artifact (as a dep string)
        for other, ds in edges.items():
            if other not in stale and any(d.split("@", 1)[0] == artifact for d in ds):
                frontier.append(other)

    if newly:
        txn = Transaction(wiki, "wiki-internal-invalidate")

        def _upd(data):
            data["stale"] = sorted(stale)

        txn.stage_state(DEPS_FILE, _upd)
        txn.commit(wiki.revision())
    return sorted(newly)


def is_stale(wiki, artifact: str) -> bool:
    return artifact in graph(wiki).get("stale", [])


def stale_artifacts(wiki) -> list:
    return list(graph(wiki).get("stale", []))


def clear_stale(txn: Transaction, artifacts: list) -> None:
    def _upd(data):
        data["stale"] = [a for a in data.get("stale", []) if a not in artifacts]

    txn.stage_state(DEPS_FILE, _upd)
