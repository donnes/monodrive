# File and folder comparison

Depends on shared operation previews and split panes. Local comparison can ship before SSH/SFTP; remote comparison requires that backend's read capability.

## Scope

Compare two explicitly chosen files or folders. Show entries present on one side, type conflicts, metadata differences, verified content differences and unreadable/unknown results. Size and modification time may identify candidates but cannot prove equal contents. Hash or compare bytes only on demand with bounded background work; do not follow directory links into an unbounded walk.

Reuse the relevant Monocode native diff rendering for two text files, stripped of Git staging and conversation dependencies. Report binary or oversized files without loading them fully into the renderer. A repository is not required.

Offer a reviewable one-way copy/merge for selected differences. The preview names source, destination, replacements and conflicts. Execution uses the operation service and revalidates changes since comparison. Do not delete destination-only files by default. Automatic bidirectional sync and scheduled mirroring are deferred.

Core and MCP expose comparison results and planned operations; the UI provides keyboard navigation, cancel and return to normal browsing. A comparison result is not a backup guarantee.

## Verification

Test same-size/different-content files, type conflicts, unreadable entries, changed inputs and selected-only execution with fixtures. Bound memory and concurrent reads, and verify text-diff and conflict views with headless snapshots.
