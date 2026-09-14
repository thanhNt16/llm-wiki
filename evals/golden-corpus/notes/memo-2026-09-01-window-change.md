# Memo: Attribution window change (2026-09-01)

**From:** Analytics lead
**Date:** 2026-09-01

Effective **2026-09-01**, the production click lookback window for order
attribution changed from 30 days to **7 days**, aligning paid attribution
with the new media-mix model. This supersedes ADR-019's 30-day window for
production.

The **sandbox** environment intentionally keeps the 30-day window so that
long-window experiments remain reproducible.
