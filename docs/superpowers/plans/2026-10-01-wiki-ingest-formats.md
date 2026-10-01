# wiki-ingest Folder + Multi-Format + Vision Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `wiki-ingest` to recursively ingest directories, support office/PDF/image formats, and let the agent supply vision-produced extraction text via `--normalized-content`.

**Architecture:** All evidence mechanics stay in the deterministic engine (`skill/llm-wiki/scripts/wikicore/`). New module `dirs.py` owns recursive traversal; `sources.py` gains format routing (UTF-8 decode-sniff, `OFFICE_EXTS` → markitdown, `IMAGE_EXTS` classification) plus `normalized_content`/`parser_name`/`force`/`root_origin` parameters. The `wiki-ingest` skill documents the per-format recipes and the vision lane; the agent is the only component that can see images.

**Spec:** `docs/superpowers/specs/2026-10-01-wiki-ingest-formats-design.md`

**Tech Stack:** Python 3 stdlib only (unittest, argparse, os, hashlib); optional runtime dep `markitdown` (CLI on PATH).

## Global Constraints

- Engine stays deterministic, network-free, stdlib-only. `markitdown` is invoked via `shutil.which` + `subprocess`, never imported.
- Ingest NEVER creates or modifies claims.
- All normalized content (including agent-supplied) passes through `_secret_gate` and `INJECTION_RE`.
- Extraction failure must never be silent — always a warning + honest `coverage`.
- Tests: `unittest`, `tempfile.mkdtemp` roots, `sys.path.insert(0, "..")` pattern from `tests/test_sources.py`. Run with `python3 -m unittest tests.test_X -v` from `skill/llm-wiki/scripts/`.
- CLI exits: `TxnError`→3, `ConflictError`→4, `LockedError`→5 (already wired in `main()`).
- Commit message style (from git log): `feat:`, `test:`, `fix:`, `docs:` prefixes.

## File Structure

- **Modify** `skill/llm-wiki/scripts/wikicore/sources.py` — format routing, new `ingest()` params, fill-in path.
- **Create** `skill/llm-wiki/scripts/wikicore/dirs.py` — directory traversal + batch ingest.
- **Modify** `skill/llm-wiki/scripts/wiki.py` — argparse flags, `cmd_ingest` dispatch.
- **Modify** `skill/wiki-ingest/SKILL.md` — routing table, vision lane, folder flow.
- **Modify** `skill/llm-wiki/references/COMMANDS.md` — ingest contract updates.
- **Modify** `install.sh` — markitdown install chain.
- **Create** `skill/llm-wiki/scripts/tests/test_dirs.py` — traversal/batch tests.
- **Modify** `skill/llm-wiki/scripts/tests/test_sources.py` — routing/normalized/force tests.

---

### Task 1: Format routing in `sources.py`

Replace the `TEXT_EXTS` allowlist with three-tier routing: `OFFICE_EXTS` → markitdown; else UTF-8 decode attempt → text; else binary `not_extracted`. Add `IMAGE_EXTS`.

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/sources.py:19-23,59-64,117-127`
- Test: `skill/llm-wiki/scripts/tests/test_sources.py`

**Interfaces:**
- Consumes: existing `ingest()`, `_secret_gate`, `_parser_for_binary`.
- Produces (for Task 2+):
  ```python
  OFFICE_EXTS = {".pdf", ".docx", ".xlsx", ".pptx", ".doc", ".xls", ".ppt"}
  IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp",
                ".tiff", ".tif", ".heic", ".heif"}
  SKIP_EXTS = {".zip", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".rar",
               ".exe", ".dll", ".dylib", ".so", ".o", ".a", ".class",
               ".wasm", ".pyc", ".mp3", ".mp4", ".wav", ".mov"}
  SKIP_NAMES = {".DS_Store", "Thumbs.db"}
  def _is_office(ref: str) -> bool      # _ext(ref) in OFFICE_EXTS
  def _is_image(ref: str) -> bool       # _ext(ref) in IMAGE_EXTS
  def _is_text_like(kind, ref, data) -> bool  # text/url/session kinds, or utf-8-decodable file
  ```

- [ ] **Step 1: Write failing tests** — append to `test_sources.py`:

```python
def test_unknown_ext_utf8_routes_text(self):
    wiki = fresh()
    p = os.path.join(tempfile.mkdtemp(), "notes.weird")
    with open(p, "w") as f:
        f.write("decide: use sqlite\n")
    r = sources.ingest(wiki, "file", p, open(p, "rb").read())
    self.assertEqual(r["coverage"]["text"], "complete")
    self.assertIn("sqlite", sources.load_content(wiki, r["source_id"], 1))

def test_webp_binary_not_extracted(self):
    wiki = fresh()
    p = os.path.join(tempfile.mkdtemp(), "pic.webp")
    data = b"RIFF\x00\x00\x00\x00WEBPVP8 " + bytes(range(64))
    with open(p, "wb") as f:
        f.write(data)
    with mock.patch.object(sources, "_parser_for_binary", return_value=(None, None)):
        r = sources.ingest(wiki, "file", p, data)
    self.assertEqual(r["coverage"]["text"], "not_extracted")
    self.assertEqual(r["coverage"]["images"], "not_extracted")

