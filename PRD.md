# Product Requirements Document

# OMP LLM Wiki — Evidence-Backed Project Memory for Coding Agents

**Version:** v0.3
**Status:** Proposed architecture
**Primary runtime:** Oh My Pi
**Compatibility goal:** Agent Skills-compatible coding agents
**Storage model:** Local-first filesystem
**Canonical representation:** Markdown + structured YAML/JSON metadata
**Development methodology:** Eval-driven, contract-first development

---

# 1. Product Definition

OMP LLM Wiki is a **local-first project memory system for coding agents**.

It converts documents, repositories, URLs, work sessions, human notes, and agent discoveries into an evidence-backed knowledge model that:

1. preserves original evidence;
2. extracts explicit claims;
3. reconciles corrections and contradictions;
4. records project decisions separately from observations;
5. incrementally builds readable wiki views;
6. supplies agents with bounded, task-specific context;
7. preserves historical knowledge;
8. explains where every important fact came from.

The primary user is:

> A developer using coding agents repeatedly across many sessions on the same project.

The primary job-to-be-done is:

> **“Resume this project, recover the decisions and constraints that matter, understand what changed, and continue without rediscovering everything.”**

---

# 2. Core Thesis

Most agent-memory systems optimize retrieval.

OMP Wiki should optimize **knowledge continuity**.

Traditional:

```text
Question
    ↓
Search documents
    ↓
Retrieve chunks
    ↓
Reconstruct context
    ↓
Reason
    ↓
Answer
    ↓
Forget synthesis
```

OMP Wiki:

```text
Evidence
    ↓
Extract claims
    ↓
Reconcile
    ↓
Accept / dispute / supersede
    ↓
Compile durable knowledge
    ↓
Generate bounded context
    ↓
Agent works
    ↓
New evidence
```

The important loop is not:

```text
document → embeddings
```

but:

```text
evidence
→ interpretation
→ accepted knowledge
→ derived views
→ correction
→ propagation
```

---

# 3. Fundamental Information Model

The system MUST distinguish four layers.

```text
┌──────────────────────────────┐
│          EVIDENCE            │
│ What did a source say?       │
└───────────────┬──────────────┘
                │ supports
                ▼
┌──────────────────────────────┐
│           CLAIM              │
│ What does it mean?           │
└───────────────┬──────────────┘
                │ accepted as
                ▼
┌──────────────────────────────┐
│     PROJECT KNOWLEDGE        │
│ Decisions / accepted facts   │
└───────────────┬──────────────┘
                │ rendered into
                ▼
┌──────────────────────────────┐
│       DERIVED VIEWS          │
│ Wiki / context / indexes     │
└──────────────────────────────┘
```

This is the most important invariant in the product.

## Evidence

Represents what a specific source version contains.

Evidence is immutable.

Examples:

* paragraph in an ADR
* row in a spreadsheet
* section in a PDF
* code in a repository revision
* explicit user statement
* agent-session transcript
* API response

Evidence does not automatically become truth.

---

## Claim

A structured interpretation of evidence.

Example:

```yaml
subject: analytics.order_attribution
predicate: click_lookback_window
value: 7
unit: days
```

A claim may be:

```text
candidate
accepted
disputed
superseded
rejected
```

---

## Decision

A decision represents what the project deliberately chose.

Examples:

```text
Use PostgreSQL instead of DynamoDB.

Production attribution uses a seven-day window.

Use transactional outbox for order events.
```

A decision may reference claims and evidence but has different semantics.

The system MUST never infer:

```text
repeated claim == project decision
```

---

## Derived View

Generated representations:

```text
wiki page
project overview
topic summary
search index
graph
context pack
change briefing
memory palace view
```

Derived views are rebuildable.

They are not authoritative storage.

---

# 4. Authority Model

Every knowledge object has an authority class.

Recommended ordering:

```text
explicit human decision
        ↓
authoritative project artifact
        ↓
verified implementation state
        ↓
corroborated evidence
        ↓
single-source evidence
        ↓
agent inference
```

Example:

```yaml
authority:
  type: explicit_project_decision
  source: ADR-019
```

Authority affects reconciliation.

