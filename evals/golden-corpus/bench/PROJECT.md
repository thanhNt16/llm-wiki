# PROJECT.md — manual project summary (baseline lane)

Maintained by hand; last touched 2026-06-30.

- The order service stores its data in PostgreSQL (RDS, Multi-AZ).
- Order events go out through a transactional outbox for durability —
  Redis pub/sub lost messages in a staging soak.
- Production attribution uses a 7-day click lookback window (changed
  2026-09-01 per the analytics memo; this file was updated late).
- Payment gateway calls retry 3 times with backoff per ADR-007.
