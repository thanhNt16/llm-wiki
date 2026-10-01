# wiki-ingest — folder + multi-format + vision extraction

Date: 2026-10-01
Status: approved design (pending spec review)

## Goal

Extend `wiki-ingest` so it can:

1. Ingest a single file **or** recursively ingest a whole folder (`--dir`).
2. Support a broad format set: pdf, docx/xlsx/pptx (+ legacy doc/xls/ppt),
   md/txt/csv/tsv/json/yaml/html/xml/etc., and raster images (png/jpg/jpeg/
   webp/gif/bmp/tiff/heic; svg decodes as XML text and routes text-side).
3. For images, the agent uses its vision model to describe the image — text
   content, diagram/flow semantics, details — and that description becomes the
   source's normalized `content.md` (raw bytes still preserved).

Approach chosen (of A/B/C considered): **extend the engine, agent stays the
extractor for vision/agent-side formats**. wikicore remains deterministic and
network-free; the agent supplies extraction text via a new ingest flag.

## Decisions (from clarifying Q&A)

- Folder ingest → **one source per file** (own source_id, versioning, dedup,
  extraction report); a shared batch record groups the run.
- Agent-produced extraction enters via **`--normalized-content <md-file>`**
  (single transaction), not a second command or sidecar convention.
- Office formats → **markitdown as the bundled extractor**, pre-installed by
  `install.sh`; PDFs try markitdown then fall back to agent extraction.
- Directory traversal lives in the **engine** (`--dir`), deterministic and
  testable — not a skill-side glob loop.
- Default skips: VCS/vendor dirs, hidden files, oversize files, known
  non-evidence binaries.
- Dedup is sha256-of-bytes per source; **`--force`** overrides dedup.

## 1. CLI & engine changes

### New flags

```
wiki.py ingest --dir <path> [--include-hidden] [--max-bytes N] [--force]
wiki.py ingest --file <path> [--normalized-content <md-file>]
                             [--parser-name <str>] [--force]
```

`--normalized-content`/`--parser-name` are valid for `--file` and `--url`
inputs; `--force` is valid for `--file`, `--url`, and `--dir`.
`--normalized-content` is rejected for `--dir` (per-file descriptions must go
through per-file ingests).

### `--dir` behavior (`wikicore/dirs.py` + `sources.py`)

- Deterministic recursive walk: sorted directory entries, stable ordering.
- Skip rules (recorded as `skipped` items in the receipt, with reason):
  - VCS/vendor dirs: `.git .svn .hg node_modules __pycache__ .venv venv dist
    build target vendor .idea .llm-wiki`
  - Hidden files/dirs (leading `.`) unless `--include-hidden`
  - Files larger than `--max-bytes` (default 50 MiB)
  - Non-evidence binaries by extension: `.zip .tar .tgz .gz .bz2 .xz .7z .rar
    .exe .dll .dylib .so .o .a .class .wasm .pyc`, and junk files by name
    (`.DS_Store`, `Thumbs.db`)
  - Symlinks (not followed)
  - Any path inside the project's `.llm-wiki/` directory
- Each member file is ingested through the existing `ingest()` path → own
  source_id, sha256 dedup, secrets scan, extraction report.
- `kind` remains `"file"` per member; each member manifest gets
  `root_origin: "dir:<abspath-of-dir>"` (schema already permits a string).
- Batch result written to `.llm-wiki/state/ingest-batches/<batch_id>.json` and
  printed:

```json
{
  "batch_id": "batch_...",
  "dir": "/abs/path",
  "counts": {"ingested": 12, "deduplicated": 3, "skipped": 40, "errors": 0},
  "items": [
    {"path": "docs/a.md", "source_id": "source_...", "version": 1,
     "status": "ingested"},
    {"path": "img.png", "status": "ingested",
     "coverage": {"text": "not_extracted"}},
    {"path": ".env", "status": "skipped", "reason": "hidden"},
    {"path": "big.bin", "status": "error", "error": "permission denied"}
  ]
}
```

- Partial failure: one unreadable file → `error` item; other files proceed.

### `--normalized-content`

After raw preservation and hashing, the agent-supplied markdown becomes
`content.md`. It takes precedence over markitdown output — the agent supplies
it exactly when it judged engine extraction absent or insufficient.

- Text passes through `_secret_gate` and `INJECTION_RE` like any normalized
  content — agent output is data too.
- Extraction report: `parser.name = <--parser-name or "agent">`,
  `coverage.text = "complete"`, `coverage.images = "complete"` when the source
  is an image (detected by extension), else left as recorded.
- Receipt gains `normalized_source: "agent"` vs `"markitdown"`/`"wikicore-raw"`.

### Dedup / `--force` matrix

| File state | Default | `--force` |
|---|---|---|
| Same abspath + same sha256, prior version text-extracted | `deduplicated: true`, skip | new version appended (same sha, new `version`) |
| Same abspath + same sha256, prior version `coverage.text == not_extracted` | if `--normalized-content` given: update `content.md` + extraction.json **in place**, receipt `updated_normalized: true`; else plain dedup | new version appended |
| Same abspath, changed sha256 | new version | new version |
| New path | version 1 | version 1 |

Rationale for in-place fill-in: a `not_extracted` version is incomplete
evidence; completing it is not a content change (sha256 of raw bytes is
unchanged) so it must not mint a new version. `--force` exists for genuine
re-extraction (e.g. markitdown installed after first ingest, fresh vision pass).

