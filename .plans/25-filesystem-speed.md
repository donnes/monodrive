# Filesystem speed

Status: planned. Needs the runner from [plan 24](24-benchmarks-and-reports.md) for before and after numbers. Each section below is its own PR.

## The constraint

Monodrive can't change what the macOS kernel and APFS charge per call. Measured on 2026-10-30, a Ryzen PC on btrfs beat an M3 Pro by 2 to 5 times on most single operations, 27 times on hard links, and 108 times on launching a freshly written binary. The Mac was under load, and the hardware differs. A RAM disk gained at most 1.4 times, so the SSD isn't the cause.

So the way to go faster is to make fewer calls. Two batched calls already beat the PC's plain equivalents in a C prototype:

| Operation | Today | Batched | PC |
|---|---|---|---|
| List 100,000 entries, no sort | 434 ms | 101 ms | 151 ms |
| Copy a folder of 10,000 small files | 1,135 ms | 94 ms | 403 ms (`cp -a --reflink`) |

These are not product numbers. The prototype builds no `Entry`, and neither Linux figure is the fastest method Linux has. Third-party tools stay at macOS speeds unless they call Monodrive.

## Whole-folder clones

`disk::copy` calls `copyfile` with `COPYFILE_CLONE | COPYFILE_RECURSIVE`, which took 12 times as long as one `clonefile` call on the folder.

Try `clonefile(from, to, CLONE_NOFOLLOW)` first, at the same hidden `.monodrive-<job>-<item>` path, and fall back to `copyfile` on `EXDEV` or `ENOTSUP`. The rest of the job stays: placeholder refusal, tally and stamp checks, rename into place, journal records. On any other failure, trash the partial clone as a failed copy does today, then retry through `copyfile`, so a single unreadable file still gets its usual error.

**Cancel and progress.** One call can't report progress or stop early. Start with the single call, and have the job report a done or not-done state for that item. Measure the largest realistic tree first. If one call passes about a second, clone each top-level child separately, checking cancel between them and applying the root's metadata last. Don't build the chunked version unless the numbers ask for it.

**Compare these against `copyfile` in tests:** extended attributes, ACLs, modification and creation times, symlinks left unfollowed, hard links within the tree, ownership, unreadable files, sockets and pipes. A difference we can't accept means falling back for that case, and `docs/operations.md` must say so.

## One bulk walker

`entries::list_dir`, `disk::tally` and `disk::stamp` each call `read_dir` plus one stat per item. A copy of one folder walks it about four times. Add one walker over `getattrlistbulk` that requests name, type, size, modification time with nanoseconds, flags and inode, and move all three onto it. The `unsafe` buffer parsing stays in that one function. Also move storage scans if a profile shows they benefit.

Handle what the prototype skipped: `ATTR_CMN_ERROR` per entry, so one bad item never hides the rest; entries that don't report every attribute; network and non-APFS volumes, with a fallback on `ENOTSUP`.

**The journal is the risk.** Stored stamps mix in each item's `st_size`, and folders report none through `ATTR_FILE_DATALENGTH`. A changed digest would make undo refuse items that didn't change. Keep digests identical by getting folder sizes from `lstat` in `stamp` only. Folders are about one entry in ten, and `list_dir` doesn't need it. A test walks a fixture both ways and requires equal entries and an equal stamp.

## Sort keys

Sorting 10,000 entries takes 21 ms by name and 35 ms by kind, over the 5 ms budget, and sorting becomes the bigger cost once listing is fast. Build each entry's key once (folded name, digit runs parsed, kind label computed), then sort on keys. Keep `natural`'s order exactly, with the existing test extended to compare old and new on generated names.

## A limit on file-work threads

Past 4 to 5 threads, metadata throughput falls on APFS and also on btrfs. Going from 5 to 11 threads costs 1.3 to 1.9 times. Where one job walks or changes many files in parallel, use one shared limit of 4, and don't scale with core count. This is a rule for parallel code. The owner's limit of two jobs at a time stays, and nothing needs converting if a path is already serial.

## Rules for later features

- **No hard links.** They're the slowest metadata operation on this Mac. Clone or symlink instead.
- **No per-item durable syncs.** `F_FULLFSYNC` ran at 549 per second here. The journal syncs per line today, which is fine for one record per change and worth watching on big batches.
- **No executables at new paths on hot paths.** Launching a fresh binary took 30 ms, against 1.2 ms for a repeat launch.

## Measure before deciding

Each of these gets a number and a decision, not code, until then:

- **Cold listing.** `purge` needs sudo, so this waits for plan 24's quiet Mac. Only if it's slow, consider drawing a remembered listing and refreshing it.
- **Trash at scale.** `NSFileManager` runs per item, untimed for thousands of items.
- **`fseventsd` load.** It used 34 hours of CPU in 7.3 days and sat near 97% in most samples. Compare it with Monodrive running and quit, with a person doing the quitting. Find out whether the index's subscription adds to it before changing anything.
- **The cause of slow fresh launches.** Compare with Terminal's Developer Tools permission on and off. If it matters, Settings or onboarding can tell developers about it.

## Reaching agents

Agents get all of this without new tools, through the contract in [plan 26](26-mcp-contract.md):

| Improvement | MCP result |
|---|---|
| Whole-folder clone | `copy` and `duplicate` get faster, and report `method`. `require_clone` fails rather than filling a disk |
| Short copies | `copy` with `wait_seconds` returns the finished job, so a 100 ms clone needs no polling |
| Bulk walker, sort keys | `list_dir` and `folder_size` get faster, with no contract change |
| Thread limit | Invisible |
| Worktree setup | [Plan 22](22-worktree-management.md)'s creation flow clones `node_modules` through the same service |

Trashing is one rename per item, so `trash` should also beat `rm -rf` on large folders. Measure that, then say so in the tool description. Only a person can empty the Trash, so agents that clean up often will grow it. Report the space involved rather than changing that rule.

## Done means

Plan 24's report shows the new path beating today's on the same machine. `docs/performance.md`'s budgets tighten to the measured results: the target for no-sort listing is the PC's 150 ms per 100,000 entries, with sort budgeted separately, since today's 376 ms baseline includes the sort. `docs/operations.md` describes the new copy path. Walk AGENTS.md's "Hit every surface" list in each PR.
