# Performance budgets

Speed is the first thing Monodrive won't compromise on, so each part that makes it feel fast has a budget. A change that pushes a number past its budget needs a fix or a deliberate new budget, written here with the reason.

| What | Budget | Baseline |
|---|---|---|
| List a folder of 10,000 entries | 40 ms | 10 ms |
| List a folder of 100,000 entries | 500 ms | 127 ms |
| Sort 10,000 shuffled entries by any column | 5 ms | 0.9 ms by name, 1.5 ms by kind, 0.5 ms by size and by date |
| Draw a frame while scrolling 10,000 entries, p99 | 4 ms | 1.6 ms list, 2.3 ms grid |
| Draw a frame while dragging a file over 10,000 entries, p99 | 4 ms | 2.5 ms |
| Draw a frame while the sidebar slides over 10,000 entries, p99 | 4 ms | 1.5 ms list, 2.2 ms grid, on 2026-11-03 |
| Launch to drawing the folder's contents, warm, release build | 150 ms | 127 ms, the middle of three medians of seven |

The baselines are from an M3 Pro, with the folders already in the disk cache: listing and sorting on 2026-11-03, launch on 2026-10-31, frame times on 2026-10-29. Compare numbers from the same machine; the budgets are for that class of Mac. Judge launch by the median of five runs, never one.

## Listing

A listing asks `getattrlistbulk` for each batch of entries' names, types, sizes, dates and flags, where `read_dir` needed an `lstat` per entry. The kernel reading each entry's inode is still most of the time: about 4.5 ms of the 10 ms for 10,000 entries. Sorting writes every entry's key into one buffer and its first eight bytes into a number, so most comparisons are one integer compare.

## Launch

The listing of a home folder is ready within a millisecond of being asked for, so launch holds the window back for it, up to 40 ms, without blocking the main thread. Opened first, the window draws empty, and the finished listing then waits behind AppKit activating the app: drawing took 240 ms that way. Closing onboarding opens its windows at once instead, since the app quits when it has no window.

Drawing is not the same as showing. GPUI draws the first frame as the window opens, and presents it at the first frame signal, a median of 161 ms after the process started. What's left before then is mostly AppKit laying out the window and Metal's first drawable. On the old path, the contents were presented at about 240 ms.

## Measuring

- `cargo bench -p monodrive-core --bench listing` lists and sorts synthetic folders under `target/tmp`.
- `cargo bench -p monodrive --bench scrolling` scrolls the real browser in the list, the grid and column view, carries a file over the list, and slides the sidebar closed and open over the list and the grid, through GPUI's headless Metal renderer. Its report also counts allocations per run, a number to watch: about 5 MB a frame today.
- `cargo run -p monodrive --release --features profile -- <folder>` prints the launch times and, when the app quits, frame draw times. "On the display by" is stamped at the frame signal after the present, so it's an upper bound; GPUI's `Present` events skip the first frames, so they can't time launch. Launch it a few times and take the median; the first launch after a build is slower. Without a folder it restores and saves your real session.

Criterion keeps the last run in `target/criterion` and compares the next one against it.

## Comparing with other tools

`scripts/bench` compares name search with Spotlight, `find` and `fd` on the same files. It needs no sudo and no screen capture.

```
scripts/bench prepare [entries] [seed]
scripts/bench search [rounds]
scripts/bench clean
```

`prepare` makes `target/bench/corpus.sparseimage`, a 64 GB sparse image holding only what's written to it, and mounts it at `/Volumes/MonodriveBench`, which shows in Finder's sidebar until `clean`. Spotlight leaves a `nobrowse` volume unindexed, so it has to be a normal mount. It writes a corpus from the seed with `manifest.tsv` beside it, then waits until Spotlight has every entry. Spotlight took 12 to 16 entries a second on an M3 Pro on battery under load, so a 100,000-entry corpus can take about two hours.

`search` writes every sample and the run's conditions to `target/bench/<time>/results.json`, prints a table, and writes `report.html` beside it from that file alone, with `post.html` and a 2400 by 1350 `post.png` for posting on X. `scripts/bench report <results.json>` rewrites all three. Tools run in turns and each query's first round is thrown away. fsearch builds a throwaway index of the corpus in memory, with no `Engine`, so nothing crawls and the live index stays closed.

- **Indexing gets two times and no ratio.** `search` remounts the volume, times fsearch's first build, then ten more. `prepare` times Spotlight from the first file written until it has every entry. fsearch lists names, sizes and dates in the foreground. Spotlight also reads contents and metadata, as a background service, so dividing one by the other says nothing about either engine. Remounting doesn't clear what macOS holds of the disk image's file, so the first build isn't a true cold read, which would need `purge` and sudo. fsearch's time on a real disk is a separate question this doesn't answer.

- **The verdict compares fsearch with Spotlight through `MDQuery`.** Both run inside the process and return paths. `mdfind` takes about 50 ms even for no match, and most of that is the command, not Spotlight, so `mdfind`'s numbers show what a person or agent at a shell gets and don't belong in a claim about engines.
- **Accuracy goes beside speed.** The answer key is every name holding the query's words, ignoring case and accents. `missed` counts names a tool should have returned. `extra` counts results outside the key. fsearch matches fuzzily, so it isn't an error count. A typo query's key is the word meant, so tools with no typo tolerance miss all of it.
- **A run that shouldn't be published says so.** Load past a quarter of the cores, battery power or a part-indexed Spotlight print `NOISY RUN` and go in the results.

For numbers to publish, follow these rules: plugged in, a quiet Mac or a separate macOS account with nothing else signed in, at least 30 rounds, and the run's `results.json` published with them. Both tools search the whole disk in real use, and this measures one volume, so a claim has to name the corpus size.

fsearch finds an accented word typed without its accent only at the start of a name: on 2026-11-01, `relatorio` found `relatório-backup.pptx` and missed `Final-relatório.md`, which Spotlight finds. The `no accent typed` query tracks this.

On 2026-11-01 Spotlight found 0 of 5 names for `日本語` on the 10,000-entry corpus, through both `MDQuery` and `mdfind`, while `find` and `fd` found all 5. The same predicate found every such name on a home folder and on a 10-entry volume, so this is unexplained and may come from the benchmark, not Spotlight. The report keeps the query and the post image leaves it out until someone finds the cause.

Still to build: content search, whole-disk first-index time, a new file becoming findable, memory and disk use, and what a person sees from keystroke to pixels, which needs screen capture and so a sign-off. The plan is [plan 24](../.plans/24-benchmarks-and-reports.md).

## Remote sources

Remote numbers are measured apart from local speed, against a throwaway server on this Mac, so they show what Monodrive adds and not a network. `cargo run --release -p monodrive-core --example remote_e2e` prints them. On an M3 Pro on 2026-10-30, a first listing of 10,000 entries takes 43 ms straight over `sftp-server` and 60 ms through ssh on loopback. Connecting through ssh takes 52 ms. Copying 300 MB through ssh runs at 328 MB/s down and 315 MB/s up, and cancel ends a running copy within about 20 ms. Peak memory stays at 16 MB over `sftp-server`.
