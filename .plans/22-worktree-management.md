# Monodrive worktree management plan

Status: proposed implementation sequence. Planning only; no feature implementation authorized by this plan itself.
Terminology: "Work3" is interpreted as "worktree".

## Goal and order

Maintainer's goal, which overrides the order below where they conflict: one place to manage all worktrees. Monodrive scans for them, lists them together, and can delete them. Deleting still needs the reviewed design this plan describes: dirty files, untracked files, branches and locks are preserved or explicitly confirmed, and removal is reversible through the Trash where possible.

Make Monodrive useful for managing existing Git worktrees first. Design and implement worktree creation through Monodrive MCP only after the management foundation is usable and its behavior has been reviewed. Monodrive doesn't launch agents; handing work to T3 Code or Monocode is in the [agent features plan](23-agent-features.md).

Git remains authoritative for repositories, branches and worktree registration. `monodrive-core` owns worktree behavior. GPUI and MCP call the same services. Keep filesystem mutations explicit, keep disk I/O off the UI thread, and preserve worktrees created by other applications.

## Roadmap dependencies

Read-only inventory can proceed independently of the file-management sequence. Mutating management, creation and setup depend on [shared operation ownership and jobs](03-file-operations.md); use that owner for cross-client coordination, receipts and recovery instead of designing another queue. The creation review still decides worktree-specific locking, reservation and setup behavior.

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
- First-release management is useful without worktree creation.

## Design review for creation through MCP

Start this milestone only after the management behavior has been evaluated. Do not add creation tools, run project setup, maintain a prepared pool or launch agents during the inventory and management milestones.

Resolve these questions before implementation:

- Which explicit inputs identify the repository, local base commit/ref, new or existing branch, and destination? What happens on branch/path collisions or a changed base ref?
- How does an MCP request authorize a filesystem mutation and any project-defined setup hooks? How are hooks trusted and their working directory/environment constrained? Creation should not perform an implicit fetch or resolve a fuzzy repository name.
- Does creation return a checkout immediately with a separate setup job, or wait for setup-ready? Define checkout-ready, preparing, ready, failed and cancelled states, with observable progress and retry behavior.
- How does the shared operation owner coordinate worktree creation, reservations and any prepared pool across UI and MCP clients? Reuse its receipts and recovery; define worktree-specific race and crash handling without creating independent queues.
- How do idempotent retries, exclusive allocation, cancellation and crash recovery avoid duplicate worktrees or deleting work another client has begun using?
- Is a small bounded pool worth its storage and replenishment costs? Define matching/invalidation inputs, pool misses, eviction and user-visible storage limits before choosing a default. No automatic reuse or reset of user-created worktrees.
- What future agent reservation records are needed? Distinguish Monodrive-owned prepared worktrees, allocated worktrees and external worktrees. Defer actual agent launch, provider adapters and session management.

Review the creation contract and lifecycle before implementing it. Creation then becomes a core service exposed through MCP and an appropriate UI entry point, not MCP-only logic. Long-running setup must not block the UI or require agents to guess readiness. Keep initial execution local to macOS. Do not add remote-machine orchestration as part of this feature.

## Creation implementation and performance validation

Implement only the design accepted in the creation review. Begin with correct ordinary Git creation, typed job state and observable setup. Keep subprocesses and any setup artifacts scoped to the requested worktree. Bound concurrency and track exactly which processes Monodrive starts. Add prepared reuse only when its ownership, validity and recovery rules are implemented; creation must remain correct on a pool miss.

Benchmark the real MCP request through setup-ready and the equivalent UI operation. Separate cold creation, cached dependency setup, prepared-pool hits, pool misses and concurrent requests. Include validation, coordination and allocation in measured handoff latency. Verify resulting `HEAD`, branch, independent writable state and setup completion. A faster incomplete workspace is not a successful result.

Use the existing benchmark only as a starting point: on this Mac and T3 Code commit `680152e27b2a961686389ca69a1557cbb2c55f08`, Git checkout took about 2 seconds, fresh checkout plus cached setup took 33-36 seconds, and a same-commit prepared-worktree preflight plus branch assignment took 0.16-0.50 seconds. Two concurrent setups finished in 56.7 seconds. APFS source/dependency copying was slower than ordinary Git/pnpm in the tested strategies. These were small samples and did not measure a Monodrive creation service or agent startup.

Target a subsecond prepared allocation as a hypothesis to validate. A 10-20x reduction in request-to-ready wait is a pool-hit target, not a promise of faster first-time setup or greater sustained throughput. Measure replenishment, retained storage and cleanup separately. Do not flush system caches or alter machine security settings to obtain results.

## Verification and delivery

Use isolated temporary Git fixtures for core behavior and MCP tests. Cover parsing and unusual paths, linked/detached/locked/stale worktrees, repository identity, failures, and consistency across entry points. Add lifecycle/concurrency tests when those behaviors are implemented. Tests must not start the whole-disk index, modify real repositories, invoke real project setup or use sleep/polling loops. Test helpers must not become production-only-for-tests exports.

Verify UI changes with before/after headless GPUI snapshots. Drive meaningful browser behavior through the existing GPUI test conventions. Show concrete design choices before UI implementation; ask before real-app screen capture. Run targeted checks first and `scripts/check` before any requested push. Review the project performance budgets when browser rendering or listing changes.

Deliver milestones separately with core behavior, UI and MCP parity, relevant verification and measured limitations. No PR, commit, cleanup of unrelated work, live index changes, dev-server startup or agent launch is authorized by writing this plan.

## References

- [Worktree benchmark report](../target/worktree-benchmark/report.txt)
- [Worktree benchmark results](../target/worktree-benchmark/results.json)

This plan is a local artifact under `.plans` and is not committed repository documentation.