It MUST NOT be represented as an opaque numerical “truth confidence.”

Prefer:

```text
evidence
authority
scope
status
acceptance reason
conflicts
```

over:

```text
confidence: 0.92
```

---

# 5. Temporal Knowledge Model

Project knowledge changes.

The system therefore distinguishes:

```text
valid time
```

from:

```text
recorded time
```

Example:

```yaml
valid_from: 2026-09-01
valid_to: null

recorded_at: 2026-09-14T10:32:00Z
```

Meaning:

> The rule became valid September 1, but the wiki learned about it September 14.

This allows correct answers to:

```text
What is the current attribution window?

What was the attribution window in August?

When did we discover that it changed?
```

---

# 6. Knowledge Scope

Claims MUST support scope.

Example:

```yaml
scope:
  project: shop-platform
  environment: production
  service: analytics
  model: orders_attribution
```

Without scope, apparently contradictory statements may actually both be correct.

Example:

```text
production → 7 days
sandbox    → 30 days
```

These must not be reconciled as a contradiction.

---

# 7. Evidence Independence

The system MUST distinguish:

```text
number of references
```

from:

```text
number of independent evidence origins
```

Example:

```text
Original ADR
    ↓
Wiki summary
    ↓
Meeting notes quoting wiki
    ↓
Agent session quoting meeting notes
```

This is:

```text
4 references
```

but approximately:

```text
1 independent origin
```

Repeated retrieval of system-generated material MUST NOT increase corroboration.

Each evidence object therefore stores ancestry.

Example:

```yaml
provenance:
  root_origin: source_0017
  derived_from:
    - source_0017
```

---

# 8. Core Product Principles

## 8.1 Evidence is immutable

Never silently rewrite historical evidence.

New information creates:

```text
new source version
```

or:

```text
new evidence object
```

---

## 8.2 Derived knowledge is disposable

Everything under:

```text
wiki/
context/
views/
indexes/
```

must be rebuildable from canonical evidence and claim state.

---

## 8.3 Agent inference is explicitly labeled

Agents may synthesize.

They may not silently convert inference into authoritative project truth.

---

## 8.4 Correction beats accumulation

A useful memory system must be able to forget incorrect derived knowledge.

Correction propagation is therefore a P0 capability.

---

## 8.5 Context is assembled, not dumped

Never load the complete wiki into every agent session.

Generate bounded task-specific context.

---

## 8.6 Deterministic mechanisms should remain deterministic

Use scripts for:

```text
hashing
IDs
parsing
locking
dependency tracking
schema validation
indexes
state transitions
```

Use agents where semantic judgment is actually required.

---

## 8.7 Progressive disclosure

```text
Skill metadata
     ↓
SKILL.md
     ↓
Wiki index
     ↓
Relevant knowledge
     ↓
Claims
     ↓
Evidence
     ↓
Raw source
```

Each level is loaded only when required.

---

# 9. Filesystem Architecture

```text
project/
│
├── .omp/
│   ├── skills/
│   └── commands/
│
└── .llm-wiki/
    │
    ├── wiki.yaml
    ├── README.md
    │
    ├── raw/
    │   ├── files/
    │   ├── urls/
    │   ├── repositories/
    │   ├── sessions/
    │   └── text/
    │
    ├── sources/
    │   └── <source-id>/
    │       ├── manifest.yaml
    │       ├── content.md
    │       ├── extraction.json
    │       └── assets/
    │
    ├── claims/
    │   └── ...
    │
    ├── decisions/
    │   └── ...
    │
    ├── notes/
    │
    ├── wiki/
    │   ├── index.md
    │   ├── overview.md
    │   ├── concepts/
    │   ├── entities/
    │   ├── decisions/
    │   ├── procedures/
    │   ├── questions/
    │   └── changes/
    │
    ├── context/
    │
    ├── views/
    │
    └── .state/
        ├── manifest.json
        ├── dependencies.json
        ├── aliases.json
        ├── graph.json
        ├── operations.jsonl
        ├── review-queue.jsonl
        ├── locks/
        └── staging/
```

---

# 10. Canonical vs Derived Storage

Canonical:

```text
raw/
sources/
claims/
decisions/
notes/
```

