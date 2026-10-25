"""Jev — advisory System One decision client (env-gated, stdlib only).

Jev is strictly advisory: when DECISION_TOKEN or DECISION_MODEL is unset the
client reports disabled and wired flags emit byte-identical output to today.
ask() never raises — transport or parse failures return ok:false so callers
degrade to absent decision fields instead of failing the command.

Wire shape (POST JSON): {model, state, questions}
  -> {"success": true,
      "result": {"model": str,
                 "answers": {q: {"type": "noul", "noul": <p(yes)>}
                              | {"type": "choice", "choice": {"pick": str, "probs": {opt: p}}}
                              | {"type": "score", "score": {"level": int, "expected": float}}},
                 "usage": {...}}}
Normalized answers always look like {"type": t, "value": v}.
"""
import json
import os
import urllib.error
import urllib.request

TOKEN_ENV = "DECISION_TOKEN"
MODEL_ENV = "DECISION_MODEL"
ENDPOINT_ENV = "DECISION_ENDPOINT"
DEFAULT_ENDPOINT = "http://localhost:20128/v1/systemone"
TIMEOUT_S = 10


def is_ready() -> bool:
    """True when both DECISION_TOKEN and DECISION_MODEL are set."""
    return bool(os.environ.get(TOKEN_ENV) and os.environ.get(MODEL_ENV))


def endpoint() -> str:
    return os.environ.get(ENDPOINT_ENV) or DEFAULT_ENDPOINT


def missing_env() -> list:
    """Unset required env vars, in canonical order (for jev --check)."""
    return [name for name in (TOKEN_ENV, MODEL_ENV) if not os.environ.get(name)]


def _post(payload: dict) -> dict:
    """One POST to the configured endpoint; returns parsed JSON body.
    Private so tests can stub the transport layer."""
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        endpoint(), data=body, method="POST",
        headers={"Authorization": "Bearer %s" % os.environ[TOKEN_ENV],
                 "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _fail(error, disabled=False):
    return {"ok": False, "disabled": disabled, "answers": {}, "usage": {},
            "error": error}


def _to_wire(questions: dict) -> dict:
    """Translate caller-friendly question shapes onto the System One wire:
    choice takes criteria {option: description} — an `options` list becomes
    {opt: opt}; score takes an ordered criteria list — `levels: N` becomes
    N generic level descriptions. Everything else passes through."""
    out = {}
    for name, q in (questions or {}).items():
        if not isinstance(q, dict):
            out[name] = q
            continue
        w = dict(q)
        opts = w.pop("options", None)
        levels = w.pop("levels", None)
        if opts is not None and "criteria" not in w:
            w["criteria"] = {str(o): str(o) for o in opts}
        elif levels is not None and "criteria" not in w:
            w["criteria"] = ["level %d" % i for i in range(int(levels))]
        out[name] = w
    return out

def _normalize(name: str, raw) -> dict:
    """Wire answer -> {"type": t, "value": v}. Official wire shapes:
    noul   {"type":"noul","noul":p}
    choice {"type":"choice","choice":"<pick>","confidence":c,
            "probabilities":{opt:p}}   (nested {pick,probs} also accepted)
    score  {"type":"score","score":s,"probabilities":{"<level>":p}}
    Unknown/malformed shapes keep their declared type with value None
    rather than raising."""
    if not isinstance(raw, dict):
        return {"type": "unknown", "value": None}
    t = raw.get("type")
    if t == "noul":
        v = raw.get("noul", raw.get("value"))
        try:
            v = float(v)
        except (TypeError, ValueError):
            v = None
        return {"type": "noul", "value": v}
    if t == "choice":
        ch = raw.get("choice")
        if isinstance(ch, dict):
            dist = ch.get("probs") or ch.get("probabilities") \
                or ch.get("distribution") or {}
            pick = ch.get("pick") or ch.get("answer") or ch.get("value")
        else:
            dist = raw.get("probabilities") or raw.get("probs") \
                or raw.get("distribution") or {}
            pick = ch if isinstance(ch, str) else raw.get("pick")
        if pick is None and dist:
            try:
                pick = max(dist, key=lambda k: dist[k])
            except (TypeError, ValueError):
                pick = None
        value = {"pick": pick, "distribution": dist}
        if raw.get("confidence") is not None:
            value["confidence"] = raw["confidence"]
        return {"type": "choice", "value": value}
    if t == "score":
        sc = raw.get("score")
        probs = raw.get("probabilities") or {}
        if isinstance(sc, dict):
            level, expected = sc.get("level"), sc.get("expected")
        else:
            level, expected = sc, None
            if probs:
                try:
                    expected = sum(float(k) * float(p)
                                   for k, p in probs.items())
                except (TypeError, ValueError):
                    expected = None
            if expected is None and sc is not None:
                try:
                    expected = float(sc)
                except (TypeError, ValueError):
                    expected = None
        try:
            level = int(round(float(level)))
        except (TypeError, ValueError):
            level = None
        try:
            expected = float(expected)
        except (TypeError, ValueError):
            expected = None
        return {"type": "score", "value": {"level": level,
                                           "expected": expected}}
    return {"type": t if isinstance(t, str) else "unknown", "value": None}


def ask(state: str, questions: dict) -> dict:
    """Ask Jev typed questions about `state`. Never raises.

    Returns {"ok", "answers": {q: {"type", "value"}}, "usage",
             "disabled", "error"}. Env unset -> ok:false + disabled:true."""
    if not is_ready():
        return _fail(None, disabled=True)
    payload = {"model": os.environ[MODEL_ENV], "state": state,
               "questions": _to_wire(questions)}
    try:
        body = _post(payload)
    except urllib.error.HTTPError as e:
        try:
            detail = e.read().decode("utf-8", "replace")[:400]
        except Exception:
            detail = ""
        return _fail("HTTP %s %s" % (e.code, detail or e.reason))
    except Exception as e:  # timeout, refused, bad JSON — all degrade silently
        return _fail("%s: %s" % (type(e).__name__, e))
    if not isinstance(body, dict) or body.get("success") is not True:
        detail = body.get("error") if isinstance(body, dict) else None
        return _fail(detail or "endpoint returned success != true")
    result = body.get("result") or {}
    raw_answers = result.get("answers") or {}
    answers = {}
    if isinstance(raw_answers, dict):
        for q, raw in raw_answers.items():
            answers[q] = _normalize(q, raw)
    return {"ok": True, "disabled": False, "answers": answers,
            "usage": result.get("usage") or {}, "error": None}
