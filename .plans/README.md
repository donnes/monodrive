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
| 07 Places, drives and NAS | Done; a real SMB share still needs a check | ae1cead, 532d2b7, 4a396e1, f3bfa68 |
| 08 Tabs and panes | Done; drag between panes is in plan 09 | 8c16e59 and earlier |
| 09 Drag and drop | Done, with spring-loaded folders; drags to and from other apps need a person to check | 8173471 and earlier |
| 10 Open With and Get Info | Core on main; UI waiting for picks | |
| 11 Search | Core and MCP filters on main; UI picked, filter UI skipped for now | |
| 12 Batch rename | Core on main; UI waiting for picks | |
| 13 Archives | Core on main; UI waiting for picks | |
| 14 Remote locations | Core on main; UI waiting for picks | |
| 15 File and folder comparison | Core on main; UI waiting for picks | |
| 16 Offline catalogs | Core on main; UI waiting for picks | |
| 17 Storage inspection | Core on main; UI waiting for picks | |
| 18 Cloud storage | Local slice core on main; UI waiting for picks; direct providers deferred | |
| 19 More views | Waiting for UI picks | |
| 21 Extensions | User commands core on main; UI waiting for picks; runtime deferred | |
| 22 Worktree management | Inventory on main; goal changed to one place to scan and manage all worktrees; deletion needs a design review | |
| 23 Agent features | Later | |

## UI picks

The maintainer chose these from drawn options. Mockups are not in the repo. Show 3 to 4 options before any other UI work.

- 03 to 05: Monocode popover for the context menu, a title bar ring and popover for jobs, toasts with Undo, an in-app Empty Trash card.
- Top bar: one job, where am I among which tabs. Back and forward, the folder name (which becomes the first tab), and the ring. Path, selection and free space go in a thin hideable bar under the files, one per pane. Rename errors sit under the field.
- 07: a capacity bar under each drive, an Edit mode for Favorites, a Finder-style Connect to Server sheet, the Settings icon at the bottom right of the sidebar with a Trash icon button at the bottom left.
- 08: compact tabs in the title bar; two equal panes with their own path lines, plus a way to collapse the sidebar.
- 09: an outline on the target and a pill by the pointer that says what will happen. Spring-loaded folders: resting about 1 second opens the folder, Space opens at once, Esc returns, with a setting.
- 10: a docked inspector that follows the selection, like Spacedrive's (large icon, name and kind, then Details, Dates and Storage sections), and Open With as a context menu submenu.
- 11: no filter UI for now; the maintainer will rethink it. Core and MCP filters stay. Matching lines under each file, and saved and recent searches in the search field's menu.
- 12: rename in the list, with a bar across it.
- 13: browse a ZIP like a folder, with a read-only header and Extract buttons.
- 14: servers under Locations.
- 15: an aligned two-pane list with a status column.
- 16: a cataloged drive keeps its sidebar row when unplugged, with its last known contents browsable read-only under a banner.
- 17: a report of largest files and verified duplicates, with a review before anything goes to the Trash.
- 18: a cloud badge at the end of the row or on a grid icon's corner; clicking it downloads.
- 19: columns with a preview in the last column; gallery as a large preview above a thumbnail strip.
- 21: a Commands page in Settings.
- 22: one place to manage all worktrees. The search index finds .git entries and Git confirms each repository. Delete moves the folder to the Trash, then removes Git's registration; refused if locked or the main worktree, and dirty or unpushed work needs an extra confirmation showing what's there.

- Onboarding (done; granting Full Disk Access itself needs a person to check): first launch, a dedicated window with a step list beside a how-to picture: Welcome, Full Disk Access, Connect agents, Ready. The first index crawl waits for the Full Disk Access step; the app detects the grant on return and offers to reopen.

Saved browsing workspaces were dropped.

## Next up, in order

1. Plans 09 to 19, 21 and 22 with the picks above, following DESIGN.md. The palette (⌘K) is on main; its Remote locations and Cloud storage pages grow as plans 14 and 18 land. Choosing an SFTP server only says browsing isn't available yet, and every server reads "Not connected", until plan 14's UI connects from the window.

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
- [User commands and extensions](21-extensions.md), after stable commands and operation services.
- [Worktree management](22-worktree-management.md) stays separate: inspect existing worktrees first, then reviewed management and creation. Read-only inventory can proceed independently; mutations and setup reuse the shared jobs foundation.
- [Agent handoff and agent management](23-agent-features.md), after reliable shared operations and the relevant worktree lifecycle. Agent features are planned for later, not excluded from Monodrive.

## Deferred choices

- Tags remain excluded unless a concrete workflow changes that decision.
- Automatic bidirectional sync, a distributed filesystem, virtual mounts and a broad plugin runtime are not prerequisites for remote file management.
- Keep the macOS app scope. Browsing a Linux server through SFTP does not require a Linux app. A Linux indexing helper needs a separate scope decision.
- Do not copy Finder's per-folder view persistence or `.DS_Store` behavior.