def test_svg_routes_text(self):
    wiki = fresh()
    p = os.path.join(tempfile.mkdtemp(), "icon.svg")
    svg = b'<svg xmlns="http://www.w3.org/2000/svg"><text>Hi</text></svg>'
    with open(p, "wb") as f:
        f.write(svg)
    r = sources.ingest(wiki, "file", p, svg)
    self.assertEqual(r["coverage"]["text"], "complete")

def test_office_ext_calls_parser(self):
    wiki = fresh()
    p = os.path.join(tempfile.mkdtemp(), "spec.docx")
    data = b"PK\x03\x04" + b"\x00" * 100
    with open(p, "wb") as f:
        f.write(data)
    with mock.patch.object(sources, "_parser_for_binary",
                           return_value=("extracted text", "markitdown")) as m:
        r = sources.ingest(wiki, "file", p, data)
    m.assert_called_once()
    self.assertEqual(r["parser"], {"name": "markitdown", "version": "?"})
    self.assertEqual(r["coverage"]["text"], "partial")
```

Note: `test_binary_pdf_preserved_not_extracted` (existing) must still pass —
it exercises the office path without a mock; markitdown absent → `not_extracted`
or present → `partial`; make it tolerant if needed:

```python
self.assertIn(r["coverage"]["text"], ("not_extracted", "partial"))
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_sources -v`
Expected: FAIL — `.weird` file routes binary (`not_extracted`), images coverage key absent.

- [ ] **Step 3: Implement routing in `sources.py`**

Replace lines 19-23 (ext sets) with:

```python
OFFICE_EXTS = {".pdf", ".docx", ".xlsx", ".pptx", ".doc", ".xls", ".ppt"}
IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp",
              ".tiff", ".tif", ".heic", ".heif"}
SKIP_EXTS = {".zip", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".rar",
             ".exe", ".dll", ".dylib", ".so", ".o", ".a", ".class",
             ".wasm", ".pyc", ".mp3", ".mp4", ".wav", ".mov"}
SKIP_NAMES = {".DS_Store", "Thumbs.db"}
```

Replace `_is_text_like` (lines 59-64) with:

```python
def _is_office(ref: str) -> bool:
    return _ext(ref) in OFFICE_EXTS


def _is_image(ref: str) -> bool:
    return _ext(ref) in IMAGE_EXTS


def _is_text_like(kind: str, ref: str, data: bytes) -> bool:
    if kind in ("text", "url", "session"):
        return True
    if kind == "file":
        if _ext(ref) in OFFICE_EXTS:
            return False  # markitdown path; decode-sniff must not win
        try:
            data.decode("utf-8")
            return True
        except UnicodeDecodeError:
            return False
    return False  # directory/repo handled as binary-ish bundle
```

In `ingest()` update the call site (line ~117):

```python
    text_like = _is_text_like(kind, ref, data)
```

and initialize coverage so images get honest reporting (line ~169):

```python
    coverage = {
        "text": "complete",
        "tables": "skipped",
        "images": "not_extracted" if _is_image(ref) else "skipped",
        "diagrams": "skipped",
        "formulas": "skipped",
    }
```

`_parser_for_binary` is already called for all non-text files (line ~176);
office files reach it naturally. No change needed there.

- [ ] **Step 4: Run tests to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_sources -v`
Expected: PASS (all).

- [ ] **Step 5: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/sources.py skill/llm-wiki/scripts/tests/test_sources.py
git commit -m "feat: decode-sniff format routing, office/image ext sets in ingest"
```

---

### Task 2: `--normalized-content`, `--parser-name`, `--force`, `root_origin`

Extend `sources.ingest()` signature and CLI. Implements the vision-lane plumbing
and the dedup/`--force` matrix from spec §Dedup.

**Files:**
- Modify: `skill/llm-wiki/scripts/wikicore/sources.py` (`ingest()`)
- Modify: `skill/llm-wiki/scripts/wiki.py:44-57,189-195`
- Test: `skill/llm-wiki/scripts/tests/test_sources.py`

**Interfaces:**
- Consumes: `_is_office`, `_is_image` from Task 1.
- Produces (for Task 3):
  ```python
  def ingest(wiki, kind, ref, data, source_id=None, *,
             normalized_content=None,   # str | None — agent-written markdown
             parser_name=None,          # str | None — e.g. "agent-vision"
             force=False,               # bypass sha256 dedup -> new version
             root_origin=None) -> dict  # str | None — e.g. "dir:/abs/path"
  ```
  Receipt may add keys: `normalized_source: "agent"|"markitdown"|"wikicore-raw"`,
  `updated_normalized: true` (fill-in path).

- [ ] **Step 1: Write failing tests** — append to `test_sources.py`:

```python
def _png_bytes():
    return (b"\x89PNG\r\n\x1a\n" + bytes(range(64)))

