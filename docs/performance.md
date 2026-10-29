# Performance budgets

Speed is the first thing Monodrive won't compromise on, so each part that makes it feel fast has a budget. A change that pushes a number past its budget needs a fix or a deliberate new budget, written here with the reason.

| What | Budget | Baseline |
|---|---|---|
| List a folder of 10,000 entries | 40 ms | 30 ms |
| List a folder of 100,000 entries | 500 ms | 376 ms |
| Sort 10,000 shuffled entries by any column | 5 ms | **21 ms by name, 35 ms by kind**; 1.8 ms by size, 0.5 ms by date |
| Draw a frame while scrolling 10,000 entries, p99 | 4 ms | 1.6 ms list, 2.3 ms grid |
| Launch to the folder's contents, warm, release build | 150 ms | **236 ms**, median of five; the first frame comes at 136 ms |

The baselines are from an M3 Pro on 2026-10-29, with the folders already in the disk cache. Compare numbers from the same machine; the budgets are for that class of Mac. Judge launch by the median of five runs, never one.

## Over budget

- **Sorting by name and by kind.** Each comparison walks both names a character at a time, lowercasing as it goes, and sorting by kind also builds both kind labels as strings.
- **Launch to the folder's contents.** Listing the home folder takes under a millisecond on the background executor, but its result reaches the window about 100 ms after the first frame. Starting the index and building the menus aren't the cause; what holds the main thread is still unknown.

## Measuring

- `cargo bench -p monodrive-core --bench listing` lists and sorts synthetic folders under `target/tmp`.
- `cargo bench -p monodrive --bench scrolling` scrolls the real browser, list and grid, through GPUI's headless Metal renderer. Its report also counts allocations per run, a number to watch: about 5 MB a frame today.
- `cargo run -p monodrive --release --features profile` prints the launch times, to the first frame and to the folder's contents, and frame draw times when the app quits. Launch it a few times and take the median; the first launch after a build is slower.

Criterion keeps the last run in `target/criterion` and compares the next one against it.
