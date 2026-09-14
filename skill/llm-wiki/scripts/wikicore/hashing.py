"""Content hashing and canonical JSON serialization."""
import hashlib
import json


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def canonical_json(obj) -> str:
    """Deterministic serialization used for content hashes and diffing."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
