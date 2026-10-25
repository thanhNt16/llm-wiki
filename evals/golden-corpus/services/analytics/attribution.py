"""Attribution settings for services/analytics.

Production defaults. The click lookback window was reduced from 30 to 7
days effective 2026-09-01 (see notes/memo-2026-09-01-window-change.md and
docs/spec-attribution-window.pdf). ADR-019's 30-day window is superseded
for production; sandbox keeps 30 days via analytics.sandbox.json.
"""

# Production click lookback / attribution window, in days.
CLICK_LOOKBACK_DAYS = 7

# Date the 7-day window took effect in production.
CLICK_LOOKBACK_EFFECTIVE = "2026-09-01"

# Sandbox intentionally retains the long window for reproducible
# long-window experiments; do not "fix" the divergence.
SANDBOX_CLICK_LOOKBACK_DAYS = 30