class TestNormalizedContent(unittest.TestCase):
    def test_normalized_content_stored(self):
        wiki = fresh()
        p = os.path.join(tempfile.mkdtemp(), "chart.png")
        data = _png_bytes()
        with open(p, "wb") as f:
            f.write(data)
        r = sources.ingest(wiki, "file", p, data,
                           normalized_content="# chart.png\n\n## Text content\n(none)\n",
                           parser_name="agent-vision")
        self.assertEqual(r["normalized_source"], "agent")
        self.assertEqual(r["coverage"]["text"], "complete")
        self.assertEqual(r["coverage"]["images"], "complete")
        self.assertIn("chart.png",
                      sources.load_content(wiki, r["source_id"], 1))

    def test_normalized_content_secret_gated(self):
        wiki = fresh(policy="deny")
        p = os.path.join(tempfile.mkdtemp(), "x.png")
        with open(p, "wb") as f:
            f.write(_png_bytes())
        with self.assertRaises(TxnError):
            sources.ingest(wiki, "file", p, _png_bytes(),
                           normalized_content="key sk-live-abcdef1234567890")

    def test_normalized_fill_in_updates_raw_version(self):
        wiki = fresh()
        p = os.path.join(tempfile.mkdtemp(), "d.png")
        data = _png_bytes()
        with open(p, "wb") as f:
            f.write(data)
        with mock.patch.object(sources, "_parser_for_binary",
                               return_value=(None, None)):
            r1 = sources.ingest(wiki, "file", p, data)
        self.assertEqual(r1["coverage"]["text"], "not_extracted")
        r2 = sources.ingest(wiki, "file", p, data,
                            normalized_content="# d.png\n\nflowchart A->B\n",
                            parser_name="agent-vision")
        self.assertTrue(r2.get("updated_normalized"))
        self.assertEqual(r2["version"], 1)  # in place — no new version
        self.assertIn("flowchart",
                      sources.load_content(wiki, r1["source_id"], 1))

    def test_force_mints_new_version_same_sha(self):
        wiki = fresh()
        p = os.path.join(tempfile.mkdtemp(), "a.md")
        with open(p, "w") as f:
            f.write("v1 text\n")
        r1 = sources.ingest(wiki, "file", p, open(p, "rb").read())
        r2 = sources.ingest(wiki, "file", p, open(p, "rb").read(), force=True)
        self.assertEqual(r1["source_id"], r2["source_id"])
        self.assertEqual(r2["version"], 2)
        self.assertFalse(r2["deduplicated"])

    def test_root_origin_recorded(self):
        wiki = fresh()
        p = os.path.join(tempfile.mkdtemp(), "a.md")
        with open(p, "w") as f:
            f.write("x\n")
        r = sources.ingest(wiki, "file", p, open(p, "rb").read(),
                           root_origin="dir:/tmp/somedir")
        m = sources.get_manifest(wiki, r["source_id"])
        self.assertEqual(m["root_origin"], "dir:/tmp/somedir")

    def test_fill_in_uncompiles_version(self):
        wiki = fresh()
        p = os.path.join(tempfile.mkdtemp(), "img.png")
        data = _png_bytes()
        with open(p, "wb") as f:
            f.write(data)
        with mock.patch.object(sources, "_parser_for_binary",
                               return_value=(None, None)):
            r = sources.ingest(wiki, "file", p, data)
        sources.mark_compiled(wiki, r["source_id"], 1)
        self.assertEqual(sources.pending_versions(wiki), [])
        sources.ingest(wiki, "file", p, data,
                       normalized_content="# img.png\ntext\n",
                       parser_name="agent-vision")
        pending = sources.pending_versions(wiki)
        self.assertEqual(len(pending), 1)
        self.assertEqual(pending[0]["version"], 1)
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_sources -v`
Expected: FAIL — `TypeError: unexpected keyword argument 'normalized_content'`.

- [ ] **Step 3: Implement in `sources.py`**

Change signature to:

```python
def ingest(wiki, kind: str, ref: str, data: bytes, source_id: Optional[str] = None,
           normalized_content: Optional[str] = None,
           parser_name: Optional[str] = None,
           force: bool = False,
           root_origin: Optional[str] = None) -> dict:
```

In the dedup branch (currently lines 137-149), replace with:

```python
    if sid and not force:
        manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        for v in manifest["versions"]:
            if v["sha256"] == sha:
                if normalized_content is not None:
                    return _fill_normalized(wiki, sid, v, normalized_content,
                                            parser_name, ref, sha)
                return {
                    "source_id": sid,
                    "version": v["version"],
                    "sha256": sha,
                    "deduplicated": True,
                    "warnings": [],
                    "secrets": {"findings": [], "redacted": 0},
                    "raw_path": v["raw_path"],
                }
        version = len(manifest["versions"]) + 1
    elif sid:
        manifest = wiki.load_json("sources/%s/manifest.json" % sid)
        version = len(manifest["versions"]) + 1
    else:
        sid = ids.new("source")
        manifest = {
            "id": sid, "kind": kind,
            "origin": ref if kind != "text" else "inline-text",
            "root_origin": root_origin or sid,
            "created_at": _now(), "versions": [],
            "title": filename or ref[:80],
        }
        version = 1
