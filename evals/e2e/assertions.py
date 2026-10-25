"""Deterministic e2e assertions for plan Task 12 / spec §6.

Every check is a pure filesystem or CLI probe — no LLM judging. Each
assert_sN(project_root, transcript_path) returns {"pass": bool, "detail": str}.

Transcript helpers (assistant_text / shell_commands / token_estimate) are also
consumed by run_e2e.py for the commands_used / tokens_estimate JSONL columns.

Scenarios (spec §6 scenario list):
  S1 init→compile→cited answer   S2 near-dup→extend/allow-new
  S3 triage→session-kind ingest  S4 correction→flip-with-history
  S5 refusal on unanswerable     S6 query ladder (no grep/rg)
  S7 multi-part llm-query        S8 graph-data -> graph.html viz
"""
import glob
import json
import os
import re
import subprocess
import sys

_THIS_DIR = os.path.dirname(os.path.abspath(__file__))
_REPO_ROOT = os.path.abspath(os.path.join(_THIS_DIR, "..", ".."))

_ID_TOKEN_RE = re.compile(r"\b(?:claim|source|decision)_[0-9A-HJKMNP-TV-Z]{26}\b")
_GREP_RE = re.compile(r"(?<![\w./-])(grep|rg)(?=\s|$)")
_REFUSAL_TOKENS = (
    "cannot answer", "can't answer", "no evidence", "insufficient evidence",
    "do not know", "don't know", "no information", "nothing in the wiki",
    "not in the wiki", "unable to answer", "cannot determine",
    "no project evidence", "unanswerable", "cannot find evidence",
    "no basis", "not documented", "cannot be answered",
)


def engine_path() -> str:
    """Resolve the engine under test: WIKI_E2E_ENGINE env > repo-local HEAD >
    installed skill copies. Assertions shell out to this for `verify`."""
    env = os.environ.get("WIKI_E2E_ENGINE")
    candidates = [
        env,
        os.path.join(_REPO_ROOT, "skill", "llm-wiki", "scripts", "wiki.py"),
        os.path.expanduser("~/.omp/agent/skills/llm-wiki/scripts/wiki.py"),
        os.path.expanduser("~/.agents/skills/llm-wiki/scripts/wiki.py"),
        os.path.expanduser("~/.cellockai/skills/llm-wiki/scripts/wiki.py"),
    ]
    for cand in candidates:
        if cand and os.path.isfile(cand):
            return cand
    return ""


