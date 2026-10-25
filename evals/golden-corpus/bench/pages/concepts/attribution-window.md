---
type: concept
title: Attribution window
subject: attribution window
created: 2026-10-01T00:00:00Z
bench_deps:
  - attribution.window|click_lookback_days|7|production
  - attribution.window|click_lookback_days|30|sandbox
  - attribution.window|effective_date|2026-09-01|production
---

The production click lookback window is 7 days, effective 2026-09-01 — the
memo supersedes ADR-019's 30-day window for production. Sandbox keeps 30
days so long-window experiments stay reproducible.

## See also

- [[wiki:concepts/order-event-delivery]]
- [[wiki:questions/attribution-window]]
- [[wiki:concepts/attribution-lookback-window]]
