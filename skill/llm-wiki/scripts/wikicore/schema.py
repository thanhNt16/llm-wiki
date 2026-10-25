"""Minimal JSON Schema (draft-07 subset) validator — stdlib only.

Supported keywords: type, required, properties, patternProperties,
additionalProperties, propertyNames, minProperties, maxProperties, enum,
const, items, pattern, minLength, maxLength, minimum, maximum, minItems,
uniqueItems. `validate` returns a list of human-readable error strings
(empty == valid).
"""
import re


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

    if isinstance(value, str):
        if "minLength" in prop_schema and len(value) < prop_schema["minLength"]:
            errors.append("%s: shorter than minLength %d"
                          % (where, prop_schema["minLength"]))
        if "maxLength" in prop_schema and len(value) > prop_schema["maxLength"]:
            errors.append("%s: longer than maxLength %d"
                          % (where, prop_schema["maxLength"]))
        if "pattern" in prop_schema and not re.search(prop_schema["pattern"], value):
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
        if "minProperties" in prop_schema and len(value) < prop_schema["minProperties"]:
            errors.append("%s: fewer than minProperties %d"
                          % (where, prop_schema["minProperties"]))
        if "maxProperties" in prop_schema and len(value) > prop_schema["maxProperties"]:
            errors.append("%s: more than maxProperties %d"
                          % (where, prop_schema["maxProperties"]))
        props = prop_schema.get("properties", {})
        patterns = prop_schema.get("patternProperties", {})
        name_schema = prop_schema.get("propertyNames")
        addl = prop_schema.get("additionalProperties")
        for key, sub in value.items():
            if name_schema is not None:
                _check(key, name_schema, "%s.<key %r>" % (where, key), errors)
            matched = key in props
            if matched:
                _check(sub, props[key], "%s.%s" % (where, key), errors)
            for pat, psub in patterns.items():
                if re.search(pat, key):
                    matched = True
                    _check(sub, psub, "%s.%s" % (where, key), errors)
            if matched:
                continue
            if addl is False:
                errors.append("%s: unexpected property %r" % (where, key))
            elif isinstance(addl, dict):
                _check(sub, addl, "%s.%s" % (where, key), errors)


def json_key(item):
    import json

    return json.dumps(item, sort_keys=True, default=str)


def validate(obj, schema: dict) -> list:
    errors = []
    _check(obj, schema, "", errors)
    return errors
