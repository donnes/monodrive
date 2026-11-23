# Monodrive worktree management plan

Status: done (scan, Worktrees page, removal through the Trash; see docs/worktrees.md). Creating worktrees, through MCP or the window, was dropped on 2026-11-23 along with agent handoff.
Terminology: "Work3" is interpreted as "worktree".

## Goal and order

Maintainer's goal, which overrides the order below where they conflict: one place to manage all worktrees. Monodrive scans for them, lists them together, and can delete them. Deleting still needs the reviewed design this plan describes: dirty files, untracked files, branches and locks are preserved or explicitly confirmed, and removal is reversible through the Trash where possible.

Monodrive manages existing Git worktrees. It doesn't create them, launch agents or hand work to agent apps; T3 Code and Monocode do that.

Git remains authoritative for repositories, branches and worktree registration. `monodrive-core` owns worktree behavior. GPUI and MCP call the same services. Keep filesystem mutations explicit, keep disk I/O off the UI thread, and preserve worktrees created by other applications.

## Roadmap dependencies

Read-only inventory can proceed independently of the file-management sequence. Removal depends on [shared operation ownership and jobs](03-file-operations.md); use that owner for cross-client coordination, receipts and recovery instead of designing another queue.

Worktree navigation reuses existing browsing. Remote file access alone does not authorize remote Git operations, setup or agent launch. Local inventory and management do not wait for cloud backends or agent features.

## Inventory existing worktrees

Add a small worktrees service in `crates/monodrive-core/src/worktrees.rs`, exported from `lib.rs`. Resolve a selected folder to its repository and common Git directory. Use Git's machine-readable, `NUL`-delimited worktree listing and typed results for paths, `HEAD`, branch or detached state, bare state, and Git's locked/prunable annotations. Resolve repositories by Git metadata rather than folder names. Handle linked worktrees, nested input folders and symlink aliases without listing the same repository twice.

Treat a missing folder, stale registration, permission failure and a clean checkout as distinct states. Do not equate `prunable` with safe to delete. Avoid fetching remotes, starting fsearch, scanning the disk, or running setup hooks to list worktrees. Discover from an explicitly opened or registered repository first; the file index may later suggest candidates, which Git must validate.

Add read-only MCP operations to list a repository's worktrees and inspect one worktree. Final tool names and schemas should be agreed alongside implementation; use explicit repository/worktree paths and terse responses. Core services do not depend on GPUI or MCP protocol types.

### Acceptance criteria

- Given a repository folder or one of its linked worktrees, UI and MCP receive the same inventory.
- Non-repositories and inaccessible paths produce useful errors.
- Listing never changes the repository or launches setup.

## Management UI and lifecycle controls

Before implementing substantial layout, present two concrete alternatives using Monodrive's design language: a worktrees section associated with the current repository, or a dedicated worktree overview opened from a command. Choose one for the first release; do not build both speculatively.

Show path, branch or detached `HEAD`, primary/linked status and Git lock state. Provide open in Monodrive, copy path, refresh, and return to the previous location. Add actions through `commands.rs` so keyboard shortcuts, menu, palette and Keyboard settings remain consistent. Reuse normal browser history and folder navigation. Keep new view code outside `browser.rs` when it removes complexity.

Let the user register and forget repository locations. Forgetting a repository removes Monodrive's bookmark only; it must not alter Git or disk contents. Persist only Monodrive's own registrations/preferences, never a second authoritative copy of Git worktree metadata. UI placement and MCP equivalents use the same core behavior.

Load dirty status for the selected worktree on demand. Do not run `git status` for every row on each render. Coalesce refreshes, discard stale background results, and use scoped changes or explicit refresh instead of continuous whole-repository polling. Keep large inventories virtualized.

Add explicit lock/unlock management only with clear distinctions between a Git worktree lock and a future Monodrive agent reservation. A Git lock prevents pruning; it is not proof that an agent is running. An unknown owner is not an idle worktree.

Cleanup requires a separate reviewed design within this milestone. Define how Trash, Git registration and restoration work together before shipping removal. The ordinary `git worktree remove` command deletes files, so it cannot be presented as a Trash operation. If safe, reviewable restoration cannot be provided, leave folder removal out of this release. Stale Git-registration pruning is a separate explicit action and must not remove folders or branches implicitly. Recheck conditions when a mutation executes, not just when the dialog opens. Preserve dirty files, untracked files, branches and active/unknown agent state.

### Acceptance criteria

- Users and MCP clients can inspect and manage existing worktrees with consistent results.
- Opening has Back.
- Mutations run in the background, report errors and have an explicit recovery path.
- Cleanup cannot silently discard user data.

## Verification and delivery

Use isolated temporary Git fixtures for core behavior and MCP tests. Cover parsing and unusual paths, linked/detached/locked/stale worktrees, repository identity, failures, and consistency across entry points. Add lifecycle/concurrency tests when those behaviors are implemented. Tests must not start the whole-disk index, modify real repositories, invoke real project setup or use sleep/polling loops. Test helpers must not become production-only-for-tests exports.

Verify UI changes with before/after headless GPUI snapshots. Drive meaningful browser behavior through the existing GPUI test conventions. Show concrete design choices before UI implementation; ask before real-app screen capture. Run targeted checks first and `scripts/check` before any requested push. Review the project performance budgets when browser rendering or listing changes.

Deliver milestones separately with core behavior, UI and MCP parity, relevant verification and measured limitations. No PR, commit, cleanup of unrelated work, live index changes, dev-server startup or agent launch is authorized by writing this plan.

This plan is a local artifact under `.plans` and is not committed repository documentation.