Derived:

```text
wiki/
context/
views/
.state/indexes
graph
```

Important rule:

> Deleting all derived directories and recompiling MUST recover the same semantic project state.

Exact generated prose may differ.

Semantic state must remain equivalent.

---

# 11. Claim Schema

Example:

```yaml
id: claim_01JABC

subject: project.analytics.attribution

predicate: click_lookback_window

value: 7
unit: days

scope:
  environment: production
  model: orders_attribution

valid_from: 2026-09-01
valid_to: null

recorded_at: 2026-09-14T10:00:00Z

status: accepted

authority:
  type: explicit_project_decision

evidence:
  - source_id: source_0018
    source_version: 3
    locator:
      type: heading
      value: "Attribution Window"

supersedes:
  - claim_0017

acceptance:
  reason: explicit_project_decision
  accepted_at: 2026-09-14T10:05:00Z
```

---

# 12. Evidence Locator

Evidence references should be as precise as practical.

Supported locators:

```text
Markdown heading
line range
PDF page + bounding box
spreadsheet sheet + cell range
PowerPoint slide
repository path + line range
URL section
transcript timestamp
```

Example:

```yaml
locator:
  type: spreadsheet_range
  sheet: Configuration
  range: B12:C12
```

This lets users inspect the actual basis of a claim.

---

# 13. Source Extraction Report

Every non-trivial ingestion creates an extraction report.

Example:

```yaml
source_id: architecture_pdf

coverage:
  text: complete
  tables: partial
  images: preserved
  diagrams: not_interpreted
  formulas: complete

warnings:
  - page 14 diagram was preserved as image but not semantically extracted

parser:
  name: docling
  version: ...
```

Extraction failure MUST NOT be silent.

Especially important for:

```text
PDF
Excel
PowerPoint
images
scanned documents
diagrams
formulas
```

---

# 14. Source Versioning

Separate:

```text
source identity
```

from:

```text
source version
```

Example:

```text
architecture.pdf
    │
    ├── version 1 → hash A
    ├── version 2 → hash B
    └── version 3 → hash C
```

Same source origin:

```text
same source_id
```

Changed bytes:

```text
new version
```

---

# 15. Dependency Graph

The system maintains dependencies between:

```text
source versions
claims
decisions
wiki pages
indexes
context packs
views
```

Example:

```text
source_18/v3
       ↓
claim_42
       ↓
decision_9
       ↓
concept/attribution
       ↓
context_pack_118
```

When `claim_42` changes:

```text
mark descendants stale
```

not:

```text
rebuild everything
```

---

# 16. Correction Propagation

Correction propagation is a flagship capability.

Example:

Old accepted knowledge:

```text
click window = 30 days
```

New evidence:

```text
production changed to 7 days from September 1
```

Pipeline:

```text
NEW EVIDENCE
      ↓
extract candidate claim
      ↓
resolve scope
      ↓
compare temporal validity
      ↓
detect relationship with existing claim
      ↓
classify
```

Possible relationships:

```text
CORRECTION
POLICY_CHANGE
SCOPE_DIFFERENCE
CONTRADICTION
CORROBORATION
DUPLICATE
UNRELATED
```

If policy change:

```text
claim_old.valid_to = 2026-08-31
claim_new.valid_from = 2026-09-01
```

If correction:

```text
claim_old.status = superseded
claim_new supersedes claim_old
```

Then:

```text
find dependent artifacts
→ invalidate
→ rebuild
```

---

# 17. Derived Artifact Invalidation

Every context pack, summary, wiki page, or index records its dependency set.

Example:

```yaml
derived_from:
  claims:
    - claim_42@3
    - claim_77@1

generated_at: ...

compiler_version: ...
```

If any dependency changes:

```text
artifact = STALE
```

It must not silently remain current.

---

# 18. Context Packs

`/wiki-context` becomes a P0 capability.

Purpose:

> Generate the smallest useful context package for a specific agent task.

Example:

```text
/wiki-context "implement order attribution fix" --budget 6000
```

Context pack:

```text
Task
Current project state
Relevant constraints
Relevant decisions
Relevant concepts
Relevant implementation locations
Known failed approaches
Open questions
Evidence references
```

