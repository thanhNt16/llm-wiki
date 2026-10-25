---
type: change
title: Retired rollout plan (planted stale page)
subject: retired rollout plan
created: 2026-06-01T00:00:00Z
stale_after: 2026-08-01
bench_deps:
  - order.event.delivery|mechanism|transactional outbox|
---

Planted fixture: an old rollout plan whose `stale_after` (2026-08-01) is in
the past, so doctor flags `stale_page_ttl`. The outbox relay replaced this
plan.

## See also

- [[wiki:concepts/order-event-delivery]]