def _engine_has_triage() -> bool:
    """True when the resolved engine exposes a `triage` subcommand. Absent on
    this HEAD → S3 marks that sub-check unsupported instead of hard-failing."""
    wiki = engine_path()
    if not wiki:
        return False
    try:
        proc = subprocess.run([sys.executable, wiki, "--help"],
                              capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return re.search(r"\btriage\b", proc.stdout) is not None


# ---------------------------------------------------------------- transcripts

def _json_docs(transcript_path):
    """Yield dicts: whole-file JSON first, else NDJSON line by line."""
    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as f:
            raw = f.read()
    except OSError:
        return
    raw = raw.strip()
    if not raw:
        return
    try:
        doc = json.loads(raw)
        if isinstance(doc, dict):
            yield doc
            return
        if isinstance(doc, list):
            for d in doc:
                if isinstance(d, dict):
                    yield d
            return
    except ValueError:
        pass
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            doc = json.loads(line)
        except ValueError:
            continue
        if isinstance(doc, dict):
            yield doc


def _walk(node):
    """Yield every dict and list in a parsed JSON tree."""
    if isinstance(node, dict):
        yield node
        for v in node.values():
            yield from _walk(v)
    elif isinstance(node, list):
        for v in node:
            yield from _walk(v)


def _tool_name(d):
    for k in ("name", "tool", "tool_name", "toolName"):
        v = d.get(k)
        if isinstance(v, str):
            return v
    return ""


def _command_from(d, sink):
    """Extract a shell command string from a tool-call-shaped dict."""
    for key in ("input", "arguments", "args", "params", "call"):
        sub = d.get(key)
        if isinstance(sub, str):
            try:
                sub = json.loads(sub)
            except ValueError:
                if _looks_shell(sub):
                    sink.append(sub)
                continue
        if isinstance(sub, dict):
            for ck in ("command", "cmd", "script"):
                v = sub.get(ck)
                if isinstance(v, str) and v.strip():
                    sink.append(v)
                    return


def _looks_shell(s):
    return bool(re.match(
        r"^\s*(cd\b|ls\b|cat\b|python[0-9.]*\b|bash\b|sh\b|grep\b|rg\b|"
        r"find\b|echo\b|mkdir\b|git\b|\./|~/)", s))


def shell_commands(transcript_path):
    """Ordered shell commands the runtime executed, best-effort.

    omp --mode json emits tool_call/tool_result NDJSON events; claude
    --output-format stream-json emits assistant messages carrying
    tool_use {name: Bash, input: {command}} blocks. Both reduce to dicts
    whose name hints a shell and that carry a command string."""
    commands = []
    for doc in _json_docs(transcript_path):
        for d in _walk(doc):
            name = _tool_name(d).lower()
            is_toolish = any(k in d for k in
                             ("input", "arguments", "args", "params", "call"))
            if is_toolish and (
                    re.search(r"bash|shell|terminal|sh\b|exec|process|cmd", name)
                    or not name):
                _command_from(d, commands)
    if not commands:
        # last-resort: plain-text transcript — grab obvious command literals
        try:
            with open(transcript_path, encoding="utf-8",
                      errors="replace") as f:
                raw = f.read()
        except OSError:
            return commands
        for m in re.finditer(r'"(?:command|cmd)"\s*:\s*"((?:[^"\\]|\\.)*)"',
                             raw):
            try:
                commands.append(json.loads('"%s"' % m.group(1)))
            except ValueError:
                commands.append(m.group(1))
    return commands


def assistant_text(transcript_path):
    """All assistant-visible text (omp text blocks, claude text blocks and the
    final `result` field), joined. Used for citation/refusal assertions."""
    parts = []
    for doc in _json_docs(transcript_path):
        for d in _walk(doc):
            if d.get("type") == "text" and isinstance(d.get("text"), str):
                parts.append(d["text"])
            elif d.get("type") == "result" and isinstance(d.get("result"), str):
                parts.append(d["result"])
    if parts:
        return "\n".join(parts)
    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def token_estimate(transcript_path):
    """Sum distinct usage records (deduped — NDJSON repeats full usage per
    message_start/message_end/agent_end)."""
    seen, total = set(), 0
    for doc in _json_docs(transcript_path):
        for d in _walk(doc):
            usage = None
            for k in ("usage", "total_usage"):
                if isinstance(d.get(k), dict):
                    usage = d[k]
                    break
            if usage is None and isinstance(d.get("input"), (int, float)):
                usage = d
            if not isinstance(usage, dict):
                continue
            key = json.dumps(usage, sort_keys=True, default=str)
            if key in seen:
                continue
            seen.add(key)
            n = usage.get("totalTokens") or usage.get("total_tokens")
            if not isinstance(n, (int, float)):
                n = sum(x for x in (
                    usage.get("input"), usage.get("output"),
                    usage.get("input_tokens"), usage.get("output_tokens"))
                    if isinstance(x, (int, float)))
            total += int(n or 0)
    return total


# ------------------------------------------------------------------- fixtures

def _claims(project_root):
    out = []
    for path in sorted(glob.glob(os.path.join(
            project_root, ".llm-wiki", "claims", "*.json"))):
        try:
            with open(path) as f:
                out.append(json.load(f))
        except (OSError, ValueError):
            continue
    return out


def _source_manifests(project_root):
    out = []
    for path in sorted(glob.glob(os.path.join(
            project_root, ".llm-wiki", "sources", "*", "manifest.json"))):
        try:
            with open(path) as f:
                out.append(json.load(f))
        except (OSError, ValueError):
            continue
    return out

def _forced_new(project_root, rel):
    """True when a page file carries frontmatter forced_new: true — the
    engine's marker that --allow-new was passed deliberately past the
    near-duplicate guard."""
    try:
        with open(os.path.join(project_root, ".llm-wiki", "wiki", rel)) as f:
            head = f.read(2000)
    except OSError:
        return False
    return re.search(r"^forced_new:\s*true\s*$", head, re.M) is not None


def _page_slugs(project_root):
    base = os.path.join(project_root, ".llm-wiki", "wiki")
    out = []
    for path in glob.glob(os.path.join(base, "*", "*.md")):
        rel = os.path.relpath(path, base).replace(os.sep, "/")
        if not rel.endswith("/index.md"):
            out.append(rel)
    return sorted(out)


def _verify(project_root):
    """Run `wiki.py verify`; returns (exit_ok, detail)."""
    wiki = engine_path()
    if not wiki:
        return False, "no engine found for verify"
    try:
        proc = subprocess.run(
            [sys.executable, wiki, "verify"], cwd=project_root,
            capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, "verify invocation failed: %s" % e
    if proc.returncode != 0:
        return False, "verify exit=%d: %s" % (
            proc.returncode, (proc.stdout + proc.stderr).strip()[:200])
    return True, "verify ok"


# ---------------------------------------------------------------- assertions

def assert_s1(project_root, transcript_path):
    """init→compile→cited answer: wiki artifacts exist, `verify` passes, and
    the agent's answer cites at least one id that resolves to a real file."""
    store = os.path.join(project_root, ".llm-wiki")
    if not os.path.isdir(store):
        return {"pass": False, "detail": ".llm-wiki/ missing — init never ran"}
    pages = _page_slugs(project_root)
    if not pages:
        return {"pass": False, "detail": "no wiki/<kind>/*.md artifacts built"}
    claims = _claims(project_root)
    if not claims:
        return {"pass": False, "detail": "no claims/*.json — compile produced nothing"}
    ok, why = _verify(project_root)
    if not ok:
        return {"pass": False, "detail": why}
    text = assistant_text(transcript_path)
    cited = [t for t in _ID_TOKEN_RE.findall(text)
             if glob.glob(os.path.join(store, "*", t + "*.json"))
             or glob.glob(os.path.join(store, "*", t, "manifest.json"))]
    if not cited:
        return {"pass": False,
                "detail": "verify ok but no claim_/source_/decision_ citation in output resolves"}
    return {"pass": True,
            "detail": "artifacts+%d claims, verify ok, citation %s resolves"
                      % (len(claims), cited[0])}


def assert_s2(project_root, transcript_path):
    """Near-dup: the wiki was seeded with wiki/concepts/retry-policy.md and the
    agent was asked for 'Retry Policy Notes'. The engine's sanctioned ways out
    of the near-duplicate block are extend-first or a deliberate --allow-new
    (which stamps forced_new: true on the page). Pass iff the transcript shows
    the handled path AND any second page provably went through --allow-new —
    a second page created silently (no forced_new marker) is a fail."""
    text = assistant_text(transcript_path).lower()
    handled = ("extend" in text or "--allow-new" in text
               or "allow_new" in text or "near-duplicate" in text
               or "near duplicate" in text)
    extra = [p for p in _page_slugs(project_root)
             if "retry" in os.path.basename(p)
             and "polic" in os.path.basename(p)
             and os.path.basename(p) != "retry-policy.md"]
    if not handled:
        return {"pass": False,
                "detail": "output never mentions extend/--allow-new/"
                          "near-duplicate — guard path unclear"}
    if extra and not all(_forced_new(project_root, p) for p in extra):
        return {"pass": False,
                "detail": "second page created without forced_new marker "
                          "(silent dup): %s" % ", ".join(extra)}
    if extra:
        return {"pass": True,
                "detail": "guard fired; deliberate --allow-new path recorded "
                          "(forced_new) on %s" % ", ".join(extra)}
    return {"pass": True,
            "detail": "no second page; output shows extend/allow-new handling"}


def assert_s3(project_root, transcript_path):
    """Triage: a manifest exists AND at least one source was ingested with
    kind=session. If the engine lacks `triage`/--kind session, the affected
    sub-check is reported unsupported in detail rather than hard-failing."""
    unsupported = []
    triage_supported = _engine_has_triage()

    manifest = None
    engine_manifests = glob.glob(os.path.join(
        project_root, ".llm-wiki", "triage*", "*.json")) + glob.glob(
        os.path.join(project_root, ".llm-wiki", "triage*.json"))
    agent_manifest = os.path.join(project_root, "inbox-triage.json")
    if engine_manifests:
        manifest = engine_manifests[0]
    elif os.path.isfile(agent_manifest):
        manifest = agent_manifest
    if manifest is None:
        if not triage_supported:
            unsupported.append("engine triage subcommand")
        else:
            return {"pass": False,
                    "detail": "no triage manifest under .llm-wiki/ and no "
                              "inbox-triage.json at project root"}
    else:
        try:
            with open(manifest) as f:
                data = json.load(f)
            if not data:
                return {"pass": False,
                        "detail": "triage manifest %s is empty" % manifest}
        except (OSError, ValueError) as e:
            return {"pass": False,
                    "detail": "triage manifest %s unreadable: %s"
                              % (manifest, e)}

    sessions = [m for m in _source_manifests(project_root)
                if m.get("kind") == "session"]
    if not sessions:
        return {"pass": False,
                "detail": "manifest ok but no source ingested with "
                          "kind=session%s"
                          % ("; unsupported: " + ", ".join(unsupported)
                             if unsupported else "")}
    detail = "manifest %s + %d session-kind source(s)" % (
        os.path.relpath(manifest, project_root), len(sessions))
    if unsupported:
        detail += "; unsupported on this engine: " + ", ".join(unsupported)
    return {"pass": True, "detail": detail}


def assert_s4(project_root, transcript_path):
    """Correction: attribution/window-days = 7 is current and the 30d claim is
    superseded (flip-with-history — the old value is retained, terminal)."""
    claims = [c for c in _claims(project_root)
              if c.get("subject") == "attribution"
              and c.get("predicate") == "window-days"]
    if not claims:
        return {"pass": False,
                "detail": "no attribution/window-days claims — correction never staged"}
    cur7 = [c for c in claims
            if str(c.get("value")) == "7"
            and c.get("status") not in ("superseded", "rejected")]
    olds30 = [c for c in claims if str(c.get("value")) == "30"]
    if not cur7:
        return {"pass": False,
                "detail": "no current window-days=7 claim; present: %s"
                          % [(c.get("value"), c.get("status")) for c in claims]}
    new = cur7[0]
    sup30 = [c for c in olds30 if c.get("status") == "superseded"
             and c.get("superseded_by") == new["id"]]
    if not olds30:
        return {"pass": False,
                "detail": "pre-correction 30d claim vanished — history lost"}
    if not sup30:
        return {"pass": False,
                "detail": "no 30d claim superseded by %s; 30d states: %s"
                          % (new["id"],
                             [(c.get("status"), c.get("superseded_by"))
                              for c in olds30])}
    # a still-current *unscoped/prod* 30d claim means the flip did not take;
    # scope-differenced 30d claims (e.g. sandbox) legitimately stay accepted
    live30 = [c for c in olds30
              if c.get("status") not in ("superseded", "rejected")
              and (c.get("scope") or {}).get("env") in (None, "prod", "production")]
    if live30:
        return {"pass": False,
                "detail": "unscoped/prod 30d claim still %s alongside 7d"
                          % live30[0].get("status")}
    return {"pass": True,
            "detail": "7d current (%s), 30d superseded→%s"
                      % (new["id"], new["id"])}


def assert_s5(project_root, transcript_path):
    """Refusal: output for an unanswerable question carries refusal tokens."""
    text = assistant_text(transcript_path).lower()
    hit = next((t for t in _REFUSAL_TOKENS if t in text), None)
    if hit is None:
        return {"pass": False,
                "detail": "no refusal token in output — fabricated or evasive answer"}
    return {"pass": True, "detail": "refusal token %r present" % hit}


def assert_s6(project_root, transcript_path):
    """Query ladder: the agent answered via the wiki without grep/rg over the
    corpus. Commands are extracted from the transcript's tool calls; a
    transcript with no parseable commands at all is inconclusive (fail), so
    an empty run can't pass vacuously."""
    commands = shell_commands(transcript_path)
    if not commands:
        raw = assistant_text(transcript_path)
        if not raw.strip():
            return {"pass": False,
                    "detail": "empty transcript — nothing executed"}
        if _GREP_RE.search(raw):
            return {"pass": False,
                    "detail": "no structured commands extracted, and raw "
                              "transcript mentions grep/rg"}
        return {"pass": False,
                "detail": "no shell commands extractable from transcript — "
                          "cannot prove the grep-free query ladder"}
    offenders = [c for c in commands if _GREP_RE.search(c)]
    if offenders:
        return {"pass": False,
                "detail": "grep/rg used for the query: %s"
                          % "; ".join(o.strip()[:80] for o in offenders[:3])}
    wiki_used = any("wiki" in c and "query" in c for c in commands) or \
        "query-prepare" in assistant_text(transcript_path)
    detail = "%d commands, none grep/rg" % len(commands)
    if not wiki_used:
        detail += " (note: no query-prepare invocation observed either)"
    return {"pass": True, "detail": detail}



def assert_s7(project_root, transcript_path):
    """Multi-part llm-query: the agent answered (a) order-service database,
    (b) the retry max-attempts conflict values, (c) the source census — via
    the wiki's query commands, never grep/rg over sources."""
    commands = shell_commands(transcript_path)
    if not commands:
        return {"pass": False,
                "detail": "no shell commands extractable — inconclusive"}
    offenders = [c for c in commands if _GREP_RE.search(c)]
    if offenders:
        return {"pass": False,
                "detail": "grep/rg used for the query: %s"
                          % "; ".join(o.strip()[:80] for o in offenders[:3])}
    query_cmds = [c for c in commands
                  if any(k in c for k in ("query-prepare", "subjects",
                                          "context-pack", "status"))]
    if not query_cmds:
        return {"pass": False,
                "detail": "no wiki query command (query-prepare/subjects/"
                          "context-pack/status) observed"}
    text = assistant_text(transcript_path).lower()
    missing = []
    if "postgres" not in text:
        missing.append("database answer 'postgres'")
    if not ("retry" in text and "3" in text and "5" in text):
        missing.append("retry conflict values 3 and 5")
    if not re.search(r"\b(?:7|seven)\b", text):
        missing.append("source count (7)")
    if missing:
        return {"pass": False,
                "detail": "answers missing from output: %s"
                          % "; ".join(missing)}
    return {"pass": True,
            "detail": "postgres + retry 3/5 conflict + census answered via "
                      "%d query cmd(s), no grep/rg" % len(query_cmds)}


def assert_s8(project_root, transcript_path):
    """Graph-ui: agent emitted `graph-data` and rendered an HTML visualization
    with real node labels."""
    commands = shell_commands(transcript_path)
    graph_cmd = any("graph-data" in c for c in commands)
    if not graph_cmd:
        return {"pass": False,
                "detail": "no `graph-data` invocation observed"}
    payload_path = os.path.join(project_root, "graph.json")
    try:
        with open(payload_path) as f:
            payload = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        return {"pass": False,
                "detail": "graph.json missing/unparseable: %s" % e}
    nodes, links = payload.get("nodes") or [], payload.get("links") or []
    if not nodes or not links:
        return {"pass": False,
                "detail": "graph.json has %d nodes / %d links — empty graph"
                          % (len(nodes), len(links))}
    htmls = glob.glob(os.path.join(project_root, "*.html"))
    if not htmls:
        return {"pass": False, "detail": "no *.html visualization produced"}
    body = ""
    for h in htmls:
        try:
            body += open(h, encoding="utf-8", errors="replace").read().lower()
        except OSError:
            pass
    if not re.search(r"<script|<svg|<canvas", body):
        return {"pass": False,
                "detail": "html exists but renders nothing "
                          "(no script/svg/canvas): %s"
                          % [os.path.basename(h) for h in htmls]}
    labels = [l for l in ("order service", "retry", "attribution")
              if l in body]
    if len(labels) < 2:
        return {"pass": False,
                "detail": "viz missing node labels — only %s of "
                          "{order service, retry, attribution}" % labels}
    return {"pass": True,
            "detail": "graph.json %d nodes/%d links; %s renders labels %s"
                      % (len(nodes), len(links),
                         os.path.basename(htmls[0]), labels)}


ASSERTIONS = {"s1": assert_s1, "s2": assert_s2, "s3": assert_s3,
              "s4": assert_s4, "s5": assert_s5, "s6": assert_s6,
              "s7": assert_s7, "s8": assert_s8}

