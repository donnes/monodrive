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
| 10 Open With and Get Info | Done; launching apps, the open panel and terminals need a person to check | 5c5f7b1, 5a19615, 74b635c |
| 11 Search | Done; filter UI skipped for now | 1c1c4f5, ef44c3d |
| 12 Batch rename | Done | 1574337, 13200b7 |
| 13 Archives | Done; archives from the wild (encrypted, huge, made on Windows) need a person to check | |
| 14 Remote locations | Done; Apple's Quick Look panel and Open on a server file need a person to check | |
| 17 Storage inspection | Core on main; UI picked, not built | |
| 18 Cloud storage | Done; downloading and removing downloads from a real iCloud Drive or File Provider folder needs a person to check | |
| 19 Column view | Done; layout A with preview P1. Frame times need a quiet machine to record against the budget, and the privacy-prompt Open needs a person to check | |
| 21 User commands | Core on main; UI picked, not built; parked | |
| 22 Worktree management | Done: the disk-wide scan, the Worktrees page (option A) and removal through the Trash with undo, in core, MCP and the window. Creation through MCP is its own later milestone. Keeping a detached HEAD's commits alive with a ref while they're in the Trash needs a decision | |
| 23 Agent features | Later | |
| 24 Benchmarks and reports | Planned; do first | |
| 25 Filesystem speed | Planned; needs plan 24's runner | |
| 26 MCP contract v1 | Planned; tool map and profiles need review | |
| 27 Overview | Done except step 5: worktree suggestions (one row for all clean worktrees, picked) wait for plan 22's view, and Free up's link waits for plan 17's report | 82cb1cf |
| 28 Built-in previews and editing | Done; the full app, with a real shared owner and session, needs a person to check | |
| 29 Video thumbnails | Done; a real share and a folder behind a privacy prompt need a person to check | |
| 30 Name and organize files | Suggest Names and rules-only Organize done, for text-based files with no model; a model is held behind a speed gate; the review page is option A, picked without drawn options | |

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
- 17: a report of largest files and verified duplicates, with a review before anything goes to the Trash.
- 18: a cloud badge at the end of the row or on a grid icon's corner; clicking it downloads. The inspector shows where an item is as a chip by its kind, which is the button.
- 19: columns with a preview in the last column.
- 21: a Commands page in Settings.
- 22: one place to manage all worktrees. The search index finds .git entries and Git confirms each repository. Delete moves the folder to the Trash, then removes Git's registration; refused if locked or the main worktree, and dirty or unpushed work needs an extra confirmation showing what's there.
- 27: Desk. Free space and up to 3 suggestions first, then your stuff across every location, with what's happening now beside it.
- 28: the preview takes over the pane, with one bar above that also holds the tools while editing. The sidebar, tabs and a second pane stay.

- Onboarding (done; granting Full Disk Access itself needs a person to check): first launch, a dedicated window with Welcome and then one checklist of Full Disk Access and coding agents. The first index crawl waits until the access is granted or onboarding closes; the app detects the grant on return and offers to reopen.

Saved browsing workspaces were dropped. On 2026-10-30 these were dropped too: file and folder comparison and offline catalogs, with their core code and MCP tools (Git history has them), plus direct cloud providers, the gallery view, the extension runtime, and Monodrive running agents itself.

## Next up, in order

1. [Benchmarks and reports](24-benchmarks-and-reports.md), which plans 25 and 26 use to prove their claims.
2. [Filesystem speed](25-filesystem-speed.md).
3. [MCP contract v1](26-mcp-contract.md), before the MCP server is promoted in a stable release.
4. The UI for plans 13, 14, 17, 18, 19, 22 and [27](27-overview.md) with the picks above, following DESIGN.md. The palette (⌘K) is on main; its Remote locations and Cloud storage pages grow as plans 14 and 18 land. Choosing an SFTP server only says browsing isn't available yet, and every server reads "Not connected", until plan 14's UI connects from the window.

## Current state

- List and grid views, sorting, hidden files, thumbnails, folder tree, navigation history, Go to Folder, command palette, settings, and FSEvents updates exist.
- Path-based multi-selection, ranges, marquee selection, selected counts/sizes, and selection tests exist. Rename/move continuity still depends on the operations service.
- Name search and MCP `find_files`, `grep`, `list_dir`, and `index_status` exist. Content search in the window and richer filters remain planned.
- Listing/scrolling benchmarks and launch profiling exist. `docs/performance.md` records outstanding sorting and launch budget misses. Do not recreate this instrumentation or mark the performance work complete.
- Core file mutations, transfer recovery, connected-source management, and worktree management remain planned.

## Rules shared by every milestone

Behavior lives in `monodrive-core`; the UI and MCP call the same services. Human commands use `commands.rs` for menus, keybindings and the palette. UI-only presentation, such as Quick Look, does not need a meaningless MCP equivalent. Long-running work exposes job status, cancellation and results to both clients.

Preserve disk contents, surface partial failures, and offer reversal where the source supports it. Check capabilities rather than promising local Trash or atomic moves on every backend. Provide a way back from navigation and a way to disconnect or forget a source without deleting its files.

Keep disk I/O, network I/O, hashing and extraction off the UI thread. `docs/performance.md` is the authority for measured local budgets. Local cached search stays fast; remote results arrive progressively. Connecting and synchronizing are separate choices. Never crawl a new source merely because it was opened.

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
17. [Storage inspection and duplicate review](17-storage-inspection.md).
18. [Cloud storage](18-cloud-storage.md). Local placeholders only.

## Other features and later agent work

These are separate follow-on tracks. They do not require every storage backend to ship first.

- [Column view](19-more-views.md), after selection and previews.
- [User commands](21-extensions.md), parked until a real workflow needs them.
- [Worktree management](22-worktree-management.md) stays separate: inspect existing worktrees first, then reviewed management and creation. Read-only inventory can proceed independently; mutations and setup reuse the shared jobs foundation.
- [Filesystem speed](25-filesystem-speed.md) absorbs the sorting miss in plan 01.
- [Agent handoff and status](23-agent-features.md), after reliable shared operations and the relevant worktree lifecycle.
- [Video thumbnails](29-video-thumbnails.md), then [built-in previews](28-previews.md), which takes its poster from the thumbnails.
- [Name and organize files](30-local-organize.md), independent of the other two. It uses no model unless one clears its speed gate.

## Deferred choices

- Tags remain excluded unless a concrete workflow changes that decision.
- Automatic bidirectional sync, a distributed filesystem and virtual mounts are not prerequisites for remote file management.
- Keep the macOS app scope. Browsing a Linux server through SFTP does not require a Linux app. A Linux indexing helper needs a separate scope decision.
- Do not copy Finder's per-folder view persistence or `.DS_Store` behavior.
