# Performance budgets

Speed is the first thing Monodrive won't compromise on, so each part that makes it feel fast has a budget. A change that pushes a number past its budget needs a fix or a deliberate new budget, written here with the reason.

The budgets are in [`benchmarks/budgets.toml`](../benchmarks/budgets.toml), and every report `scripts/bench` writes judges the run against them in its Budgets table. They're for an M3 Pro-class Mac with the files in the disk cache. Compare numbers from the same machine, and judge launch by the median of five runs, never one.

The last baselines before `scripts/bench`, on an M3 Pro: listing 10,000 entries took 10 ms and 100,000 took 127 ms, and sorting 10,000 took 0.9 ms by name, 1.5 ms by kind and 0.5 ms by size and by date, on 2026-11-03. Frames took 1.6 ms (list) and 2.3 ms (grid) at p99 while scrolling, 2.5 ms while dragging, and 1.5 to 2.2 ms while the sidebar slides. Launch took 127 ms, the middle of three medians of seven, on 2026-10-31.

## Listing

A listing asks `getattrlistbulk` for each batch of entries' names, types, sizes, dates, flags and inodes, where `read_dir` needed an `lstat` per entry. A copy's checks walk with the same call: `tally` and `stamp`, which together walked a copied folder about four times, item by item. `stamp` still takes each folder's size from `lstat`, since the journal keeps stamps made with it and the bulk call reports none for folders. The kernel reading each entry's inode is still most of the time: about 4.5 ms of the 10 ms for 10,000 entries. Sorting writes every entry's key into one buffer and its first eight bytes into a number, so most comparisons are one integer compare.

## File work

Monodrive can't change what the kernel and APFS charge per call, so it goes faster by making fewer calls: a listing, a copy's checks and a whole copy each take one call per batch or per folder rather than one per item. These rules keep later features on that path.

- **At most 4 threads for file work.** Past 4 to 5 threads, metadata throughput falls on APFS and on btrfs alike. Where one job walks or changes many files at once, share a limit of 4 rather than one per core. Today nothing in `monodrive-core` runs file work on more threads: finding duplicates reads with 2.
- **No hard links.** They're the slowest metadata call on this Mac: 1.5 ms each on one thread on 2026-11-09, under load, against 14 µs on donnes-pc. Clone or symlink instead.
- **No durable sync per item.** `F_FULLFSYNC` took 1.8 to 3.3 ms a call here. The journal syncs once per line, which is fine for one record per change and worth watching on big batches.
- **No newly written executables on a hot path.** Launching a binary at a path never run before took 45 to 77 ms, against about 1 ms for launching one again, since macOS checks a new executable on its first launch.

Measured and decided:

- **Trash at scale.** Move to Trash goes through `NSFileManager` one item at a time: 1,000 selected files took 0.66 s on 2026-11-09, at load 5, on the core suite's disk image. That's fine as one request for now; past a few thousand items it's worth becoming a job with progress. A whole folder is one rename, and a 20,000-entry `node_modules` went to the Trash in 0.45 to 0.5 s against 1.8 s for `rm -rf`, which the `trash` tool's description now tells agents. Only a person empties the Trash, so agents that clean up often grow it, and the description says so rather than changing that rule.

Not decided yet, because they need a measurement this Mac can't make:

- **Cold listing** needs `purge` and so sudo, which the quiet Mac will have. Only if it's slow is drawing a remembered listing first worth building.
- **`fseventsd`'s load** needs a person to quit Monodrive and compare, to find out whether the index's subscription adds to it.
- **Slow fresh launches** need a person to switch Terminal's Developer Tools permission off and on and rerun `scripts/bench platform`. If the permission matters, Settings or onboarding can say so.

## Launch

The listing of a home folder is ready within a millisecond of being asked for, so launch holds the window back for it, up to 40 ms, without blocking the main thread. Opened first, the window draws empty, and the finished listing then waits behind AppKit activating the app: drawing took 240 ms that way. Closing onboarding opens its windows at once instead, since the app quits when it has no window.

