# shop-platform

Order management and analytics platform for the shop product line.

- `services/orders` — order service (Python/FastAPI)
- `services/analytics` — attribution and reporting
  (`services/analytics/attribution.py` holds the production
  `CLICK_LOOKBACK_DAYS = 7` constant, effective 2026-09-01)
- `config/` — per-environment configuration
- `docs/` — architecture decision records + `spec-attribution-window.pdf`
  (the spec PDF confirms the corrected 7-day production window)
- `notes/` — memos, meeting notes, postmortems
- `transcripts/` — agent session transcripts
- `data/` — config matrix exports

Key docs: [ADR-001](docs/adr-001-postgres.md),
[ADR-007](docs/adr-007-retries.md),
[ADR-019](docs/adr-019-attribution.md).

## Corpus file list

| Path | Role |
|------|------|
| `docs/adr-001-postgres.md` | Postgres decision (accepted) |
| `docs/adr-007-retries.md` | retry policy: 3 retries (conflicts with prod config) |
| `docs/adr-019-attribution.md` | 30-day window decision (superseded for prod) |
| `docs/spec-attribution-window.pdf` | binary spec: 7-day window effective 2026-09-01 |
| `notes/memo-2026-09-01-window-change.md` | the correction: prod 30→7 days |
| `notes/meeting-2026-09-10.md` | stale 30-day recap + outbox decision (dated AFTER the memo — trap) |
| `notes/failed-redis-pubsub.md` | negative knowledge, scoped to order-event-delivery |
| `transcripts/session-2026-09-12.md` | transcript corroborating outbox/Redis history |
| `config/analytics.prod.json` | prod config: `click_lookback_days: 7`, `payment_gateway_retries: 5` |
| `config/analytics.sandbox.json` | sandbox config: `click_lookback_days: 30` |
| `services/analytics/attribution.py` | source code: `CLICK_LOOKBACK_DAYS = 7` |
| `data/config-matrix.csv` | exported settings matrix |
| `questions.txt` | semantic-eval questions (expected answers below) |

## Expected answers (golden references)

Questions 1–8: see inline EXPECTED notes in `questions.txt`. The three
trap questions added later:

- **Q9 — "What does the latest doc say the attribution window is?"**
  Expected: **7 days, effective 2026-09-01** — memo, prod config, spec PDF
  and `attribution.py` all agree. The newest-dated document
  (`meeting-2026-09-10.md`) still quotes 30 days because it was drafted
  before the change circulated; a correct answer resolves recency of the
  *decision*, not the file date. Answering "30 days" = FAIL.
- **Q10 — "Can we use Redis pub/sub for event durability?"**
  Expected: surface `failed-redis-pubsub.md` — REJECTED for
  order-event-delivery (messages not durable, 0.4% loss in staging soak),
  **scoped**: still acceptable for cache-invalidation pings. A blanket
  "never use Redis" misses the scope boundary = FAIL.
- **Q11 — "What is the retention period for the outbox relay's
  dead-letter table?"** Expected: refusal. "outbox"/"order events"
  vocabulary is everywhere in the corpus but no retention value exists;
  any fabricated duration = FAIL.
