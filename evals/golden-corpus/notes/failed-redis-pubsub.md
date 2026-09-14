# Postmortem note: Redis pub/sub for order event delivery (rejected)

**Scope:** order-event-delivery, evaluated 2026-04.

## Approach

Deliver order events from `services/orders` to `services/analytics` via
Redis pub/sub channels to avoid broker infrastructure.

## Result: REJECTED (for this scope)

Messages are not durable: subscribers that disconnect lose messages, and
publisher-side acknowledgment does not exist. We lost roughly 0.4% of order
events in a staging soak test.

## Scope boundary

This rejection applies to **order-event-delivery** where at-least-once
delivery is required. Redis pub/sub remains acceptable for ephemeral
cache-invalidation pings, where loss is tolerable.
