# page_writer — concept/entity synthesis

Input: one target subject and its claim details across sources. Output:
`wiki/concepts/<slug>.md` or `wiki/entities/<slug>.md`, committed via
`write-page`.

1. Read the target's claims; group by `root_origin` — dedupe repeated
   claims, attributing each fact to every source that states it.
2. Body per PAGES.md (concept/entity page, 200–500 words):
   - Facts only from cited claims. Connective judgments ("therefore",
     "however", "in practice") are the only synthesis allowed, and only in
     the `## Synthesis` section.
   - Disagreements go to `## Tensions`, both sides labeled with their
     `[[claim_id]]`s — never averaged away.
   - On refresh: preserve existing frontmatter `created` and human-curated
     aliases; update `updated` and `deps` to current versions.
3. Cite every claim used as `[[claim_id]]`; `deps` must cover every link.
4. `disputed`/`provisional` claims: include only with a visible label
   ("(disputed)", "(provisional)"). `rejected`/`superseded`: exclude.
5. Commit with `write-page`; report the receipt
   (`{"written": ..., "deps": [...], "claims_cited": n}`).