```

Add the fill-in helper (it owns its own transaction):

```python
def _fill_normalized(wiki, sid, version_entry, content, parser_name, ref, sha):
    """In-place completion of a not_extracted version (spec §Dedup)."""
    v = version_entry["version"]
    extraction = wiki.load_json("sources/%s/extraction.json" % sid)
    cur = extraction.get("extraction_report", {})
    if extraction.get("source_version") != v or \
            cur.get("coverage", {}).get("text") != "not_extracted":
        return {  # prior version already extracted -> plain dedup
            "source_id": sid, "version": v, "sha256": sha,
            "deduplicated": True, "warnings": [
                "normalized_content_ignored: version already text-extracted"],
            "secrets": {"findings": [], "redacted": 0},
            "raw_path": version_entry["raw_path"],
        }
    warnings = []
    text, secret_summary = _secret_gate(wiki, content,
                                        os.path.basename(ref), warnings)
    txn = Transaction(wiki, "wiki-ingest-fill")
    txn.stage_write("sources/%s/content.md" % sid, text)
    coverage = dict(cur["coverage"])
    coverage["text"] = "complete"
    if _is_image(ref):
        coverage["images"] = "complete"
    report = {"source_id": sid, "source_version": v, "coverage": coverage,
              "warnings": warnings,
              "parser": {"name": parser_name or "agent", "version": "in-session"}}
    extraction["extraction_report"] = report
    txn.stage_write("sources/%s/extraction.json" % sid, extraction)

    def _uncompile(state):
        done = state.get("compiled", {}).get(sid, [])
        if v in done:
            done.remove(v)

    txn.stage_state(".state/manifest.json", _uncompile)
    txn.changes = {"sources_registered": 0, "versions_added": 0}
    txn.warnings = warnings
    txn.commit(wiki.revision())
    return {"source_id": sid, "version": v, "sha256": sha,
            "deduplicated": False, "updated_normalized": True,
            "raw_path": version_entry["raw_path"], "coverage": coverage,
            "warnings": warnings, "secrets": secret_summary,
            "run_id": txn.run_id}
```

In the main body of `ingest()`, where normalized text is produced
(currently lines 120-127 and 173-188), insert `normalized_content` precedence:

```python
    normalized_text = ""
    parser = {"name": "wikicore-raw", "version": "1.0"}
    if normalized_content is not None:
        normalized_text, secret_summary = _secret_gate(
            wiki, normalized_content, filename, warnings)
        parser = {"name": parser_name or "agent", "version": "in-session"}
        normalized_source = "agent"
    elif text_like:
        text = data.decode("utf-8", "replace")
        text, secret_summary = _secret_gate(wiki, text, filename, warnings)
        normalized_text = text
        normalized_source = "wikicore-raw"
    else:
        secret_summary = {"findings": [], "redacted": 0}
        normalized_source = None
```

In the storage block, when `normalized_content is not None` write it directly
before the text_like branch:

```python
    if normalized_content is not None:
        txn.stage_write(norm_rel, normalized_text)
        coverage["text"] = "complete"
        if _is_image(ref):
            coverage["images"] = "complete"
    elif text_like:
        txn.stage_write(norm_rel, normalized_text)
    else:
        ...existing markitdown/not_extracted path, plus
        normalized_source = "markitdown" when parsed...
