"""ULID-style identifiers: 48-bit ms timestamp + 80-bit randomness, Crockford base32."""
import re
import secrets
import time

CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
_ID_RE = re.compile(r"^[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$")


def _encode(value: int, length: int) -> str:
    out = []
    for _ in range(length):
        out.append(CROCKFORD[value & 0x1F])
        value >>= 5
    return "".join(reversed(out))


def new(kind: str) -> str:
    """Return a fresh id like `claim_01JABC...` (26 ULID chars)."""
    if not kind or not kind.replace("_", "").isalpha() or kind != kind.lower():
        raise ValueError("kind must be lowercase letters/underscores: %r" % kind)
    ts = int(time.time() * 1000) & ((1 << 48) - 1)
    rand = secrets.randbits(80)
    return "%s_%s" % (kind, _encode(ts, 10) + _encode(rand, 16))


def validate(s: str) -> bool:
    return isinstance(s, str) and bool(_ID_RE.match(s))
