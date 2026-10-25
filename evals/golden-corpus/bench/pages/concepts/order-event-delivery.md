---
type: concept
title: Order event delivery
subject: order event delivery
created: 2026-10-01T00:00:00Z
bench_deps:
  - order.event.delivery|mechanism|transactional outbox|
  - order.event.delivery|rationale|durability: Redis pub/sub loses messages on subscriber disconnect|
  - event.delivery.redis-pubsub|status|rejected|order-event-delivery
---

Order events are delivered via a transactional outbox: events are written in
the same Postgres transaction as the order state change, then relayed. Redis
pub/sub was rejected for this scope — messages are not durable on subscriber
disconnect (0.4% loss in a staging soak) — though it remains acceptable for
cache-invalidation pings.

## See also

- [[wiki:concepts/order-database]]
- [[wiki:entities/payment-gateway]]
- [[wiki:concepts/attribution-window]]
- [[wiki:changes/retired-rollout-plan]]
