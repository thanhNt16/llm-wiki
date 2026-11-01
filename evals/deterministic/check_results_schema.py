"""Validate evals/semantic/results.jsonl rows against results.schema.json."""
import json, re

_COMMIT_RE = re.compile(r"^[0-9a-f]{7,40}$")


def check_rows(schema_path, rows_path):
    with open(schema_path) as f:
        schema = json.load(f)
    required = set(schema.get("required", []))
    enum = set(schema["properties"]["scenario"]["enum"])
    errors = []
    with open(rows_path) as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as e:
                errors.append("line %d: invalid JSON: %s" % (n, e))
                continue
            if not isinstance(row, dict):
                errors.append("line %d: not a JSON object" % n)
                continue
            for k in sorted(required):
                if k not in row:
                    errors.append("line %d: missing required field %r" % (n, k))
            if "scenario" in row and row["scenario"] not in enum:
                errors.append("line %d: scenario %r not in enum" % (n, row["scenario"]))
            for k in ("timestamp", "answer", "rubric_notes"):
                if k in row and not isinstance(row[k], str):
                    errors.append("line %d: %s is not a string" % (n, k))
            if "engine_commit" in row and not (
                isinstance(row["engine_commit"], str)
                and _COMMIT_RE.match(row["engine_commit"])):
                errors.append("line %d: engine_commit %r not string hex 7-40" % (n, row["engine_commit"]))
            if "pass" in row and not isinstance(row["pass"], bool):
                errors.append("line %d: pass is not boolean" % n)
            if "citations" in row and not (
                isinstance(row["citations"], list)
                and all(isinstance(c, str) for c in row["citations"])):
                errors.append("line %d: citations not a string array" % n)
    return (not errors, errors)
