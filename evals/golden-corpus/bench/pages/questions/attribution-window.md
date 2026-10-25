---
type: question
title: What is the attribution window?
subject: attribution window
created: 2026-10-01T00:00:00Z
bench_deps:
  - attribution.window|click_lookback_days|7|production
  - attribution.window|click_lookback_days|30|sandbox
---

Q: What is the current click lookback window?

A: 7 days in production (since 2026-09-01); 30 days in sandbox. The meeting
notes of 2026-09-10 still quote 30 days — stale recap, not current policy.

## See also

- [[wiki:concepts/attribution-window]]
- [[wiki:concepts/attribution-lookback-window]]
