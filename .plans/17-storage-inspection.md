# Storage inspection and duplicate review

Status: done. Core, MCP and the window's Find Duplicates page are on main, with the storage benchmark suite.

Depends on Get Info sizing and operations.

## Decisions

- On 2026-11-11 the window's part narrowed to Find Duplicates. Search (`size:>500mb in:~/Downloads`, sorted by Size) and the Overview's largest lists already answer "what's large", so the report's Largest files and Folders sections were dropped. The Overview doesn't link to a report: hashing a whole home folder from one bare button costs too much. `storage_scan` stays an MCP tool.
- Find Duplicates (⌥⌘U, the File menu, the palette, and the menu on a folder's empty space) opens a page in the pane's history for the lone selected folder, else the folder shown. Back leaves it and stops a scan; Esc or Cancel stops a scan and keeps what it found. The page is saved as its folder, so a launch never starts the read.
- The layout is option A of three drawn: groups as rows that open in place, each copy with Show in Folder and Keep or Trash. Every copy starts as Keep, and the last kept copy of a group can't be switched. Groups not read whole are listed under their own title with no choice. Files under 1 MB are left out until "Include Smaller Files". Moving goes through the window's card listing every path that goes and every copy that stays, then one `TrashDuplicates` operation that one Undo reverses; a copy that changed since it was read stays, and the toast says so. The maintainer asked to proceed without picking between the drawn options, so this is the recommended one.
- The window keeps the scan's verified identities and builds the cleanup from them, so it never scans twice. MCP scans stay synchronous with a timeout and partial results, rather than jobs.
- The `storage` suite in `scripts/bench` measures a 100,000-entry scan and finding duplicates among 48 files, each with how far the process's footprint rose.

## Scope

Inspect an explicitly selected folder or source for large files, directory usage and duplicate candidates. Reuse sizing services and report permission failures, incomplete totals and cancelled scans. Distinguish logical size from actual allocated/reclaimable storage; APFS clones and hard links make these different.

Find duplicate candidates with cheap metadata and, where useful, sampled hashes. Confirm equality against live files with full content verification before offering removal. Revalidate identity and changes at execution. Never remove the only retained member of a verified group.

The user chooses which copies to retain. Preview the exact paths to Trash and use ordinary operation recovery. No automatic cleanup based on age, folder name or a claimed percentage of reclaimable space. Worktree cleanup remains under the worktree lifecycle, not generic duplicate cleanup.

Core and MCP expose scan jobs, partial results, verification status and selected cleanup plans. The UI can cancel, inspect errors, open a containing folder and return to the scan. Hashing and scans have bounded concurrency and no continuous polling.

## Verification

Test false candidates, changed files, hard links, unavailable sources and retained-copy protection using small fixtures. Add allocated-space checks only where the filesystem supports a reliable fixture. Measure scan throughput and peak memory separately from browser benchmarks.
