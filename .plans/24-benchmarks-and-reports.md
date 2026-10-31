# Benchmarks and reports

Status: planned. Do this first: plans 25 and 26 claim speed, and this plan is how they prove it. The numbers behind those plans came from throwaway C programs in `target/fs-bench`, run on a Mac at load 13 to 28. They are good enough to choose work and not good enough to publish.

## Goal

One command runs the benchmarks and writes a self-contained HTML report. Every number in the report traces back to raw samples, and the report says when a result can't be trusted. Today `docs/performance.md` holds hand-copied numbers, and nothing measures copies, the MCP round trip, search, or anything on Linux.

## Rules that keep it truthful

- **Raw samples are the record.** The runner writes every sample to JSON. The report only reads that file, so no chart can show a number the run didn't produce.
- **Every run records its conditions.** Commit, build profile, machine, macOS build, volume format, FileVault, power source, thermal state, and load before and after each suite.
- **Compare by alternating.** A and B run in turns (A B B A), so drift hits both, and the first round of each is thrown away so both start warm. Compare only samples from the same run. A and B are usually two builds: see `--against` below.
- **Each comparison gets a verdict.** Faster, slower or no clear difference, from a bootstrap interval on the ratio of medians. A report never prints a multiplier for "no clear difference".
- **A noisy run says so.** Past a set load or spread, the report marks the run as noisy at the top and budgets don't pass or fail on it.
- **Show the spread.** Median, minimum and p90 beside each result, with the sample count. Bars never hide the range.
- **Prove the work happened.** After each operation, check it: entry counts for a copy, that a clone shares storage when it claims to, and result counts for a listing.
- **Label warm and cold.** Warm-cache and cold-cache results never share a chart.
- **Label cross-machine charts.** A Mac-against-PC chart says "different hardware" in its title. Same-machine charts carry that claim only when it's true.
- **Name the competition honestly.** Compare with what someone would use instead: `find`, `cp -c`, `ditto`, `rsync`. Don't compare against a configuration nobody runs, and use the competitor's fastest documented flags.
- **Publish losses.** Results where Monodrive loses stay in the report.

## What gets measured

Each suite records wall time, CPU time, and the process's instructions and cycles from `proc_pid_rusage`.

| Suite | Runs on | Measures |
|---|---|---|
| Platform | Mac, PC | create, stat, open and read, symlink, hard link, clone, copy, delete at 1 to 24 threads; spawn of the same and of a fresh binary; each flush type |
| Monodrive core | Mac | `list_dir`, `sort` by column, and whole copy, duplicate, move and trash jobs through the owner |
| Search | Mac | name search on a generated index of 3 million entries, by query type |
| MCP | Mac | `monodrive mcp` start to first reply, `tools/list` bytes, `find_files`, `grep`, `list_dir` and `copy` round trips over stdio |
| Real workload | Mac, PC | a pinned lockfile's `pnpm install` from an offline store, `git worktree add`, deleting the result |
| Existing | Mac | the scrolling benchmark and launch profile, imported as samples |

Fixtures are generated from a seed and checked-in size and depth distributions, taken from a real `node_modules` and a real Downloads folder. Every fixture lives in a temp folder. The owner runs on its own `ops::Paths` in that folder, so it never joins the user's owner or journal. Trash suites run only on a disk image the runner attaches and detaches, because trashing on the startup disk would fill the user's Trash. The clippy and no-sleep rules still apply to tests. The runner itself may need `#[expect]` for deleting its own fixtures.

## Measuring search without a crawl

Search needs no crawl and never opens the live index. fsearch's public `walk::Listing` and `Index::build` take listings from memory, as its own `snap` example does, so the suite generates them from the seed. A prototype on 2026-10-30 built 3,001,001 entries in 1.4 s and a 154 MB `index.bin`, searched them in memory, and started an `Engine` on the saved file. The first answer came in 19 ms, and 10 seconds later the entry count hadn't changed and the overlay was empty, so nothing had been crawled.

- **Two levels.** `Index::load` plus `Searcher` times the matching alone. An `Engine` on the same saved file adds what the app really goes through. It needs `event_id` 0 and an empty `skipped` file beside `index.bin`, because a missing one makes it rescan the gated folders, /Volumes among them. Don't sample until its first compaction finishes: that run spent 11.5 s of CPU in those 10 seconds, of which the log explains only the 1.45 s compaction. Find out what the rest was before trusting Engine numbers.
- **Queries.** A fixed set, each reported separately: a rare word, a common word, a word and a number, a name with an extension, a typo, and no match.
- **Names decide the result.** In the prototype a query matching 1 in 8 names took 6 to 7 ms, while a typo took 0.9 ms and no match 0.2 ms. Its names were crude, so these aren't product numbers, but they show one median can't stand for search. Take the name distribution from real folders. Set the search budget per query type from the first real run, and only then decide whether "about a millisecond" still holds, and for which queries.
- **`find_files` over MCP.** The suite starts `monodrive mcp` with `HOME` set to a temp folder holding that `index.bin` and `skipped` under `Library/Application Support/FSearch`. This path is untried. Before any sample it refuses to run unless the child's `index_status` reports the generated entry count, and it stops the child by captured PID if not. `support_dir` follows `HOME`, so Monodrive's own folder moves with it.
- **The real index, on request.** `scripts/bench search-real` copies the saved `index.bin` to a temp folder and opens the copy with `Index::load` and `Searcher` only, so no `Engine`, watcher or lock. It gives the true disk's numbers, is never in the default run or CI, and the report holds query times and hit counts, not paths. It removes the copy afterwards.

