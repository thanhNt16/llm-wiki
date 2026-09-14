# ADR-001: Use PostgreSQL for the order service

**Status:** Accepted
**Decided:** 2026-01-15

## Context

The order service needs transactional, strongly consistent storage for order
state. DynamoDB was considered for operational simplicity.

## Decision

We use PostgreSQL (RDS, Multi-AZ) for order persistence.

## Consequences

- Strong consistency for order state transitions.
- We own migration management (sqitch).
- Connection pooling via pgbouncer at the service edge.