```

And add `"normalized_source": normalized_source` to the returned receipt.

- [ ] **Step 4: Wire the CLI** — `wiki.py`

In `cmd_ingest` (lines 44-57), read the normalized file and pass params:

```python
def cmd_ingest(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    normalized = None
    if args.normalized_content:
        with open(args.normalized_content, "r", encoding="utf-8") as f:
            normalized = f.read()
    if args.url:
        import urllib.request
        with urllib.request.urlopen(args.url, timeout=60) as resp:
            data = resp.read()
        _emit(sources.ingest(wiki, "url", args.url, data,
                             source_id=args.source_id,
                             normalized_content=normalized,
                             parser_name=args.parser_name,
                             force=args.force))
    else:
        data = _read_bytes(args)
        ref = args.file or args.text_ref or "inline"
        kind = "text" if args.text is not None else "file"
        _emit(sources.ingest(wiki, kind, ref, data,
                             source_id=args.source_id,
                             normalized_content=normalized,
                             parser_name=args.parser_name,
                             force=args.force))
    return 0
```

Add to the `ingest` subparser (after line 194):

```python
    p.add_argument("--normalized-content", default=None,
                   help="agent-produced extraction markdown file")
    p.add_argument("--parser-name", default=None,
                   help="parser label for --normalized-content, e.g. agent-vision")
    p.add_argument("--force", action="store_true",
                   help="bypass sha256 dedup and append a new version")
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_sources -v`
Expected: PASS (all).

- [ ] **Step 6: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/sources.py skill/llm-wiki/scripts/wiki.py skill/llm-wiki/scripts/tests/test_sources.py
git commit -m "feat: --normalized-content, --parser-name, --force, fill-in on ingest"
```

---

### Task 3: `dirs.py` — recursive `--dir` ingest

**Files:**
- Create: `skill/llm-wiki/scripts/wikicore/dirs.py`
- Modify: `skill/llm-wiki/scripts/wiki.py` (`cmd_ingest`, parser)
- Test: `skill/llm-wiki/scripts/tests/test_dirs.py` (new)

**Interfaces:**
- Consumes: `sources.ingest()` (Task 2 signature), `sources.SKIP_EXTS`, `SKIP_NAMES`.
- Produces:
  ```python
  SKIP_DIRS = {".git", ".svn", ".hg", "node_modules", "__pycache__",
               ".venv", "venv", "dist", "build", "target", "vendor",
               ".idea", ".llm-wiki"}
  DEFAULT_MAX_BYTES = 50 * 1024 * 1024
  def ingest_dir(wiki, path, *, include_hidden=False,
                 max_bytes=DEFAULT_MAX_BYTES, force=False) -> dict
  ```
  Returns the batch receipt dict (spec §1). Also writes
  `.llm-wiki/state/ingest-batches/<batch_id>.json`.

- [ ] **Step 1: Write failing tests** — `tests/test_dirs.py`:

```python
import json, os, sys, tempfile, unittest
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from wikicore import dirs, sources
from wikicore.store import Wiki, init_wiki
from wikicore.transaction import TxnError


def fresh():
    root = tempfile.mkdtemp()
    wiki = Wiki(root)
    init_wiki(wiki, "dir-test")
    return wiki


def tree(files):
    """files: {relpath: bytes}; returns dir path."""
    root = tempfile.mkdtemp()
    for rel, data in files.items():
        p = os.path.join(root, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.write(data)
    return root


class TestIngestDir(unittest.TestCase):
    def test_walks_nested_tree_one_source_per_file(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "sub/b.md": b"b\n", "sub/deep/c.md": b"c\n"})
        r = dirs.ingest_dir(wiki, d)
        self.assertEqual(r["counts"]["ingested"], 3)
        ids = {i["source_id"] for i in r["items"]}
        self.assertEqual(len(ids), 3)
        m = sources.get_manifest(wiki, r["items"][0]["source_id"])
        self.assertEqual(m["root_origin"], "dir:" + os.path.abspath(d))

    def test_skip_rules(self):
        wiki = fresh()
        d = tree({
            "keep.md": b"k\n",
            ".hidden.md": b"h\n",
            "node_modules/x.js": b"js\n",
            ".git/config": b"g\n",
            "big.bin": b"x" * (1024),
            "a.zip": b"PK\x03\x04zip",
            ".DS_Store": b"junk",
        })
        r = dirs.ingest_dir(wiki, d, max_bytes=512)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["keep.md"]["status"], "ingested")
        self.assertEqual(by[".hidden.md"]["reason"], "hidden")
        self.assertEqual(by["node_modules/x.js"]["reason"], "vcs/vendor dir")
        self.assertEqual(by["big.bin"]["reason"], "oversize")
        self.assertEqual(by["a.zip"]["reason"], "non-evidence binary")
        self.assertEqual(by[".DS_Store"]["reason"], "non-evidence binary")

    def test_include_hidden(self):
        wiki = fresh()
        d = tree({".env.example": b"A=1\n", "x.md": b"x\n"})
        r = dirs.ingest_dir(wiki, d, include_hidden=True)
        self.assertEqual(r["counts"]["ingested"], 2)

    def test_rerun_dedups_then_force(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "b.md": b"b\n"})
        dirs.ingest_dir(wiki, d)
        r2 = dirs.ingest_dir(wiki, d)
        self.assertEqual(r2["counts"]["deduplicated"], 2)
        r3 = dirs.ingest_dir(wiki, d, force=True)
        self.assertEqual(r3["counts"]["ingested"], 2)
        sid = next(i["source_id"] for i in r3["items"])
        self.assertEqual(len(sources.get_manifest(wiki, sid)["versions"]), 2)

    def test_changed_file_new_version_only(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "b.md": b"b\n"})
        dirs.ingest_dir(wiki, d)
        with open(os.path.join(d, "a.md"), "w") as f:
            f.write("a changed\n")
        r = dirs.ingest_dir(wiki, d)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "ingested")
        self.assertEqual(by["a.md"]["version"], 2)
        self.assertEqual(by["b.md"]["status"], "deduplicated")

    def test_skips_wiki_dir_inside_tree(self):
        wiki = fresh()
        # a directory that contains a .llm-wiki store must not ingest it
        wiki2_root = tempfile.mkdtemp()
        os.makedirs(os.path.join(wiki2_root, ".llm-wiki"), exist_ok=True)
        with open(os.path.join(wiki2_root, ".llm-wiki", "x.json"), "w") as f:
            f.write("{}")
        with open(os.path.join(wiki2_root, "a.md"), "w") as f:
            f.write("a\n")
        r = dirs.ingest_dir(wiki, wiki2_root)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "ingested")
        self.assertNotIn(".llm-wiki/x.json", by)

    def test_unreadable_file_is_error_not_abort(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n", "b.md": b"b\n"})
        os.chmod(os.path.join(d, "b.md"), 0)
        try:
            r = dirs.ingest_dir(wiki, d)
        finally:
            os.chmod(os.path.join(d, "b.md"), 0o644)
        by = {i["path"]: i for i in r["items"]}
        self.assertEqual(by["a.md"]["status"], "ingested")
        self.assertEqual(by["b.md"]["status"], "error")

    def test_batch_receipt_written(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n"})
        r = dirs.ingest_dir(wiki, d)
        p = wiki.p("state", "ingest-batches", "%s.json" % r["batch_id"])
        self.assertTrue(os.path.isfile(p))
        with open(p) as f:
            saved = json.load(f)
        self.assertEqual(saved["batch_id"], r["batch_id"])

    def test_dir_must_be_directory(self):
        wiki = fresh()
        d = tree({"a.md": b"a\n"})
        with self.assertRaises(TxnError):
            dirs.ingest_dir(wiki, os.path.join(d, "a.md"))


if __name__ == "__main__":
    unittest.main()
```



- [ ] **Step 2: Run tests to verify failure**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_dirs -v`
Expected: FAIL — `ModuleNotFoundError: wikicore.dirs`.

- [ ] **Step 3: Implement `wikicore/dirs.py`**

```python
"""Recursive directory ingest: deterministic walk, one source per file.

Traversal rules are deterministic and testable; each member file goes through
sources.ingest() unchanged (own source_id, dedup, secrets scan, report).
"""
import os
import time
from typing import Optional

from . import ids, sources
from .transaction import TxnError

SKIP_DIRS = {".git", ".svn", ".hg", "node_modules", "__pycache__",
             ".venv", "venv", "dist", "build", "target", "vendor",
             ".idea", ".llm-wiki"}
DEFAULT_MAX_BYTES = 50 * 1024 * 1024


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _classify_skip(path: str, rel: str, name: str, is_dir: bool,
                   include_hidden: bool, max_bytes: int) -> Optional[str]:
    """Return skip reason, or None if the entry should be ingested."""
    if is_dir:
        if name in SKIP_DIRS:
            return "vcs/vendor dir"
        if name.startswith(".") and not include_hidden:
            return "hidden"
        return None
    if name in sources.SKIP_NAMES:
        return "non-evidence binary"
    if os.path.splitext(name)[1].lower() in sources.SKIP_EXTS:
        return "non-evidence binary"
    if name.startswith(".") and not include_hidden:
        return "hidden"
    if os.path.islink(path):
        return "symlink"
    try:
        if os.path.getsize(path) > max_bytes:
            return "oversize"
    except OSError:
        return "unreadable"
    return None


def _walk(root: str, include_hidden: bool, max_bytes: int) -> list:
    """Yield (relpath, action, reason) for every entry; files + skips."""
    items = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        for d in list(dirnames):
            p = os.path.join(dirpath, d)
            rel = os.path.relpath(p, root)
            reason = _classify_skip(p, rel, d, True, include_hidden, max_bytes)
            if reason:
                items.append((rel, "skipped", reason))
                dirnames.remove(d)
        for f in filenames:
            p = os.path.join(dirpath, f)
            rel = os.path.relpath(p, root)
            reason = _classify_skip(p, rel, f, False, include_hidden, max_bytes)
            if reason:
                items.append((rel, "skipped", reason))
            else:
                items.append((rel, "file", None))
    items.sort(key=lambda t: t[0])
    return items


def ingest_dir(wiki, path: str, *, include_hidden: bool = False,
               max_bytes: int = DEFAULT_MAX_BYTES, force: bool = False) -> dict:
    if not wiki.exists():
        raise TxnError("wiki not initialized; run wiki-init first")
    if not os.path.isdir(path):
        raise TxnError("not a directory: %s" % path)
    root = os.path.abspath(path)
    root_origin = "dir:" + root
    batch_id = ids.new("batch")
    items, counts = [], {"ingested": 0, "deduplicated": 0,
                         "skipped": 0, "errors": 0}
    for rel, action, reason in _walk(root, include_hidden, max_bytes):
        if action == "skipped":
            items.append({"path": rel, "status": "skipped", "reason": reason})
            counts["skipped"] += 1
            continue
        full = os.path.join(root, rel)
        try:
            with open(full, "rb") as fh:
                data = fh.read()
        except OSError as e:
            items.append({"path": rel, "status": "error", "error": str(e)})
            counts["errors"] += 1
            continue
        try:
            r = sources.ingest(wiki, "file", full, data,
                               force=force, root_origin=root_origin)
        except Exception as e:
            items.append({"path": rel, "status": "error", "error": str(e)})
            counts["errors"] += 1
            continue
        if r.get("deduplicated"):
            status, key = "deduplicated", "deduplicated"
        elif r.get("updated_normalized"):
            status, key = "updated_normalized", "ingested"
        else:
            status, key = "ingested", "ingested"
        counts[key] += 1
        item = {"path": rel, "status": status,
                "source_id": r["source_id"], "version": r["version"]}
        if r.get("coverage"):
            item["coverage"] = r["coverage"]
        if r.get("warnings"):
            item["warnings"] = r["warnings"]
        items.append(item)
    receipt = {"batch_id": batch_id, "dir": root, "captured_at": _now(),
               "counts": counts, "items": items}
    out_dir = wiki.p("state", "ingest-batches")
    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "%s.json" % batch_id), "w") as f:
        import json
        json.dump(receipt, f, indent=2, ensure_ascii=False)
        f.write("\n")
    return receipt
