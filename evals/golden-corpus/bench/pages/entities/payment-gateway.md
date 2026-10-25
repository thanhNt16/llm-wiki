---
type: entity
title: Payment gateway
subject: payment gateway
created: 2026-10-01T00:00:00Z
disputed: true
bench_deps:
  - payment.gateway|retries|3|production
  - payment.gateway|retries|5|production
---

Payment gateway calls retry on 5xx and timeouts with exponential backoff.
The configured production retry count is **disputed**: ADR-007 records 3,
while the production config and ops report say 5. Resolve before changing.

## See also

- [[wiki:concepts/order-event-delivery]]
- [[wiki:concepts/order-database]]