---

# 19. Context Budget

Context packs MUST respect explicit budgets.

Example:

```yaml
budget:
  max_tokens: 6000
```

Selection priority:

```text
1. critical constraints
2. accepted decisions
3. directly relevant claims
4. relevant implementation references
5. unresolved conflicts
6. related concepts
7. supplementary evidence
```

The assembler stops when budget is exhausted.

---

# 20. Context Receipt

Every context pack produces a receipt.

```yaml
context_id: context_01J...

task: implement attribution update

generated_at: ...

budget:
  requested: 6000
  estimated: 5740

claims:
  - claim_42@3
  - claim_81@1

decisions:
  - decision_09@2

sources:
  - source_18@3

wiki_pages:
  - concepts/attribution.md
```

This enables debugging:

> Which version of project knowledge did the coding agent actually receive?

---

# 21. Session Continuity

At the beginning of a new coding session, OMP Wiki should support:

```text
/wiki-context --resume
```

Output:

```text
Project snapshot
What changed since last context receipt
Current decisions
Current constraints
Outstanding work
Recently superseded assumptions
Known problems
Open questions
```

This is more useful than merely retrieving “similar memories.”

---

# 22. Change Briefing

Built-in derived view:

```text
/wiki-query "what changed since my previous session?"
```

or:

```text
/wiki-context --changes-since <context-id>
```

Report:

```text
New decisions
Changed decisions
Superseded claims
New contradictions
Resolved contradictions
Changed implementation assumptions
New unresolved questions
```

---

# 23. Negative Knowledge

The system should preserve useful failures.

Example:

```yaml
type: failed_approach

approach:
  use Redis pub/sub for event durability

result:
  rejected

reason:
  messages are not durable

scope:
  order-event-delivery

evidence:
  ...
```

Negative knowledge MUST be scoped.

Avoid converting:

```text
failed once
```

into:

```text
never do this
```

---

# 24. Knowledge Debt

OMP Wiki should expose unresolved knowledge quality issues as actionable debt.

Examples:

```text
unsupported accepted claim
unresolved contradiction
stale authoritative source
orphan decision
low extraction coverage
missing source locator
unknown effective date
duplicate concept
unreviewed candidate
```

`/wiki-doctor` surfaces them.

---

# 25. Review Queue

Semantic uncertainty should not be hidden.

Store:

```text
.state/review-queue.jsonl
```

Review item types:

```text
possible contradiction
possible correction
ambiguous scope
authority conflict
duplicate identity
low extraction quality
candidate decision
source retraction impact
```

Each review item contains:

```text
problem
evidence
proposed resolution
affected knowledge
risk
```

---

# 26. Human Review Skill

Command:

```text
/wiki-review
```

Examples:

```text
/wiki-review
/wiki-review conflicts
/wiki-review stale
/wiki-review claim_42
```

Actions:

```text
accept
reject
merge
mark duplicate
mark authoritative
mark superseded
set scope
set validity interval
defer
```

Human review becomes part of the normal lifecycle rather than an emergency fallback.

---

# 27. Command Surface

Recommended P0:

```text
/wiki-init
/wiki-ingest
/wiki-compile
/wiki-query
/wiki-context
/wiki-review
/wiki-doctor
```

Optional convenience aliases:

```text
/wiki-status
/wiki-lint
```

but conceptually:

```text
wiki-doctor = diagnostics
wiki-review = semantic resolution
```

---

# 28. `/wiki-init`

Responsibilities:

```text
discover project
validate current workspace
create configuration
install schemas
install templates
initialize state
initialize indexes
run diagnostics
```

Properties:

```text
idempotent
migration-aware
non-destructive
```

---

# 29. `/wiki-ingest`

Contract:

> Preserve and normalize evidence.

It MUST:

```text
identify source
capture version
preserve original
normalize
generate extraction report
register evidence
return receipt
```

It MUST NOT:

```text
accept project claims
make decisions
rewrite wiki pages
```

---

# 30. `/wiki-compile`

Contract:

> Convert pending evidence into candidate claims, reconcile them with existing project knowledge, and rebuild only affected derived artifacts.

Pipeline:

