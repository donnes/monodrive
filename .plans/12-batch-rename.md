# Batch rename

Depends on editing commands and the shared operation preview/recovery contract.

## Scope

Add find/replace and numbered naming for an explicit selection. Preview old and new names in visible selection order, preserve extensions by default, and show invalid names and collisions before applying. Sorting or changing the selection invalidates the preview.

Core produces a typed rename plan that UI and MCP can inspect and execute with the same policies. Revalidate at execution, including case-insensitive collisions and case-only renames on the actual filesystem. Temporary names needed to resolve cycles belong to the job and its recovery record. A batch is not assumed atomic: report each outcome and support validated undo of completed items.

Expose the sheet through the command table and provide cancel without mutations. No regex engine, metadata naming language or automated rules in the first version.

## Verification

Test stable numbering order, collisions, case changes, rename cycles, preview invalidation and recovery after a partial batch in isolated folders. Extend operation tests rather than duplicating their entire coverage. Verify the preview sheet through headless snapshots.