```



- [ ] **Step 4: Wire the CLI** — `wiki.py`

Add to the ingest subparser:

```python
    p.add_argument("--dir", default=None)
    p.add_argument("--include-hidden", action="store_true")
    p.add_argument("--max-bytes", type=int, default=dirs.DEFAULT_MAX_BYTES)
```

In `cmd_ingest`, dispatch before `--url`:

```python
def cmd_ingest(args) -> int:
    wiki = Wiki(args.root or os.getcwd())
    if args.dir:
        if args.normalized_content:
            raise TxnError("--normalized-content is per-file; not valid with --dir")
        _emit(dirs.ingest_dir(wiki, args.dir,
                              include_hidden=args.include_hidden,
                              max_bytes=args.max_bytes, force=args.force))
        return 0
    ...existing normalized file read + url/file branch...
```

Add `from wikicore import dirs` to the import line at wiki.py:14-15.

- [ ] **Step 5: Run tests to verify pass**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest tests.test_dirs tests.test_sources -v`
Expected: PASS (all).

- [ ] **Step 6: Commit**

```bash
git add skill/llm-wiki/scripts/wikicore/dirs.py skill/llm-wiki/scripts/wiki.py skill/llm-wiki/scripts/tests/test_dirs.py
git commit -m "feat: recursive --dir ingest with batch receipt"
```

