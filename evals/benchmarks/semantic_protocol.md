# Two-arm semantic protocol (PRD §60 baseline comparison)

Arms answer the same 8 scenarios over the same golden-corpus copy:

## Arm A — without the skill (plain markdown + grep)
You are given a project directory (evals/golden-corpus copy). Use ONLY shell
text search (rg/grep) and file reading to answer. For each scenario, output
one JSONL row: {"arm":"grep","scenario":"<id>","answerable":bool,"answer":
"<answer>","citations":["<relpath>:<line>"],"effort_notes":"<searches run>"}.
Cite exact file and line for every factual claim. If the corpus cannot
answer, set answerable=false and say what is missing. Do not guess.

## Arm B — with the skill (wiki-built project)
You are given the same corpus WITH a built .llm-wiki/ (claims compiled and
corroborated). Engine = the path `skill/llm-wiki/scripts/wiki.py` in this
repo checkout (relative to the project root you run from). Use ONLY these
read commands (run from the project root):
  python3 skill/llm-wiki/scripts/wiki.py query-prepare --question "..." [--as-of YYYY-MM-DD]
  python3 skill/llm-wiki/scripts/wiki.py context-pack --task "..." --budget 6000
  python3 skill/llm-wiki/scripts/wiki.py page-show --artifact wiki/<kind>/<slug>.md
Answer from query-prepare matches; cite claim ids. Same JSONL shape with
"arm":"wiki".

## Scenarios (ids fixed)
1 direct-recall — Which database does the order service use?
2 synthesis — Why do we use transactional outbox?
3 correction — What is the current attribution window? (expect 7d)
4 historical — What was the attribution window before September 2026?
  (expect 30d, cite the memo)
5 scope — Does staging use the same attribution window as prod?
6 conflict — How many retries are configured? (must present both 3 and 5)
7 unanswerable — What is our 2028 pricing plan? (must refuse)
8 isolation — (two-corpus check; scorer verifies no cross citations)

Scoring is mechanical (score_semantic.py): citations must resolve; expected
values must appear; refusals must be explicit. No LLM judgment calls.
