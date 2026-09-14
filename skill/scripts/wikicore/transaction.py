"""PLAN -> STAGE -> VALIDATE -> COMMIT transaction model (PRD §38).

Canonical state is only ever touched by commit(), under a global semantic
commit lock (flock) with optimistic base_revision checking (PRD §40-42).
Failed commits leave staging behind for doctor inspection and never
partially apply (release gate: atomicity).
"""
import errno
import fcntl
import json
import os
import shutil
import time

from . import ids
from .hashing import canonical_json
from .schema import validate as schema_validate

SKILL_VERSION = "0.3.0"


class TxnError(Exception):
    """Validation/usage problem. CLI exit code 3."""


class ConflictError(TxnError):
    """Stale base_revision. CLI exit code 4."""


class LockedError(TxnError):
    """Commit lock held by another writer. CLI exit code 5."""


class Transaction:
    #: registry of fn(txn) -> None; raise TxnError to block commit
    VALIDATORS = []

    def __init__(self, wiki, command: str):
        self.wiki = wiki
        self.command = command
        self.run_id = ids.new("run")
        self.started_at = _now()
        self.staged = []  # ordered ops: ("write", rel, src_path_in_staging) | ("delete", rel, None)
        self._state_overlay = {}  # rel -> dict; composes repeated stage_state calls
        self.changes = {}
        self.warnings = []
        self.conflicts = []
        self.staging_dir = wiki.p(".state", "staging", self.run_id)
        os.makedirs(self.staging_dir, exist_ok=True)

    # ---- STAGE ----------------------------------------------------------
    def stage_write(self, rel: str, data) -> None:
        rel = _clean_rel(rel)
        dst = os.path.join(self.staging_dir, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        if isinstance(data, (dict, list)):
            text = json.dumps(data, indent=2, ensure_ascii=False) + "\n"
            with open(dst, "w", encoding="utf-8") as f:
                f.write(text)
        elif isinstance(data, bytes):
            with open(dst, "wb") as f:
                f.write(data)
        else:
            with open(dst, "w", encoding="utf-8") as f:
                f.write(str(data))
        self._replace_or_append(("write", rel, dst))
        self._state_overlay.pop(rel, None)

    def _replace_or_append(self, op) -> None:
        """Last write to the same rel wins (compose repeated stage_state calls)."""
        for i, existing in enumerate(self.staged):
            if existing[0] == op[0] and existing[1] == op[1]:
                self.staged[i] = op
                return
        self.staged.append(op)

    def stage_delete(self, rel: str) -> None:
        rel = _clean_rel(rel)
        self._replace_or_append(("delete", rel, None))
        self._state_overlay.pop(rel, None)

    def stage_state(self, rel: str, update_fn) -> dict:
        """Read a canonical JSON state file (or earlier staged version), apply
        update_fn(dict), stage the result. Repeated calls compose."""
        rel = _clean_rel(rel)
        if rel in self._state_overlay:
            data = self._state_overlay[rel]
        else:
            path = self.wiki.p(rel)
            if os.path.isfile(path):
                with open(path) as f:
                    data = json.load(f)
            else:
                data = {}
        update_fn(data)
        self._state_overlay[rel] = data
        dst = os.path.join(self.staging_dir, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with open(dst, "w", encoding="utf-8") as f:
            f.write(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
        self._replace_or_append(("write", rel, dst))
        return data

    # ---- VALIDATE -------------------------------------------------------
    def validate(self) -> None:
        for check in list(Transaction.VALIDATORS):
            check(self)
        _default_validator(self)

    # ---- COMMIT ---------------------------------------------------------
    def commit(self, base_revision: int) -> dict:
        self.validate()
        lock_path = self.wiki.p(".state", "locks", "commit.lock")
        os.makedirs(os.path.dirname(lock_path), exist_ok=True)
        lock_fd = open(lock_path, "w")
        try:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as e:
                if e.errno in (errno.EACCES, errno.EAGAIN):
                    raise LockedError("another wiki commit is in progress")
                raise
            current = self.wiki.revision()
            if current != base_revision:
                raise ConflictError(
                    "stale commit: base_revision=%d but wiki is at %d; re-read and rebase"
                    % (base_revision, current)
                )
            for op, rel, src in self.staged:
                dst = self.wiki.p(rel)
                if op == "write":
                    os.makedirs(os.path.dirname(dst), exist_ok=True)
                    os.replace(src, dst)
                elif op == "delete":
                    if os.path.exists(dst):
                        os.unlink(dst)
            receipt = {
                "run_id": self.run_id,
                "command": self.command,
                "skill_version": SKILL_VERSION,
                "started_at": self.started_at,
                "finished_at": _now(),
                "changes": self.changes,
                "warnings": self.warnings,
                "conflicts": self.conflicts,
                "revision": {"before": current, "after": current + 1},
            }
            errors = schema_validate(receipt, _run_receipt_schema())
            if errors:
                raise TxnError("run receipt invalid: %s" % "; ".join(errors))
            os.makedirs(self.wiki.p(".state", "operations"), exist_ok=True)
            with open(
                self.wiki.p(".state", "operations", "%s.receipt.json" % self.run_id), "w"
            ) as f:
                json.dump(receipt, f, indent=2, ensure_ascii=False)
                f.write("\n")
            with open(self.wiki.p(".state", "operations.jsonl"), "a") as f:
                f.write(canonical_json(receipt) + "\n")
            state = self.wiki.state()
            state["revision"] = current + 1
            state["content_hash"] = canonical_json([op[0] + ":" + op[1] for op in self.staged])[:64]
            self.wiki.save_state(state)
            shutil.rmtree(self.staging_dir, ignore_errors=True)
            return receipt
        finally:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_UN)
            except OSError:
                pass
            lock_fd.close()


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _clean_rel(rel: str) -> str:
    rel = rel.strip("/")
    parts = [p for p in rel.split("/") if p not in ("", ".")]
    if not parts or ".." in parts or any(p.startswith(".") for p in parts[1:]):
        raise TxnError("illegal staging path: %r" % rel)
    if parts[0].startswith(".") and parts[0] != ".state":
        raise TxnError("illegal staging path: %r" % rel)
    return "/".join(parts)


_RUN_RECEIPT_SCHEMA_PATH = os.path.join(
    os.path.dirname(__file__), "..", "..", "llm-wiki", "schemas", "run-receipt.schema.json"
)


def _run_receipt_schema() -> dict:
    with open(_RUN_RECEIPT_SCHEMA_PATH) as f:
        return json.load(f)


def _default_validator(txn: Transaction) -> None:
    """Staged canonical JSON must parse and match its schema when identifiable."""
    for op, rel, src in txn.staged:
        if op != "write" or not rel.endswith(".json"):
            continue
        with open(src) as f:
            try:
                data = json.load(f)
            except ValueError as e:
                raise TxnError("staged %s is not valid JSON: %s" % (rel, e))
        _check_object(rel, data)


def _check_object(rel: str, data) -> None:
    """Schema-enforce canonically-named files only (claims/claim_*, decisions/decision_*)."""
    base = os.path.basename(rel)
    schema_file = None
    if rel.startswith("claims/") and base.startswith("claim_"):
        schema_file = "claim.schema.json"
    elif rel.startswith("decisions/") and base.startswith("decision_"):
        schema_file = "decision.schema.json"
    if schema_file and isinstance(data, dict):
        with open(_RUN_RECEIPT_SCHEMA_PATH.replace("run-receipt.schema.json", schema_file)) as f:
            s = json.load(f)
        errors = schema_validate(data, s)
        if errors:
            raise TxnError("staged %s fails schema: %s" % (rel, "; ".join(errors)))
