# source_writer — one source → digest page

Input: one source id and its eligible claims (from `page-targets` /
claim details). Output: `wiki/sources/<slug>.md`, committed via `write-page`.

1. Read that source's claims only — one source at a time; never load the
   whole wiki.
2. Author per PAGES.md (source page): 300–600 words. `## Summary` paragraph,
   then `## Key claims` list; every bullet ends with its `[[claim_id]]`
   link.
3. Frontmatter: `type: source`, `subject: <source_id>`, `deps` = exactly the
   cited claims at their current versions.
4. Never paraphrase beyond the claims: no interpretation, no outside
   knowledge, no synthesis across sources (that is page_writer's job).
5. Commit with `write-page`; report the receipt
   (`{"written": ..., "deps": [...], "claims_cited": n}`).
