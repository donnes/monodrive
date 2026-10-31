# Storage inspection and duplicate review

Depends on Get Info sizing and operations.

## Scope

Inspect an explicitly selected folder or source for large files, directory usage and duplicate candidates. Reuse sizing services and report permission failures, incomplete totals and cancelled scans. Distinguish logical size from actual allocated/reclaimable storage; APFS clones and hard links make these different.

Find duplicate candidates with cheap metadata and, where useful, sampled hashes. Confirm equality against live files with full content verification before offering removal. Revalidate identity and changes at execution. Never remove the only retained member of a verified group.

The user chooses which copies to retain. Preview the exact paths to Trash and use ordinary operation recovery. No automatic cleanup based on age, folder name or a claimed percentage of reclaimable space. Worktree cleanup remains under the worktree lifecycle, not generic duplicate cleanup.

Core and MCP expose scan jobs, partial results, verification status and selected cleanup plans. The UI can cancel, inspect errors, open a containing folder and return to the scan. Hashing and scans have bounded concurrency and no continuous polling.

## Verification

Test false candidates, changed files, hard links, unavailable sources and retained-copy protection using small fixtures. Add allocated-space checks only where the filesystem supports a reliable fixture. Measure scan throughput and peak memory separately from browser benchmarks.
