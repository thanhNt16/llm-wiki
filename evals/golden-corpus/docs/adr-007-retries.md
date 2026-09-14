# ADR-007: Retry policy for payment gateway calls

**Status:** Accepted
**Decided:** 2026-02-20

## Decision

Payment gateway calls retry up to **3 times** with exponential backoff
(base 200ms) on 5xx and timeout errors. No retries on 4xx.

## Rationale

3 retries bounds worst-case latency while covering transient gateway blips.
