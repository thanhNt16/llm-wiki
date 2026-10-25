---
type: concept
title: Order database
subject: order database
created: 2026-10-01T00:00:00Z
bench_deps:
  - svc.order.db|engine|PostgreSQL|
---

The order service persists order state in PostgreSQL (RDS, Multi-AZ), per
ADR-001. Strong consistency is required for order state transitions;
migrations are managed with sqitch.

## See also

- [[wiki:concepts/order-event-delivery]]
- [[wiki:questions/attribution-window]]
- [[wiki:concepts/lookback-window-attribution]]