```text
pending source versions
        ↓
source analyzers
        ↓
candidate claims
        ↓
identity + scope resolution
        ↓
reconciliation
        ↓
accepted/disputed/review
        ↓
dependency invalidation
        ↓
wiki compilation
        ↓
indexes
        ↓
validation
```

---

# 31. Candidate Claims

Extraction produces candidates first.

Example:

```yaml
status: candidate
```

A candidate becomes accepted based on policy.

Possible acceptance bases:

```text
explicit_project_decision
authoritative_source
human_review
implementation_verification
independent_corroboration
```

---

# 32. Automatic Acceptance Policy

Safe automatic acceptance might include:

```text
explicit user decision
signed project ADR
current configuration file
verified implementation fact
```

Other claims may remain:

```text
candidate
```

or:

```text
provisional
```

until corroborated or reviewed.

Policy is project configurable.

---

# 33. `/wiki-query`

Query architecture:

```text
QUESTION
   ↓
interpret scope/time
   ↓
indexes
   ↓
accepted knowledge
   ↓
related claims
   ↓
conflicts
   ↓
evidence expansion
   ↓
raw evidence if necessary
```

Default query is read-only.

---

# 34. Query Answer Contract

Answers should contain logically:

```text
Answer
Evidence
Scope
Historical applicability
Unresolved conflicts
Knowledge gaps
```

For simple answers, the UI may render only relevant parts.

---

# 35. Unsupported Questions

If the wiki does not contain sufficient evidence:

```text
I don't have enough project evidence to answer this reliably.
```

Then optionally state:

```text
what is known
what is missing
what evidence would resolve it
```

Never fabricate project state from generic knowledge.

---

# 36. Historical Query

Support queries such as:

```text
What was our deployment strategy in May?

Why did attribution change?

What did we believe before ADR-019?

When did this assumption stop being valid?
```

These operate against temporal claims.

---

# 37. `/wiki-doctor`

Replaces a collection of fragmented maintenance commands.

Static diagnostics:

```text
broken references
invalid schema
orphan source
invalid IDs
missing dependency
corrupt index
stale derived artifact
failed extraction
incomplete transaction
lock issue
```

Semantic diagnostics:

```text
unsupported accepted claim
unresolved contradiction
possible duplicate
missing scope
missing effective date
stale decision
```

---

# 38. Transaction Model

Every mutating operation follows:

```text
PLAN
  ↓
STAGE
  ↓
VALIDATE
  ↓
COMMIT
```

Not:

```text
modify files incrementally and hope
```

Staging:

```text
.state/staging/<run-id>/
```

Only after validation:

```text
atomic commit
```

---

# 39. Run Receipt

Every command produces:

```yaml
run_id: run_01J...

command: wiki-compile
skill_version: 0.3.0

started_at: ...
finished_at: ...

inputs:
  sources: 4

changes:
  claims_created: 5
  claims_updated: 1
  pages_updated: 3

warnings: []

conflicts:
  - review_019

revision:
  before: ...
  after: ...
```

---

# 40. Concurrency Model

Coding agents may run concurrently.

The system should permit:

```text
parallel source ingestion
parallel source analysis
parallel read-only queries
```

while protecting semantic writes.

Recommended write model:

```text
worker
   ↓
staged proposal
   ↓
revision check
   ↓
commit coordinator
   ↓
atomic commit
```

No worker directly overwrites canonical shared state.

---

# 41. Optimistic Concurrency

Every semantic write references:

```text
base_revision
```

Before committing:

```text
if current_revision != base_revision
    reject stale commit
```

Then:

```text
rebase/recompile
```

This prevents agents from silently overwriting one another.

---

# 42. Lock Granularity

P0:

```text
global semantic commit lock
```

while allowing concurrent ingestion.

Later:

```text
claim-level
page-level
namespace-level
```

locking may improve throughput.

Prefer correctness first.

---

# 43. Agent Skill Architecture