fsearch's internals stay in this one file of the benchmark crate, as they stay in `index.rs` in the app. An fsearch bump can break it, and the entry-count check is what catches that.

## How it's built

`crates/monodrive-bench` is a binary with no GPUI. The platform suite uses only `libc`, so it builds on Linux. Monodrive suites sit behind `cfg(target_os = "macos")`. This is a signed-off exception to AGENTS.md's no-`cfg` rule (2026-10-30). It covers this crate only and adds no Linux support to the app.

The crate calls only what `monodrive-core` already makes public: `list_dir`, `sort`, `ops` requests and the MCP server over stdio. `disk` stays `pub(crate)`, and nothing gets exported just for benchmarks. So `tally`, `stamp` and `disk::copy` have no rows of their own. Their cost shows up in the job that runs them, which is also what a person or agent waits for.

- `scripts/bench [suite]` builds in release and runs, writing `target/bench/<timestamp>/results.json` and `report.html`.
- `scripts/bench --against <commit> [suite]` compares two builds. It makes a detached worktree of that commit under `target/bench/`, builds its benchmark tool there with its own target folder, and has the current tool run each build's `run-one --suite <name>` in turns on the same fixtures, reading samples as versioned JSON. It keeps one baseline build, the most recent, and removes the others, since a build takes several GB. The commit must contain the crate, so the runner has to land before the first speed change it judges.
- `scripts/bench report <results.json> [baseline.json]` rebuilds a report from saved data.
- `scripts/bench linux` runs the platform suites on `donnes-pc` over ssh from a checkout there, using a temp folder it removes, then merges the results.
- The report is one HTML file with inline CSS and SVG, no JavaScript dependencies and no network requests. It follows `dataviz`: one axis per chart, a table beside each chart, light and dark, and hover values. Log scales say so in the subtitle.

Budgets move from the table in `docs/performance.md` to `benchmarks/budgets.toml`, and the doc links to the report's budget table. Results stay out of the repo except for one baseline file per quiet machine, updated on purpose. PR evidence follows AGENTS.md: upload the report to GitHub, never commit it.

Once the runner covers `list_dir` and sorting, delete `benches/listing.rs` and the `criterion` dev-dependency, so there's one system. Keep `scrolling.rs`'s measuring code until it can emit samples.

## Removing the noise

Noise gets three separate treatments, in this order.

**1. Counters first.** On this Mac, while load fell to about 3.5, instruction counts varied by 0.02% for user code and 2.7% for a kernel-heavy `lstat` walk, while wall time varied by 29 to 85%. The kernel-heavy count is too large to be user code alone, so it includes kernel work. Counters don't see time spent blocked on a lock or a flush, so they guard regressions and don't replace wall time. Confirm the spread during heavy load before gating on it. GitHub's `macos-latest` could run these if the counters work inside its VMs. Check that first; if they read zero there, this gate needs the quiet Mac.

**2. Measure well on any machine.** Alternating runs, the load gate, the minimum and median together, and a pinned QoS class. These are free and also make runs on the everyday MacBook useful, though only for same-run comparisons.

**3. A quiet Mac for wall time and published numbers.** A shared VPS is the wrong tool, because other tenants are the noise, and macOS only runs on Apple hardware anyway. The choice for macOS is a dedicated Mac mini, rented or bought. It should run nothing else: no agents, no signed-in iCloud, a pinned macOS build, and FileVault and Spotlight left on because users have them on. Add a separate APFS volume for fixtures and passwordless sudo for `purge` only, so cold-cache runs work.

Linux needs no rental. `donnes-pc` varied by 1.5 to 3% while idle, which is already better than the Mac will be. Run it only when nothing else is on it, and record that it's btrfs on LUKS. For a same-CPU Linux result, run a Linux VM on the quiet Mac and label it as virtualized.

## Security

A self-hosted runner must not run code from untrusted PRs. The bench runner triggers on pushes to `main` and on a maintainer's manual dispatch, never on `pull_request`, and it holds no release signing credentials.

## Order

1. Runner, JSON format, conditions, counters, verdicts, and the platform suite ported from `target/fs-bench`. Mac and PC.
2. Report generator and `scripts/bench`.
3. `--against`, then Monodrive core, search and MCP suites, budgets file, retiring `listing.rs`. Plan 25's work starts after this step, so its first change has a baseline.
4. Real-workload suite and competitor comparisons.
5. Counter gate in CI, if the counters work there. Quiet Mac and its runner.

## Verification

Test what can lie: the verdict logic (a real difference, no difference, too few samples), the noisy-run decision, and the budget check, with fixed sample data. Skip layout tests, and look at the rendered report instead. Before trusting the suite, run it against a deliberately slowed build and confirm the report says slower, then against two identical builds and confirm it says no clear difference. Do both through `--against`. Run the search suite once while watching that the live `index.bin` and its folder don't change.