In-place fill-in consequence: `compile-plan` marks a source version compiled via
`.state.compiled[source_id]`. When `updated_normalized` fills a version that
was already compiled (i.e. compiled from empty content), the same transaction
removes that version from `.state.compiled`, so `compile-plan` picks it up
again. Claims previously extracted from the empty version are unaffected (there
were none extractable); any stale candidates reconcile normally.

### Format routing (engine-side)

Extension lists become routing hints, not gates. Classification order for
`--file`:

1. `OFFICE_EXTS` — `.pdf .docx .xlsx .pptx .doc .xls .ppt` → try markitdown;
   on failure `not_extracted` (skill falls back to agent lane).
2. Otherwise attempt UTF-8 decode: success → text path (decodes, secrets-scan,
   normalize). This replaces `TEXT_EXTS` allowlisting — `.md .csv .json .yaml
   .svg .log` and any unknown text extension just work.
3. Undecodable bytes → binary path: `not_extracted`, raw preserved.

`IMAGE_EXTS` (new) — `.png .jpg .jpeg .webp .gif .bmp .tiff .tif .heic .heif` —
drives two things: `coverage.images` in the extraction report, and the skill's
decision to route the file through the vision lane. `.svg` is XML text — it
decodes, routes text-side, and is NOT in `IMAGE_EXTS`.

### markitdown install

`install.sh` tries, in order: `uv tool install markitdown` →
`pipx install markitdown` → `pip3 install --user markitdown`. All failures →
warning, not install failure; engine falls back to `not_extracted` + agent lane
exactly as today. Detection stays `shutil.which("markitdown")`; also tried for
legacy `.doc/.xls/.ppt` (markitdown delegates where it can).

## 2. Skill recipe (wiki-ingest/SKILL.md)

Per-format routing table the agent follows:

| Input | Recipe |
|---|---|
| UTF-8-decodable text (`.md .txt .csv .tsv .json .yaml .html .svg .log`, any text ext) | `ingest --file` direct |
| `.docx .xlsx .pptx .doc .xls .ppt` | `ingest --file` (markitdown in engine); if receipt `coverage.text == not_extracted` → agent extracts text with its own tools, writes temp md, re-ingests `--normalized-content` |
| `.pdf` | markitdown first; on `not_extracted` agent extracts (read tool, `pdftotext`, vision for scanned pages) → `--normalized-content` |
| image ext | **vision lane**: agent views image, writes `desc.md` per template, then `ingest --file img --normalized-content desc.md --parser-name agent-vision` |
| unknown binary | `ingest --file` → surface `not_extracted` warning; stop unless user cares |
| directory | `ingest --dir`, then loop receipt items where `coverage.text == not_extracted` and ext ∈ image/office/pdf through the lanes above |

### Image description contract (desc.md)

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

Sections may be `(none)` where inapplicable. `parser.name` =
`agent-vision`; coverage `images: complete`, or `partial` + warning naming the
gap (EXTRACTION.md honesty rule applies — agent-written coverage reports too).

### Folder lane flow

1. `wiki.py ingest --dir <path>` — engine ingests everything it can
   deterministically; batch receipt marks binaries markitdown-parsed or
   `not_extracted`.
2. Agent loops receipt items where `coverage.text == "not_extracted"`:
   - image → vision lane; pdf/office → extraction lane.
   - on dedup'd-but-raw sources the fill-in lands in place
     (`updated_normalized`); on `--force` a new version is minted.
3. Agent reports the merged summary: ingested, deduplicated, skipped,
   vision-extracted, errors.

## 3. Non-goals

- No vision calls inside wikicore (deterministic, no network, no model access).
- No OCR engine or PDF parser vendored into the repo — markitdown is the only
  extraction dependency, and it stays optional at runtime.
- No `kind: "directory"` bundle manifest — per-file sources only; the batch
  receipt is the grouping record.
- No changes to compile/reconcile: `content.md` produced by
  `--normalized-content` feeds the existing compile-plan pipeline unchanged.

## 4. Error handling

| Case | Behavior |
|---|---|
| markitdown missing | engine `not_extracted`; skill falls back to agent lane |
| `--normalized-content` unreadable/missing | TxnError, exit 3, nothing staged |
| `--normalized-content` on text input | accepted (agent may re-normalize), recorded `normalized_source: "agent"` |
| Secret in agent-written content | `_secret_gate` applies: warn/redact/deny per config |
| Injection-like text in desc.md | `possible_prompt_injection` warning; stored as evidence |
| Unreadable file in --dir | `error` item, batch continues |
| Empty dir / nothing ingested | batch receipt with zero counts; not an error |
| `--dir` on a file path | TxnError, exit 3 |

## 5. Testing

Deterministic unit tests in `skill/llm-wiki/scripts/tests/`:

- `--dir` walk: nested tree → N sources; skip rules fire (hidden, vendor,
  oversize, symlinks); ordering deterministic; batch receipt written.
- Dedup matrix: re-ingest dir → all `deduplicated`; `--force` → new versions;
  changed file → new version, others dedup'd.
- `--normalized-content`: stored as content.md; extraction report shows
  `parser.name == agent-vision`; secrets/injection scan applied to it.
- Fill-in path: ingest png (not_extracted) → re-ingest same bytes with
  `--normalized-content` → `updated_normalized: true`, version unchanged,
  `compile-plan` now sees content.
- Extension routing: unknown-extension UTF-8 file → text; `.webp` → binary.
- `--dir` on a file, missing dir → exit 3.

Vision/model steps are not unit-tested; they are skill-procedure steps verified
by the existing eval harness pattern (semantic evals), not new test
infrastructure.