```text
USER
 │
 ▼
Wiki Facade
 │
 ├── wiki-init
 ├── wiki-ingest
 ├── wiki-compile
 ├── wiki-query
 ├── wiki-context
 ├── wiki-review
 └── wiki-doctor
 │
 ▼
Contracts
 │
 ▼
Domain Services
 │
 ├── Source Registry
 ├── Claim Store
 ├── Reconciler
 ├── Dependency Graph
 ├── Context Assembler
 └── Review Queue
 │
 ▼
Adapters / Scripts
 │
 ▼
Filesystem
```

---

# 44. Thin Skill Principle

`SKILL.md` should not contain the whole implementation.

It defines:

```text
purpose
when to trigger
when not to trigger
contract
workflow
decision rules
resource routing
failure behavior
```

Scripts provide deterministic behavior.

References provide deeper knowledge.

Schemas define machine contracts.

---

# 45. Skill Package

Example:

```text
wiki-compile/
├── SKILL.md
├── contract.yaml
│
├── references/
│   ├── RECONCILIATION.md
│   ├── TEMPORAL.md
│   └── AUTHORITY.md
│
├── schemas/
│   ├── candidate-claim.schema.json
│   └── reconcile-plan.schema.json
│
├── scripts/
│   ├── pending.py
│   ├── dependencies.py
│   └── commit.py
│
├── examples/
│
└── evals/
```

---

# 46. Shared `wiki-core`

```text
wiki-core/
├── SKILL.md
│
├── references/
│   ├── ARCHITECTURE.md
│   ├── CONVENTIONS.md
│   ├── AUTHORITY.md
│   ├── PROVENANCE.md
│   ├── TEMPORAL.md
│   ├── SECURITY.md
│   └── EVALUATION.md
│
├── schemas/
│
├── templates/
│
└── scripts/
```

Individual skills should not duplicate shared policies.

---

# 47. Source Adapters

Interface conceptually:

```text
SourceAdapter

supports(input)
acquire(input)
normalize(input)
extract_locators()
quality_report()
```

P0 adapters:

```text
Text
Markdown
URL
PDF
DOCX
XLSX
PPTX
Directory
Repository
Session
```

---

# 48. Parser Strategy

Rich documents:

```text
Docling
```

as preferred parser where appropriate.

Alternative/fallback:

```text
MarkItDown
```

Parser choice is an implementation strategy.

The rest of the architecture only consumes normalized source contracts.

---

# 49. Security Boundary

All imported content is untrusted data.

Document text such as:

```text
Ignore all previous instructions...
```

has zero authority over:

```text
agent policy
skill policy
tool permissions
project configuration
review policy
```

Content instructions are evidence, not executable instructions.

---

# 50. Secret Handling

Before durable ingestion:

```text
detect likely secrets
```

Examples:

```text
private keys
tokens
password assignments
.env
URL credentials
high-entropy API secrets
```

Policy:

```text
deny
warn
redact
allow
```

must be configurable.

Secret values must never be copied into operation logs.

---

# 51. Project Isolation

Each project has independent:

```text
source registry
claim store
decisions
configuration
indexes
context packs
review queue
```

Cross-project retrieval is disabled unless explicitly enabled.

This is a critical evaluation scenario.

---

# 52. Memory Palace / Visual Layer

The memory palace is not canonical storage.

It is a view over the same knowledge model.

Mapping:

```text
Room       → topic/project area
Landmark   → pinned decision/concept
Object     → claim/evidence
Door       → relationship
Route      → reading path
History    → temporal state
Inspection → source evidence
```

Start with:

```text
2D graph / spatial board
```

before considering 3D.

Placement metadata belongs under:

```text
views/
```

Never encode spatial position into knowledge identity.

---

# 53. Skill Candidate Generation — P2

Repeated successful procedures may become:

```text
skill candidates
```

Example:

```text
Agent repeatedly performs same deployment diagnostic sequence.
```

OMP Wiki may suggest:

```text
Candidate reusable procedure detected.
```

Promotion into executable skill requires:

```text
explicit review
contract
tests
evals
```

Knowledge must not autonomously become executable behavior.

---

# 54. Evaluation Philosophy

Evaluation happens at several layers.

```text
mechanical correctness
semantic correctness
memory correctness
retrieval usefulness
context efficiency
safety
```

Do not collapse these into one score.

---

# 55. Eval Categories

## Contract evals

Did a skill respect its boundary?

