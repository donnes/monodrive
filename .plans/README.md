# Monodrive plans

Reviewed against the code on 2026-10-29. These are local implementation plans, not a claim that proposed features exist. This revision authorizes planning only.

## Tracker

Update a plan's row when its work lands on `main`.

| Plan | Status | Commits |
|---|---|---|
| 01 Performance baseline | Done | |
| 02 Selection | Done | |
| 03 File operations | Done | f932352, 3876a60, 41570a7, 2cacc36, 091c83c, 7bb3e34 |
| 04 Editing commands | Done | 62d9772, c8b03d7, a8293ff, 2ff9f13 |
| 05 Context menu | Done | d6f31e9, 74b996e |
| 06 Quick Look | Done | 4c901ec |
| 07 Places, drives and NAS | Core on main; SMB and UI waiting for picks | |
| 08 Tabs and panes | Waiting for UI picks | |
| 09 Drag and drop | Waiting for UI picks | |
| 10 Open With and Get Info | Core on main; UI waiting for picks | |
| 11 Search | Core on main; UI waiting for picks | |
| 12 Batch rename | Core on main; UI waiting for picks | |
| 13 Archives | Core on main; UI waiting for picks | |
| 14 Remote locations | Core on main; UI waiting for picks | |
| 15 File and folder comparison | Core on main; UI waiting for picks | |
| 16 Offline catalogs | Core on main; UI waiting for picks | |
| 17 Storage inspection | Core on main; UI waiting for picks | |
| 18 Cloud storage | Local slice core on main; UI waiting for picks; direct providers deferred | |
| 19 More views | Waiting for UI picks | |
| 20 Browsing workspaces | Waiting for tabs, panes and UI picks | |
| 21 Extensions | User commands core on main; UI waiting for picks; runtime deferred | |
| 22 Worktree management | Inventory on main; UI waiting for picks; creation needs a design review | |
| 23 Agent features | Later | |

## Current state

- List and grid views, sorting, hidden files, thumbnails, folder tree, navigation history, Go to Folder, command palette, settings, and FSEvents updates exist.
- Path-based multi-selection, ranges, marquee selection, selected counts/sizes, and selection tests exist. Rename/move continuity still depends on the operations service.
- Name search and MCP `find_files`, `grep`, `list_dir`, and `index_status` exist. Content search in the window and richer filters remain planned.
- Listing/scrolling benchmarks and launch profiling exist. `docs/performance.md` records outstanding sorting and launch budget misses. Do not recreate this instrumentation or mark the performance work complete.
- Core file mutations, transfer recovery, connected-source management, and worktree management remain planned.

## Rules shared by every milestone

Behavior lives in `monodrive-core`; the UI and MCP call the same services. Human commands use `commands.rs` for menus, keybindings and the palette. UI-only presentation, such as Quick Look, does not need a meaningless MCP equivalent. Long-running work exposes job status, cancellation and results to both clients.

Preserve disk contents, surface partial failures, and offer reversal where the source supports it. Check capabilities rather than promising local Trash or atomic moves on every backend. Provide a way back from navigation and a way to disconnect or forget a source without deleting its files.

Keep disk I/O, network I/O, hashing and extraction off the UI thread. `docs/performance.md` is the authority for measured local budgets. Local cached search stays fast; remote results arrive progressively and offline results show their age. Connecting, cataloging and synchronizing are separate choices. Never crawl a new source merely because it was opened.

For each implementation, use isolated fixtures, extend existing meaningful tests, and verify UI changes with headless snapshots. Follow AGENTS.md for regression evidence, tests, permissions and process cleanup. Compare the relevant performance benchmarks when listing, sorting, rendering or launch changes; do not rerun unrelated benchmarks for every feature. Show alternatives before substantial unsettled layout work.

## Core file management order

Numbers give the recommended sequence, not a requirement to finish unrelated optional features. Each plan names its actual dependencies. Performance remains an acceptance gate throughout.

1. [Performance baseline and remaining budget work](01-performance-baseline.md). Instrumentation exists; resolve or explicitly review documented misses.
2. [Selection](02-selection.md). Foundation exists; retain its behavior and finish mutation continuity with operations.
3. [File operations, shared jobs and recovery](03-file-operations.md). Establish one operation owner before adding more write clients.
4. [Editing commands](04-editing-commands.md).
5. [Context menu](05-context-menu.md). Add later commands only when their services exist.
6. [Quick Look](06-quick-look.md).
7. [Places, external drives and NAS](07-places-drives-and-nas.md).
8. [Tabs and split panes](08-tabs-and-panes.md). Move earlier so transfers have useful source/destination views.
9. [Drag and drop](09-drag-and-drop.md). Use the existing operation service, places and panes.
10. [Open With, packages and Get Info](10-open-with-and-info.md).
11. [Search beyond names](11-search.md).
12. [Batch rename](12-batch-rename.md).
13. [Archives](13-archives.md).
14. [Remote locations over SSH/SFTP](14-remote-locations.md).
15. [File and folder comparison](15-file-and-folder-comparison.md). Local comparison can ship before remote locations.
16. [Offline catalogs](16-offline-catalogs.md).
17. [Storage inspection and duplicate review](17-storage-inspection.md).
18. [Cloud storage](18-cloud-storage.md). Add direct providers only for a concrete need; handle local placeholders first.

## Other features and later agent work

These are separate follow-on tracks. They do not require every storage backend to ship first.

- [Column and gallery views](19-more-views.md), after selection and previews.
- [Saved browsing workspaces](20-browsing-workspaces.md), after tabs, panes and source identity.
- [User commands and extensions](21-extensions.md), after stable commands and operation services.
- [Worktree management](22-worktree-management.md) stays separate: inspect existing worktrees first, then reviewed management and creation. Read-only inventory can proceed independently; mutations and setup reuse the shared jobs foundation.
- [Agent handoff and agent management](23-agent-features.md), after reliable shared operations and the relevant worktree lifecycle. Agent features are planned for later, not excluded from Monodrive.

## Deferred choices

- Tags remain excluded unless a concrete workflow changes that decision.
- Automatic bidirectional sync, a distributed filesystem, virtual mounts and a broad plugin runtime are not prerequisites for remote file management.
- Keep the macOS app scope. Browsing a Linux server through SFTP does not require a Linux app. A Linux indexing helper needs a separate scope decision.
- Do not copy Finder's spring-loaded folders, per-folder view persistence or `.DS_Store` behavior.
