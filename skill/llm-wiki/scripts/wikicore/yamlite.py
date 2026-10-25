"""Restricted YAML frontmatter writer/reader for DERIVED markdown pages only.

Supports flat scalars (str/int/float/bool/None) and lists of scalars.
Canonical data never passes through here — canonical storage is JSON.
"""
import re

from .transaction import TxnError

# Frontmatter fences must be lines containing only "---": a "---" mid-line
# (e.g. inside a scalar value like `title: a---b`) never opens/closes a block.
_FENCE_RE = re.compile(r"\A---\s*\n(.*?)^---[ \t]*\s*\Z", re.S | re.M)
_QUOTE_IF = (": ", " #", "'")
# Scalars that would re-parse as a non-string (int/float/bool/None/list) must
# be emitted quoted so the string value survives the round trip.
_NONSTRING_SCALAR_RE = re.compile(
    r"^(?:null|true|false|-?\d+|-?\d+\.\d+|\[\]|\[''\])$")
_UNESCAPE = {"n": "\n", "r": "\r", "\\": "\\"}


def _escape(text: str) -> str:
    return (text.replace("\\", "\\\\").replace("'", "''")
            .replace("\n", "\\n").replace("\r", "\\r"))


def _unescape(text: str) -> str:
    out = []
    i = 0
    while i < len(text):
        ch = text[i]
        if ch == "'" and text[i + 1:i + 2] == "'":
            out.append("'")
            i += 2
        elif ch == "\\" and i + 1 < len(text):
            out.append(_UNESCAPE.get(text[i + 1], ch + text[i + 1]))
            i += 2
        else:
            out.append(ch)
            i += 1
    return "".join(out)


def _quote(value: str) -> str:
    needs = (
        value == ""
        or value.startswith((" ", "'", "#"))
        or value.endswith(" ")
        or any(t in value for t in _QUOTE_IF)
        or "\\" in value or "\n" in value or "\r" in value
        or _NONSTRING_SCALAR_RE.match(value) is not None
    )
    if needs:
        return "'" + _escape(value) + "'"
    return value


def _dump_scalar(value) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return repr(value) if isinstance(value, float) else str(value)
    return _quote(str(value))


def dumps(data: dict) -> str:
    lines = []
    for key, value in data.items():
        if isinstance(value, dict):
            raise ValueError("yamlite: nested dicts not supported (key %r)" % key)
        if isinstance(value, list):
            if any(isinstance(item, (dict, list)) for item in value):
                raise ValueError("yamlite: nested lists not supported (key %r)" % key)
            if not value:
                lines.append("%s: []" % key)
            else:
                lines.append("%s:" % key)
                for item in value:
                    lines.append("  - " + _dump_scalar(item))
        else:
            lines.append("%s: %s" % (key, _dump_scalar(value)))
    return "\n".join(lines)


def _parse_scalar(text: str):
    if text == "null" or text == "":
        return None
    if text == "true":
        return True
    if text == "false":
        return False
    if text.startswith("'") and text.endswith("'") and len(text) >= 2:
        return _unescape(text[1:-1])
    if re.match(r"^-?\d+$", text):
        return int(text)
    if re.match(r"^-?\d+\.\d+$", text):
        return float(text)
    if text == "[]" or text == "['']" :
        return [] if text == "[]" else [""]
    return text


def loads(text: str) -> dict:
    m = _FENCE_RE.match(text.strip() + ("\n" if not text.endswith("\n") else ""))
    if not m:
        # No frontmatter fences: page simply has no frontmatter.
        return {}
    body = m.group(1)
    result = {}
    current_key = None
    for line in body.splitlines():
        if not line.strip() or line.strip().startswith("#"):
            continue
        if line.startswith("  - "):
            if current_key is None:
                raise TxnError("yamlite: list item without a key: %r" % line)
            if not isinstance(result.get(current_key), list):
                raise TxnError("yamlite: list item under scalar key %r: %r"
                               % (current_key, line))
            result[current_key].append(_parse_scalar(line[4:].strip()))
            continue
        if ":" not in line:
            raise TxnError("yamlite: cannot parse line: %r" % line)
        key, _, raw = line.partition(":")
        key = key.strip()
        raw = raw.strip()
        current_key = key
        if raw == "":
            result[key] = []
        elif raw == "[]":
            result[key] = []
        else:
            result[key] = _parse_scalar(raw)
            if isinstance(result[key], list):
                current_key = None
    return result