Example:

```text
/wiki-ingest must not modify claims.
```

---

## Deterministic tests

Examples:

```text
hash identity
source deduplication
version tracking
schema validation
dependency invalidation
context budget
transaction rollback
revision conflicts
```

---

## Semantic evals

Examples:

```text
correct claim extraction
scope resolution
correction classification
contradiction preservation
historical interpretation
```

---

## Trigger evals

Does the correct skill activate?

---

## Adversarial evals

Examples:

```text
prompt injection document
fake authority
malicious source
circular provenance
duplicated summaries
```

---

## Performance evals

Measure:

```text
token usage
context loaded
execution duration
affected pages
unnecessary rewrites
```

---

# 56. Golden Evaluation Corpus

Use one realistic software project.

Include:

```text
README
architecture docs
ADRs
source code
config
requirements
spreadsheet
PDF
meeting notes
session transcript
```

Intentionally contain:

```text
duplicate terms
conflicting statements
historical change
scope differences
one explicit correction
one outdated ADR
one failed approach
one unanswerable question
```

---

# 57. Core Memory Evaluation Scenarios

P0 suite:

### Direct recall

```text
Which database does the order service use?
```

### Cross-source synthesis

```text
Why do we use transactional outbox?
```

### Correction

```text
What is the current attribution window?
```

### Historical

```text
What was it before September?
```

### Scope

```text
Does staging use the same configuration?
```

### Conflict

```text
How many retries are configured?
```

### Unanswerable

```text
What is our 2028 pricing plan?
```

### Project isolation

Ensure project A does not leak into project B.

---

# 58. Critical Release Gates

100% required:

```text
Every emitted citation resolves.

Same source bytes do not create duplicate versions.

Corrections invalidate all dependent artifacts.

Concurrent stale commits are detected.

Prompt injection sources cannot change skill behavior.

Project isolation holds.

Context packs never exceed configured hard budget.

Failed transactions do not partially commit.

Retraction invalidates all descendants.
```

---

# 59. Semantic Quality Gates

Initial target:

```text
claim extraction accuracy       ≥ 95%
scope resolution                ≥ 95%
correction classification       ≥ 95%
historical answer correctness   ≥ 95%
unsupported answer refusal      ≥ 98%
citation correctness            100%
```

These numbers are starting targets and should be calibrated against real fixtures.

---

# 60. Baseline Comparison

OMP Wiki should be evaluated against:

```text
plain Markdown documents
+
one manually maintained PROJECT.md summary
+
basic text search
```

The system should demonstrate measurable improvement in:

```text
recall
correction handling
historical accuracy
source traceability
context efficiency
continuation between sessions
```

If it does not outperform this baseline, architectural complexity is not justified.

---

# 61. Product Success Metrics

For an MVP pilot:

### Memory correctness

```text
≥95% answers supported by correct current/historical evidence
```

### Correction propagation

```text
100% tested dependent artifacts invalidated
```

### Context efficiency

Target:

```text
substantially fewer tokens than full-document loading
```

while preserving task performance.

### Session continuation

Developer can resume after multiple sessions without manually reconstructing project state.

### Inspectability

User can reach the underlying evidence for important answers in at most a few navigation steps.

---

# 62. Non-Goals — MVP

Do not build yet:

```text
3D palace
hosted multi-user SaaS
mandatory vector database
graph database
autonomous skill promotion
full enterprise permissions
real-time collaboration
global personal memory
complex ontology editor
```

---

# 63. MVP Scope

Ship:

```text
/wiki-init
/wiki-ingest
/wiki-compile
/wiki-query
/wiki-context
/wiki-review
/wiki-doctor
```

With:

```text
filesystem storage
source versions
claim ledger
decision objects
provenance
temporal validity
dependency invalidation
context receipts
review queue
transactional writes
concurrency protection
core eval suite
```

---

# 64. MVP Implementation Order

## M0 — Contracts and fixtures

Build before product logic:

```text
source schema
claim schema
decision schema
context receipt schema
run receipt schema
golden corpus
eval runner
```

---

## M1 — Evidence layer

Build:

```text
/wiki-init
/wiki-ingest

source identity
source versions
raw preservation
normalization
extraction quality reports
```

