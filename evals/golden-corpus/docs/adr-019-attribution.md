# ADR-019: Click attribution lookback window

**Status:** Accepted
**Decided:** 2026-05-10

## Decision

Production order attribution uses a **30-day** click lookback window,
effective 2026-01-01. A conversion is attributed to the last paid click
within 30 days before the order.

## Notes

The 30-day window was chosen following industry standards (30-day post-click
attribution was the vendor default).