---

### Task 4: Skill docs — `wiki-ingest/SKILL.md` + `COMMANDS.md`

**Files:**
- Modify: `skill/wiki-ingest/SKILL.md`
- Modify: `skill/llm-wiki/references/COMMANDS.md:18-37`

**Interfaces:** none (documentation only; matches implemented CLI).

- [ ] **Step 1: Rewrite `skill/wiki-ingest/SKILL.md`**

Replace the Engine section's command list with:

````markdown
## Engine

All state changes go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Resolve it once, then run from the
project root:

```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" ingest --file path/to/doc.md
python3 "$WIKI" ingest --dir path/to/folder          # recursive, one source per file
python3 "$WIKI" ingest --url https://example.com/spec
python3 "$WIKI" ingest --text "user said: use Postgres" --text-ref "chat-2026-09-15"
```

## Format routing

Route every input through this table:

| Input | Recipe |
|---|---|
| UTF-8-decodable text (.md .txt .csv .tsv .json .yaml .html .svg .log, any text ext) | `ingest --file` direct |
| .docx .xlsx .pptx .doc .xls .ppt | `ingest --file` — markitdown extracts inside the engine. If receipt `coverage.text == not_extracted` → extract text yourself (read tool / docx2txt / unzip+strings), write to a temp .md, re-ingest with `--normalized-content` |
| .pdf | markitdown first; on `not_extracted` extract yourself (read tool, `pdftotext`, or vision for scanned pages) → `--normalized-content` |
| image (.png .jpg .jpeg .webp .gif .bmp .tiff .heic) | **vision lane** below |
| unknown binary | `ingest --file` → surface the `not_extracted` warning; stop unless the user says the content matters |
| directory | `ingest --dir`, then loop receipt items where `coverage.text == "not_extracted"` through the lanes above |

## Vision lane (images)

1. View the image with your vision-capable read tool.
2. Write a description file (any temp path) in this shape:

```markdown
# <image filename>

<one-paragraph summary of what the image shows>

## Text content
<verbatim transcription of all readable text; "(none)" if none>

## Diagram / flow
<semantic description of diagrams/charts/flows: nodes, edges, axes,
relationships; "(none)" if not applicable>

## Details
<notable visual elements, layout, annotations, confidence caveats>
```

3. Ingest it:

```bash
python3 "$WIKI" ingest --file img.png --normalized-content desc.md --parser-name agent-vision
```