---

## M2 — Claim layer

Build:

```text
candidate extraction
claim store
scope
valid time
recorded time
provenance ancestry
```

---

## M3 — Reconciliation

Build:

```text
/wiki-compile

duplicate resolution
corroboration
scope difference
contradiction
correction
policy change
supersession
review queue
```

---

## M4 — Derived wiki

Build:

```text
concept pages
decision pages
overview
indexes
dependency graph
incremental invalidation
```

---

## M5 — Retrieval

Build:

```text
/wiki-query
/wiki-context

historical query
conflict-aware answer
context budget
context receipts
resume briefing
```

---

## M6 — Reliability

Build:

```text
/wiki-review
/wiki-doctor

staging
atomic commits
locks
optimistic revisions
rollback
security evals
regression suite
```

---

# 65. Architecture Invariants

The implementation MUST preserve these rules.

### Invariant 1

```text
Evidence != Claim
```

### Invariant 2

```text
Claim != Decision
```

### Invariant 3

```text
Canonical knowledge != Generated wiki prose
```

### Invariant 4

```text
Repeated derivatives != Independent evidence
```

### Invariant 5

```text
Newest != Automatically correct
```

### Invariant 6

```text
Correction must propagate to every derived artifact
```

### Invariant 7

```text
Queries are read-only unless explicitly learning
```

### Invariant 8

```text
Imported content never gains execution authority
```

### Invariant 9

```text
No semantic commit without validation
```

### Invariant 10

```text
Agent context must be bounded
```

---

# 66. Architectural Boundary

```text
                    DETERMINISTIC
                         │
                         ▼
SOURCE → ACQUIRE → PRESERVE → NORMALIZE
                         │
═════════════════════════╪═════════════════════════
                         │
                    AGENT REASONING
                         │
                         ▼
                 EXTRACT CLAIMS
                         │
                         ▼
                    RECONCILE
                         │
                         ▼
                PROPOSE CHANGES
                         │
═════════════════════════╪═════════════════════════
                         │
                    DETERMINISTIC
                         │
                         ▼
               VALIDATE → COMMIT
                         │
                         ▼
                 INVALIDATE DAG
                         │
                         ▼
               REBUILD DERIVATIVES
```

This boundary should remain stable even if models, parsers, retrieval engines, and agent runtimes change.

---

# 67. Long-Term Architecture

```text
                       Claude Code
                           │
                  Codex ───┼─── OpenCode
                           │
                        Oh My Pi
                           │
                           ▼
                   ┌───────────────┐
                   │   OMP Wiki    │
                   └───────┬───────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
           Evidence      Claims      Decisions
              │            │            │
              └────────────┼────────────┘
                           ▼
                   Derived Knowledge
                           │
             ┌─────────────┼─────────────┐
             ▼             ▼             ▼
           Wiki          Context       Views
```

The long-lived product is the **knowledge model and contracts**, not the UI or the specific coding-agent host.

---

# 68. Engineering Doctrine

Prioritize decisions in this order:

```text
Correct information model
        ↓
Explicit contract
        ↓
Evidence and provenance
        ↓
Deterministic mechanism
        ↓
Semantic agent reasoning
        ↓
Transactional mutation
        ↓
Progressive disclosure
        ↓
Evaluation
        ↓
Optimization
```

The framework follows five rules:

> **Evidence before synthesis.**

> **Claims before prose.**

> **Corrections before accumulation.**

> **Context under budget.**

> **Evaluation before intuition.**

---

# 69. Product Moat Hypothesis

The differentiator is not:

```text
better vector search
```

and not:

```text
a prettier wiki
```

The hypothesis is:

> **Portable, inspectable project memory that correctly propagates changes and gives coding agents small, trustworthy context packs across long-running work.**

The hardest capability—and therefore the capability worth validating first—is:

```text
old knowledge
      +
new evidence
      ↓
correct semantic update
      ↓
historical state preserved
      ↓
all dependent memory corrected
      ↓
next coding agent receives only current relevant context
```

If OMP Wiki reliably solves that loop, it becomes significantly more valuable than ordinary agent memory or document retrieval.
