# Search beyond names

Depends on places and the existing fsearch services. Name search exists, and MCP already exposes content search through `grep`; add UI parity rather than another content engine.

- Add kind, size and modified-date filters through typed tokens such as `kind:image size:>10mb`, with editable chips. UI and MCP share parsing and semantics.
- Show matching content lines through fsearch, with bounded previews and results.
- Toggle this folder versus indexed local sources, and clear the query/filters without losing the prior location.
- Save named searches in the sidebar; support edit, remove and equivalent core/MCP operations.

Keep the existing local name-index latency target and responsive typing. Measure content search and filtered first-result latency separately. Discard stale responses when the query changes. Do not promise a millisecond end-to-end response for content extraction or network queries.

Remote search is added with remote capabilities. First support explicit scoped remote listing/search where available; no automatic server-wide walk. Later offline catalogs label source availability and last successful refresh, and remote results arrive progressively without holding back local results. Saving a search does not authorize future scans or writes.

Test filter parsing, scope boundaries, saved-query round trips and stale-result handling. Use deterministic index substitutes or fixtures; tests must not start the live index.
