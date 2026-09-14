"""Minimal JSON Schema (draft-07 subset) validator — stdlib only.

Supported keywords: type, required, properties, additionalProperties, enum,
const, items, pattern, minimum, maximum, minItems, uniqueItems.
`validate` returns a list of human-readable error strings (empty == valid).
"""


def _type_ok(value, expected) -> bool:
    if isinstance(expected, list):
        return any(_type_ok(value, t) for t in expected)
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "null":
        return value is None
    return True


def _check(value, prop_schema, path, errors):
    where = path or "root"

    if "const" in prop_schema and value != prop_schema["const"]:
        errors.append("%s: must equal %r" % (where, prop_schema["const"]))
        return

    if "enum" in prop_schema and value not in prop_schema["enum"]:
        errors.append("%s: %r not in enum %r" % (where, value, prop_schema["enum"]))
        return

    if "type" in prop_schema and not _type_ok(value, prop_schema["type"]):
        errors.append("%s: expected type %r" % (where, prop_schema["type"]))
        return

    if isinstance(value, str) and "pattern" in prop_schema:
        import re

        if not re.search(prop_schema["pattern"], value):
            errors.append("%s: %r fails pattern %r" % (where, value, prop_schema["pattern"]))

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in prop_schema and value < prop_schema["minimum"]:
            errors.append("%s: %r < minimum %r" % (where, value, prop_schema["minimum"]))
        if "maximum" in prop_schema and value > prop_schema["maximum"]:
            errors.append("%s: %r > maximum %r" % (where, value, prop_schema["maximum"]))

    if isinstance(value, list):
        if "minItems" in prop_schema and len(value) < prop_schema["minItems"]:
            errors.append("%s: needs >= %d items" % (where, prop_schema["minItems"]))
        if prop_schema.get("uniqueItems"):
            seen = [json_key(item) for item in value]
            if len(set(seen)) != len(seen):
                errors.append("%s: items not unique" % where)
        item_schema = prop_schema.get("items")
        if item_schema:
            for i, item in enumerate(value):
                _check(item, item_schema, "%s[%d]" % (where, i), errors)

    if isinstance(value, dict):
        for req in prop_schema.get("required", []):
            if req not in value:
                errors.append("%s: missing required property %r" % (where, req))
        props = prop_schema.get("properties", {})
        for key, sub in value.items():
            if key in props:
                _check(sub, props[key], "%s.%s" % (where, key), errors)
            elif props and prop_schema.get("additionalProperties") is False:
                errors.append("%s: unexpected property %r" % (where, key))


def json_key(item):
    import json

    return json.dumps(item, sort_keys=True, default=str)


def validate(obj, schema: dict) -> list:
    errors = []
    _check(obj, schema, "", errors)
    return errors