The engine stores raw bytes + your description as `content.md`, and the
extraction report records `parser.name: agent-vision`. If bytes are unchanged
from a previous raw-only ingest, your text fills the existing version in place
(`updated_normalized: true`) — that is expected, not an error. To force a fresh
version instead, add `--force`.

## Folder lane

```bash
python3 "$WIKI" ingest --dir ./docs
```

The engine walks deterministically (sorted), skips VCS/vendor dirs, hidden
files, files >50MiB (`--max-bytes` to change, `--include-hidden` to include
dotfiles), and non-evidence binaries — every skip appears in the batch receipt
with a reason. Unchanged files report `deduplicated: true`; pass `--force` to
re-ingest them anyway. After the batch, loop items whose `coverage.text` is
`not_extracted` through the office/pdf/vision lanes, then report merged counts.
````

Keep the existing "Receipt checks" and "Hard rules" sections unchanged; add one
receipt bullet: `- \`updated_normalized: true\` → your --normalized-content
filled a previously raw-only version in place; it will appear in compile-plan
again (compiled flag cleared).`

- [ ] **Step 2: Update `COMMANDS.md` ingest section**

Extend the ingest example block (lines 22-27) with:

```markdown
wiki.py ingest --dir docs/                        # recursive; batch receipt
wiki.py ingest --dir docs/ --include-hidden --max-bytes 104857600 --force
wiki.py ingest --file img.png --normalized-content desc.md --parser-name agent-vision
```

And append receipt bullets:

```markdown
- `normalized_source: "agent"` → content.md came from `--normalized-content`;
  `parser.name` reflects `--parser-name`.
- `updated_normalized: true` → identical bytes, prior version was
  `not_extracted`; content.md + extraction.json updated in place and the
  version was un-compiled so compile-plan sees the new text.
- `--force` bypasses sha256 dedup and always appends a version — use when the
  extractor improved (markitdown installed, better vision pass).
```

- [ ] **Step 3: Commit**

```bash
git add skill/wiki-ingest/SKILL.md skill/llm-wiki/references/COMMANDS.md
git commit -m "docs: format routing, vision lane, and folder ingest in wiki-ingest"
```

---

### Task 5: `install.sh` — markitdown install

**Files:**
- Modify: `install.sh` (after the symlink loop, before the verify echo)

- [ ] **Step 1: Add install block**

Append before the `echo` + verify lines:

```bash
# Optional extractor: markitdown (office/pdf text extraction in wiki-ingest).
# Try uv tool -> pipx -> pip --user; failure is a warning, not fatal.
if ! command -v markitdown >/dev/null 2>&1; then
  if command -v uv >/dev/null 2>&1; then
    uv tool install markitdown 2>/dev/null || \
      echo "note: uv install of markitdown failed; pdf/office files ingest as not_extracted" >&2
  elif command -v pipx >/dev/null 2>&1; then
    pipx install markitdown 2>/dev/null || \
      echo "note: pipx install of markitdown failed; pdf/office files ingest as not_extracted" >&2
  elif command -v pip3 >/dev/null 2>&1; then
    pip3 install --user markitdown 2>/dev/null || \
      echo "note: pip install of markitdown failed; pdf/office files ingest as not_extracted" >&2
  else
    echo "note: no uv/pipx/pip3 found; pdf/office files ingest as not_extracted" >&2
  fi
fi
```

- [ ] **Step 2: Smoke-test the script logic**

Run: `bash -n install.sh` (syntax check only — do not actually install).
Expected: no output, exit 0.

- [ ] **Step 3: Commit**

```bash
git add install.sh
git commit -m "feat: pre-install markitdown during skill install (uv/pipx/pip fallback)"
```

---

### Task 6: Full-suite verification

- [ ] **Step 1: Run the whole test suite**

Run: `cd skill/llm-wiki/scripts && python3 -m unittest discover -s tests -v`
Expected: all tests pass (existing + new).

- [ ] **Step 2: End-to-end smoke**

```bash
tmp=$(mktemp -d); cd "$tmp"
python3 $REPO/skill/llm-wiki/scripts/wiki.py init --name smoke
mkdir -p docs/sub && echo "# doc" > docs/a.md && echo "b" > docs/sub/b.md
printf '\x89PNG\r\n\x1a\n' > docs/img.png
python3 $REPO/skill/llm-wiki/scripts/wiki.py ingest --dir docs
python3 $REPO/skill/llm-wiki/scripts/wiki.py ingest --dir docs   # all deduplicated
python3 $REPO/skill/llm-wiki/scripts/wiki.py ingest --file docs/img.png \
  --normalized-content <(echo "# img.png") --parser-name agent-vision
python3 $REPO/skill/llm-wiki/scripts/wiki.py compile-plan        # img now pending
```

Expected: 3 ingested first run (png not_extracted), all deduplicated second
run, `updated_normalized` third run, compile-plan lists the png source.

- [ ] **Step 3: Final commit / confirm clean**

```bash
git status --short
```

