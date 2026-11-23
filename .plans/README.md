# Monodrive plans

Reviewed against `main` at 23bd3fe on 2026-11-23. These are local implementation plans, not a claim that proposed features exist. Writing or updating a plan authorizes planning only.

## Tracker

Update a plan's row when its work lands on `main`. What still needs a person's hands is listed under [Needs a person](#needs-a-person), not here.

| Plan | Status | Commits |
|---|---|---|
| 01 Performance baseline | Done; measurement moved to `scripts/bench` (plan 24) | |
| 02 Selection | Done | |
| 03 File operations | Done | f932352, 3876a60, 41570a7, 2cacc36, 091c83c, 7bb3e34, 57d9968 |
| 04 Editing commands | Done | 62d9772, c8b03d7, a8293ff, 2ff9f13 |
| 05 Context menu | Done | d6f31e9, 74b996e |
| 06 Quick Look | Done; plan 28's preview replaced the panel | 4c901ec |
| 07 Places, drives and NAS | Done | ae1cead, 532d2b7, 4a396e1, f3bfa68, e47fc53, 71094b4, 5a0a9b7 |
| 08 Tabs and panes | Done | 8c16e59 and earlier |
| 09 Drag and drop | Done, with spring-loaded folders | 8173471, b9018d6 |
| 10 Open With and Get Info | Done | 5c5f7b1, 5a19615, 74b635c |
| 11 Search | Done; filter UI skipped for now | 1c1c4f5, ef44c3d, 57e7495 |
| 12 Batch rename | Done | 1574337, 13200b7 |
| 13 Archives | Done | 175a874 |
| 14 Remote locations | Done | 251768a, 9c5cfb0 |
| 17 Storage inspection | Done, narrowed to Find Duplicates in the window | 689fc55 |
| 18 Cloud storage | Done | 1dc420b, 43b1ed6, e96a205 |
| 19 Column view | Done; layout A with preview P1 | feadd11, d47c51a |
| 21 User commands | Core on main; UI picked, not built; parked | |
| 22 Worktree management | Done: inventory and removal in core, MCP and the window. Creation dropped | 13e80ce, 23e052d, 38406ca |
| 24 Benchmarks and reports | Done, except the quiet Mac | 47cf58a, 950bb2d, 3f8f37c |
| 25 Filesystem speed | Done | a65771a, 702448e, c900c12 |
| 26 MCP contract v1 | Done | 887135c |
| 27 Overview | Done, with worktree suggestions and a left column for what fills the disk | 82cb1cf, 3e20de7, 88203e5, 26f206a |
| 28 Built-in previews and editing | Done | 0440b8a, e0f7708, 48347a4, 1eb3169, 07ac872 |
| 29 Video thumbnails | Done | a91adff |
| 30 Name and organize files | Suggest Names and Organize built; parked for a refactor | 5175ada, e8cc549 |
| 31 LocalSend | Done, opt-in | 7bfa0ef, 099989e |

Plans 15 (comparison), 16 (offline catalogs), 20 (browsing workspaces) and 23 (agent handoff) were dropped; see [Dropped](#dropped).

Built without a plan of their own: onboarding, Monodrive as the default file browser and Open in Monodrive in Finder (`docs/finder.md`), Share and AirDrop (67668d2), updates and their release notes (a9d1755), the Settings pages for the app icon (a301462) and notifications (e5c4f57), toasts for quiet actions (1861251), asking before sharing anonymous usage (23bd3fe), and moving a window between displays (9c87964).

## Current state

Every core file-management milestone from 01 to 19 is on `main`, and the window and MCP reach the same services. Listing, sorting, search, copies, moves, Trash with Undo, rename, archives, SFTP servers, cloud placeholders, column view, previews with editing, Find Duplicates, the Overview, Worktrees and LocalSend all work in both, where an agent equivalent means something. MCP is frozen as contract v1 with 54 tools in two profiles.

What's left is either parked by choice (21, 30), waiting on hardware (the quiet Mac) or waiting on a person (below).

## Needs a person

These can't be checked by an agent or in CI. Tick them off here as they're done.

- 07: mounting a real SMB share.
- 09: drags to and from other apps.
- 10: launching apps, the open panel and terminals.
- 13: archives from the wild: encrypted, huge, made on Windows.
- 14: Open on a server file, and dragging to and from a server in the real app.
- 17: moving copies to the real Trash and Undo in the full app, and a folder behind a privacy prompt.
- 18: downloading and removing downloads in a real iCloud Drive or File Provider folder.
- 19: frame times on a quiet machine, and Open behind a privacy prompt.
- 22: whether a detached HEAD's commits get a ref to keep them alive while the worktree sits in the Trash. This is a decision, not a check.
- 25: `fseventsd`'s load with and without Monodrive, and launch with Terminal's Developer Tools permission on and off.
- 26: the server in Cursor.
- 27: Review… on a real Mac's worktrees, and the `overview` tool on a disk under 20% free.
- 28: parity with Apple's Quick Look panel, before "Open in Quick Look" goes.
- 31: sending to and receiving from real phones and the LocalSend app.
- Onboarding: granting Full Disk Access itself.
- The quiet Mac: buy it and register it as the `bench` runner. Cold listing (25), the instruction gate (24) and publishable numbers wait on it.

## Open work

Nothing here is ordered yet. Each needs the maintainer's pick before it starts.

- Move to Trash as a job with progress. Empty Trash is one (57d9968), but Move to Trash is still one request through `NSFileManager`, which docs/performance.md says is worth changing past a few thousand items.
- [Name and organize files](30-local-organize.md): the refactor, then the parts it never built.
- [User commands](21-extensions.md): the Commands page in Settings, once a real workflow wants it.
- The search filter UI that plan 11 skipped; core and MCP filters exist.
- Rename and move continuity for the selection (plan 02), if it still shows.
- The scrolling benchmark and launch profile writing raw samples into `scripts/bench` (plan 24).
- A Share Extension, so other apps' share menus list LocalSend (plan 31).

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
- 17: a report of largest files and verified duplicates, with a review before anything goes to the Trash. Narrowed on 2026-11-11 to Find Duplicates, page A of three, with the review in the window's card.
- 18: a cloud badge at the end of the row or on a grid icon's corner; clicking it downloads. The inspector shows where an item is as a chip by its kind, which is the button.
- 19: columns with a preview in the last column.
- 21: a Commands page in Settings.
- 22: one place to manage all worktrees. The search index finds .git entries and Git confirms each repository. Delete moves the folder to the Trash, then removes Git's registration; refused if locked or the main worktree, and dirty or unpushed work needs an extra confirmation showing what's there.
- 27: Desk. Free space and up to 3 suggestions first, then your stuff across every location, with what's happening now beside it.
- 28: the preview takes over the pane, with one bar above that also holds the tools while editing. The sidebar, tabs and a second pane stay.

- Onboarding: first launch, a dedicated window with Welcome and then one checklist of Full Disk Access and coding agents. The first index crawl waits until the access is granted or onboarding closes; the app detects the grant on return and offers to reopen.

## Dropped

Saved browsing workspaces were dropped. On 2026-10-30 these were dropped too: file and folder comparison and offline catalogs, with their core code and MCP tools (Git history has them), plus direct cloud providers, the gallery view, the extension runtime, and Monodrive running agents itself. On 2026-11-23 agent handoff (plan 23) and creating worktrees (plan 22's later milestones) were dropped: T3 Code and Monocode own that, and Monodrive only manages worktrees that exist.

## Rules shared by every milestone

Behavior lives in `monodrive-core`; the UI and MCP call the same services. Human commands use `commands.rs` for menus, keybindings and the palette. UI-only presentation, such as Quick Look, does not need a meaningless MCP equivalent. Long-running work exposes job status, cancellation and results to both clients.

Preserve disk contents, surface partial failures, and offer reversal where the source supports it. Check capabilities rather than promising local Trash or atomic moves on every backend. Provide a way back from navigation and a way to disconnect or forget a source without deleting its files.

Keep disk I/O, network I/O, hashing and extraction off the UI thread. `docs/performance.md` is the authority for measured local budgets. Local cached search stays fast; remote results arrive progressively. Connecting and synchronizing are separate choices. Never crawl a new source merely because it was opened.

For each implementation, use isolated fixtures, extend existing meaningful tests, and verify UI changes with headless snapshots. Follow AGENTS.md for regression evidence, tests, permissions and process cleanup. Compare the relevant performance benchmarks when listing, sorting, rendering or launch changes; do not rerun unrelated benchmarks for every feature. Show alternatives before substantial unsettled layout work.

## Deferred choices

- Tags remain excluded unless a concrete workflow changes that decision.
- Automatic bidirectional sync, a distributed filesystem and virtual mounts are not prerequisites for remote file management.
- Keep the macOS app scope. Browsing a Linux server through SFTP does not require a Linux app. A Linux indexing helper needs a separate scope decision.
- Do not copy Finder's per-folder view persistence or `.DS_Store` behavior.
