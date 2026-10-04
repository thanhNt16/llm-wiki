"""Token normalization for retrieval keyword matching."""
import re

_SPLIT_RE = re.compile(r"[^0-9a-z]+")
_SUFFIXES = ("tion", "ment", "ing", "ed", "ly", "s")


def norm_tokens(s: str) -> set:
    """Lowercase, split on non-alnum, drop tokens shorter than 3, then strip
    one conservative trailing suffix: 's' when the word is longer than 3;
    'ing'/'ed'/'ly'/'tion'/'ment' when the remaining stem is >= 4 chars."""
    out = set()
    for tok in _SPLIT_RE.split(s.lower()):
        if len(tok) < 3:
            continue
        for suf in _SUFFIXES:
            if tok.endswith(suf) and len(tok) - len(suf) >= (3 if suf == "s" else 4):
                tok = tok[: -len(suf)]
                break
        out.add(tok)
    return out