Drawing is not the same as showing. GPUI draws the first frame as the window opens, and presents it at the first frame signal, a median of 161 ms after the process started. What's left before then is mostly AppKit laying out the window and Metal's first drawable. On the old path, the contents were presented at about 240 ms.

## Measuring

`scripts/bench` builds in release and runs the suites, writing every sample to `target/bench/<time>/results.json` and a report beside it, `report.html`, made from that file alone.

```
scripts/bench [suite...]
scripts/bench --against <commit> [suite...]
scripts/bench report <results.json> [saved results.json]
scripts/bench linux [results.json]
```

- **platform** times what the kernel and file system charge with no Monodrive code: creating, stat-ing, reading, linking, cloning, copying and deleting 4,800 files at 1 to 24 threads, launching a process again and a binary written just before, each kind of flush, and cloning a 100,000-entry `node_modules` with one `clonefile` against `copyfile`'s recursive clone. It needs only libc, so it runs on Linux too.
- **core** times `list_dir` at 1,000, 10,000 and 100,000 entries, `sort` by each column, Suggest Names on five text files, and whole copy, duplicate, move and Move to Trash jobs on a 20,000-entry `node_modules`, through an owner on its own `ops::Paths` in the fixture. Trashing happens on a disk image the suite attaches and detaches, never in the user's Trash. A copy's check reports the bytes its files don't share, so a clone that silently became a copy shows.
- **search** times name search on a generated index of 3 million entries, by query type, through a `Searcher` and through an `Engine`, with nothing crawled and the live index never opened. See [Search](#search).
- **mcp** starts `monodrive mcp` with `HOME` in the fixture and times start to first reply, `tools/list`, and `find_files`, `grep`, `list_dir` and `copy` round trips.
- **workload** times work a developer waits on: `pnpm install` of `benchmarks/workload`'s pinned lockfile from a store already on disk, `rm -rf` of the `node_modules` it made, and `git worktree add` and `remove` on a generated repository. It measures the platform, not Monodrive, and runs on Linux too. The store is fetched once into `target/bench/pnpm-store` and kept.
- **compare** puts Monodrive beside what someone would use instead, with their fastest documented flags: a copy job against `cp -c -R`, `ditto` and `rsync -a`, `list_dir` against `find` and `ls -1`, and Move to Trash against `rm -rf`, on a disk image. The others run as processes, as a person or agent runs them. Losses stay in the report.

Fixtures are generated from a seed, with the depths and sizes of the folders in `benchmarks/shapes`, measured from a real `node_modules` and a real Downloads folder (`monodrive-bench shape <folder>` makes one). They live in `target/bench/<time>/fixtures` and are deleted when the suite ends.

**How a run stays honest.** Each round of each build is its own `run-one` process at a fixed QoS class. Round 0 is thrown away. With `--against`, the two builds take turns, A B B A, on the same fixtures, and each result gets a verdict from a 95% bootstrap interval on the ratio of medians: faster, slower, or no clear difference, which never carries a multiplier. A suite is noisy, and its budgets aren't judged, when the load before it is past a quarter of the cores, the Mac is on battery, or more than a quarter of its results have a p90 past 1.5 times their median. Each sample also records CPU time and, on macOS, the instructions and cycles `proc_pid_rusage` counts for the whole process. Counters vary far less than wall time under load, but they don't see time spent waiting on a lock or a flush, so they sit beside wall time rather than replace it. `monodrive-bench counters` says whether a machine's counters work. GitHub's macOS runners count none (checked on 2026-11-09), so pull requests get no instruction gate; the quiet Mac's workflow runs `monodrive-bench gate`, which fails when any result takes more instructions than the commit before it, past 3%.

`--against <commit>` makes a worktree of that commit in `target/bench/baseline-<commit>`, builds it with its own target folder, and keeps only the latest such build, since each takes several GB. The commit must contain `crates/monodrive-bench`. `report <results.json> <saved results.json>` adds a saved run's medians beside this one's, with no verdict, since samples from different runs aren't compared.

`linux` runs the platform and workload suites on `donnes-pc` over ssh, from a checkout in `~/.cache/monodrive-bench` that keeps its build, with fixtures inside it that the run removes, and merges the results into a run's `results.json`. Its report shows the two machines side by side under "different hardware", with no verdict. Run it only when nothing else is running there.

Results stay out of the repository. Publish a run with its `results.json`, from a quiet machine: plugged in, nothing else running, and no "Noisy run" at the top of the report.

**The quiet Mac.** Wall time and published numbers need a Mac that runs nothing else, since a shared VPS's noise is its other tenants and macOS only runs on Apple hardware: a dedicated Mac mini, with no agents, no signed-in iCloud, a pinned macOS build, and FileVault and Spotlight left on because users have them on. Give it a separate APFS volume for fixtures and passwordless sudo for `purge` alone, for cold-cache runs. Register it as a self-hosted GitHub runner labeled `bench` and set the repository variable `BENCH_RUNNER` to `true`; `.github/workflows/bench.yml` then runs the suites on each push to main against the commit before, and on a maintainer's dispatch, and keeps the report as an artifact. It never runs for pull requests, so no untrusted code reaches that machine, and it holds no signing credentials. For a same-CPU Linux result, run a Linux VM on it and label the result as virtualized.

The scrolling benchmark and the launch profile aren't in `scripts/bench` yet, because they don't write raw samples:

- `cargo bench -p monodrive --bench scrolling` scrolls the real browser in the list, the grid and column view, carries a file over the list, and slides the sidebar closed and open over the list and the grid, through GPUI's headless Metal renderer. Its report also counts allocations per run, a number to watch: about 5 MB a frame today.
- `cargo run -p monodrive --release --features profile -- <folder>` prints the launch times and, when the app quits, frame draw times. "On the display by" is stamped at the frame signal after the present, so it's an upper bound; GPUI's `Present` events skip the first frames, so they can't time launch. Launch it a few times and take the median; the first launch after a build is slower. Without a folder it restores and saves your real session.

## Search

The search suite never opens the live index. It builds an index of 3 million generated names, using the Spotlight comparison's model of real folders, from listings in memory, and saves it in the fixture. A `Searcher` on that index times the matching alone, with the window's limit of 200 results, a query at a time: a rare word, a common word, the middle of a word, two letters, Japanese, a word and a number, a name with its extension, a typo, a word typed without its accent, and no match. Each is its own result, since they differ by a hundred times.

An `Engine` on a clone of the same file adds what the app goes through: its first answer after starting, then the same queries. Its `HOME` is an empty folder, so the content index has nothing to read. It still follows FSEvents for the whole disk and lists each folder that changes into its overlay, so its numbers depend on what else writes to the disk. On 2026-11-09, with other builds running, one round applied 5 entries and another 8,746. Each Engine row's check says how many, and the suite fails if the Engine's own index ever differs from the generated one, which would mean it rebuilt or crawled. Trust the `Searcher` rows for matching, and Engine rows only from a quiet machine.

The MCP suite's index also holds one real folder, the one `grep` reads, so its round trip picks files the way it does on a real disk. Before any search the suite checks that the server sees the saved index, and after the first that it holds the generated count; otherwise it stops the server, since a search would start a crawl.

`scripts/bench search-real` runs the same queries on a clone of this Mac's saved index, `~/Library/Application Support/FSearch/index.bin`, through `Index::load` and a `Searcher` only: no `Engine`, watcher or lock. Its report holds times and hit counts, never paths, and it deletes the clone. It isn't in the default run.

## Comparing with other tools

`scripts/bench spotlight` compares name search with Spotlight, `find` and `fd` on the same files. It needs no sudo and no screen capture.

```
scripts/bench spotlight prepare [entries] [seed]
scripts/bench spotlight run [rounds]
scripts/bench spotlight clean
```

`spotlight prepare` makes `target/bench/corpus.sparseimage`, a 64 GB sparse image holding only what's written to it, and mounts it at `/Volumes/MonodriveBench`, which shows in Finder's sidebar until `spotlight clean`. Spotlight leaves a `nobrowse` volume unindexed, so it has to be a normal mount. It writes a corpus from the seed with `manifest.tsv` beside it, then waits until Spotlight has every entry. Spotlight took 12 to 16 entries a second on an M3 Pro on battery under load, so a 100,000-entry corpus can take about two hours.

`spotlight run` writes every sample and the run's conditions to `target/bench/<time>/results.json`, prints a table, and writes `report.html` beside it from that file alone, with `post.html` and a 2400 by 1350 `post.png` for posting on X. `scripts/bench report <results.json>` rewrites all three. Tools run in turns and each query's first round is thrown away. fsearch builds a throwaway index of the corpus in memory, with no `Engine`, so nothing crawls and the live index stays closed.

- **Indexing gets two times and no ratio.** `spotlight run` remounts the volume, times fsearch's first build, then ten more. `spotlight prepare` times Spotlight from the first file written until it has every entry. fsearch lists names, sizes and dates in the foreground. Spotlight also reads contents and metadata, as a background service, so dividing one by the other says nothing about either engine. Remounting doesn't clear what macOS holds of the disk image's file, so the first build isn't a true cold read, which would need `purge` and sudo. fsearch's time on a real disk is a separate question this doesn't answer.

- **The verdict compares fsearch with Spotlight through `MDQuery`.** Both run inside the process and return paths. `mdfind` takes about 50 ms even for no match, and most of that is the command, not Spotlight, so `mdfind`'s numbers show what a person or agent at a shell gets and don't belong in a claim about engines.
- **Accuracy goes beside speed.** The answer key is every name holding the query's words, ignoring case and accents. `missed` counts names a tool should have returned. `extra` counts results outside the key. fsearch matches fuzzily, so it isn't an error count. A typo query's key is the word meant, so tools with no typo tolerance miss all of it.
- **A run that shouldn't be published says so.** Load past a quarter of the cores, battery power or a part-indexed Spotlight print `NOISY RUN` and go in the results.

For numbers to publish, follow these rules: plugged in, a quiet Mac or a separate macOS account with nothing else signed in, at least 30 rounds, and the run's `results.json` published with them. Both tools search the whole disk in real use, and this measures one volume, so a claim has to name the corpus size.

fsearch finds an accented word typed without its accent only at the start of a name: on 2026-11-01, `relatorio` found `relatório-backup.pptx` and missed `Final-relatório.md`, which Spotlight finds. The `no accent typed` query tracks this.

On 2026-11-01 Spotlight found 0 of 5 names for `日本語` on the 10,000-entry corpus, through both `MDQuery` and `mdfind`, while `find` and `fd` found all 5. The same predicate found every such name on a home folder and on a 10-entry volume, so this is unexplained and may come from the benchmark, not Spotlight. The report keeps the query and the post image leaves it out until someone finds the cause.

Still to build: content search, whole-disk first-index time, a new file becoming findable, memory and disk use, and what a person sees from keystroke to pixels, which needs screen capture and so a sign-off. The plan is [plan 24](../.plans/24-benchmarks-and-reports.md).

## Remote sources

Remote numbers are measured apart from local speed, against a throwaway server on this Mac, so they show what Monodrive adds and not a network. `cargo run --release -p monodrive-core --example remote_e2e` prints them. On an M3 Pro on 2026-10-30, a first listing of 10,000 entries takes 43 ms straight over `sftp-server` and 60 ms through ssh on loopback. Connecting through ssh takes 52 ms. Copying 300 MB through ssh runs at 328 MB/s down and 315 MB/s up, and cancel ends a running copy within about 20 ms. Peak memory stays at 16 MB over `sftp-server`.
