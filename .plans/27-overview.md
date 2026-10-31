# Overview

Status: direction picked (Desk, D1), not built. This plan authorizes planning only.

## Goal

A page that answers three things on opening: what should I do, where is my stuff, and what is happening now. It is about the user's things across every location, not a list of drives. The sidebar already lists drives.

Today's measurements on the maintainer's Mac are why it leads with actions: 44 GB free of 460 GB, 92.4 GB in build outputs and package stores, 12.4 GB in worktrees. Worktrees alone would miss most of what can be recovered.

## The page

From the top, in one pane, as a place in Back history:

1. **Headline and suggestions.** Free space on the startup disk, then up to 3 suggestions, each with what it frees, why it's safe, and one button.
2. **Your stuff, everywhere.** Projects and kinds of things, each row saying where it lives. A row opens its folder, or a page when it spans locations.
3. **Right now.** Jobs, what agents just changed, and which locations are connected.

Rules that apply everywhere:

- Never add free space across locations. Free space on Backup doesn't help a full startup disk.
- Opening the page reads capacity and saved results, and connects to nothing. Scanning a drive or share and connecting a server are separate, explicit choices.
- Every size shows its age when it isn't current.
- Suggestions appear only for a location under 20% free. Above that the page leads with your stuff.
- With one drive and no servers, nothing on the page looks broken or empty.

## Suggestions

A suggestion is a typed value with evidence, never a name match or a percentage. Plan 17's rule stands: no automatic cleanup, and the user presses each one.

| Kind | Proof | Action |
|---|---|---|
| Build output | `CACHEDIR.TAG` inside and a build manifest beside it (`target` on this Mac has both) | Move to Trash |
| Tool cache | A known path for that tool | Move to Trash, or the tool's own command where it owns the cleanup |
| Worktree | Git says clean, nothing unpushed, not locked, not the main worktree | Opens plan 22's review, never one press |

Rank by bytes freed, using `Usage::unshared` so clones and hard links don't inflate the number. The headline counts what you will have after Empty Trash, because moving to the Trash frees nothing yet. Each result gets the usual toast with Undo, and Empty Trash stays its own step.

Recheck the proof when the action runs. Clean refuses while a build is running, because moving `target` mid-build breaks that build and a warning is easy to click past. For Cargo, the check is a non-blocking try for a shared lock on `.cargo-lock` in each profile folder, released at once. The refusal is one line: `Cargo is building in monodrive. Try again when it finishes.` Other tools get this check only where they have a lock to test. The pnpm store can't be a Trash suggestion: `node_modules` folders hard-link into it, and only pnpm knows what's unused.

## What each location can show

| Location | Capacity | Stuff | Cleanup |
|---|---|---|---|
| Startup disk | `volumes`, off the UI thread | Known folders and repositories | Suggestions |
| External drive | `volumes` | Only after a scan the user asks for; the index never covers it | Trash, per item, from the scan's rows |
| SMB share | `volumes`, which can wait on the network | Only after a scan the user asks for | Only where the share has a Trash |
| SFTP | No free-space read exists | Connection state only in this plan | None; SFTP has no Trash |
| Disconnected drive | None | Totals from its last scan, dated, read-only | None |
| Cloud folder | Part of the disk holding it | Placeholders take no blocks, so downloads only | Remove Download, which core has |

Offline catalogs were dropped on 2026-10-30. A disconnected drive therefore keeps only group totals from its last scan, with no file list and nothing to browse. Those totals never count as proof that a copy exists.

## Core

Add `crates/monodrive-core/src/overview.rs`. The view and the MCP tool call it, and neither carries logic. It reuses `volumes`, `sizes`, `storage::scan`, `worktrees`, `cloud` and the ops owner's `jobs()`, and owns only classification, ranking and the saved summary.

- Repositories come from those registered through `worktrees::register`, plus `.git` entries the index finds, validated by Git as plan 22 says.
- Measuring walks real folders, which is slow: a `du` of the home folder hadn't finished after 8 minutes. So the page draws the saved summary at once, measures known folders in the background with bounded concurrency, and updates rows as they finish. It never walks the whole disk on open.
- Save group totals, each with its date, to one file in the support folder. It holds no paths beyond the groups' roots and is not a catalog.
- A project on two locations is one row when both have the same normalized Git remote URL. Comparing them reads local Git config only and needs no network.

## View

- Add `Stop`'s third kind beside `recents`, so Back, tabs and sessions keep working. Make it an enum rather than a second bool.
- `GoOverview` in `commands.rs` with ⌥⌘O, which is free. That gives it the menu, palette and Keyboard settings. Add a sidebar row at the top.
- Put the new code in `browser/overview.rs`, on `theme.rs`'s controls. It has no list columns, selection or marquee, and commands that act on files don't apply there.
- Stay on `DESIGN.md`'s type scale. The headline is `TEXT_TITLE` semibold, as a current folder's name is, with no display size.
- Add the page and its states to the snapshot example: under and over 20% free, one drive, unscanned share, disconnected drive.

## MCP

One read-only `overview` tool in plan 26's core profile, returning the same locations, groups, ages and suggestions as the window, including "not scanned", so an agent doesn't take old numbers as current. Scanning uses `storage_scan`. Cleanup uses the existing operation tools, under "Let agents change files". Check that this one addition stays within plan 26's 16 KB budget.

## Order

Each step ships alone and is useful alone.

1. Core and MCP for the startup disk: locations, groups and suggestions, read-only.
2. The page: headline, suggestions as links that open the folder, your stuff, jobs, Back and the command.
3. Suggestions that act, through the ops owner, with Undo.
4. Scan on request for drives and shares, the saved summary, and rows that span locations.
5. Worktree suggestions, once plan 22's delete review exists.

## Left out

- **"Claude is working here."** The ops owner knows `Origin::Agent` on jobs and operations, so "Claude moved 12 files" works. Nothing tracks which agents are connected or busy, so presence waits for plan 23.
- **Time Machine status.** Nothing in core reads it, and it isn't needed for the first release.
- **SFTP sizes and free space.** They need a remote walk and a free-space read that don't exist.
- **Free-space history and the timeline (option E).** It needs background sampling.

## Open questions

- Should the 20% threshold be a constant, or also absolute (under 50 GB free) for large drives?
- Which group holds the unexplained 284 GB on this Mac? It stays "everything else" until a scan says otherwise, and step 1 should measure how much of it the index's sizes can explain without walking.

## Verification

Use temp folders and Git fixtures for classification, proof, ranking and rechecks at execution, including a dirty worktree, a `target` without its tag, and a project on two locations. Tests never call `Index::start` and never read real caches. Drive the page with `#[gpui::test]` for Back and for suggestions that act. Compare snapshots before and after, dark and light. Run the launch and browser benchmarks in `docs/performance.md` once the sidebar row and `Stop` change land, since they touch rendering.
