# Performance budgets

Speed is the first thing Monodrive won't compromise on, so each part that makes it feel fast has a budget. A change that pushes a number past its budget needs a fix or a deliberate new budget, written here with the reason.

| What | Budget | Baseline |
|---|---|---|
| List a folder of 10,000 entries | 40 ms | 24 ms |
| List a folder of 100,000 entries | 500 ms | 267 ms |
| Sort 10,000 shuffled entries by any column | 5 ms | 1.9 ms by name, 2.6 ms by kind, 1.4 ms by size and by date |
| Draw a frame while scrolling 10,000 entries, p99 | 4 ms | 1.6 ms list, 2.3 ms grid |
| Draw a frame while dragging a file over 10,000 entries, p99 | 4 ms | 2.5 ms |
| Launch to drawing the folder's contents, warm, release build | 150 ms | 127 ms, the middle of three medians of seven |

The baselines are from an M3 Pro, with the folders already in the disk cache: listing, sorting and launch on 2026-10-31, frame times on 2026-10-29. Compare numbers from the same machine; the budgets are for that class of Mac. Judge launch by the median of five runs, never one.

## Launch

The listing of a home folder is ready within a millisecond of being asked for, so launch holds the window back for it, up to 40 ms, without blocking the main thread. Opened first, the window draws empty, and the finished listing then waits behind AppKit activating the app: drawing took 240 ms that way. Closing onboarding opens its windows at once instead, since the app quits when it has no window.

Drawing is not the same as showing. GPUI draws the first frame as the window opens, and presents it at the first frame signal, a median of 161 ms after the process started. What's left before then is mostly AppKit laying out the window and Metal's first drawable. On the old path, the contents were presented at about 240 ms.

## Measuring

- `cargo bench -p monodrive-core --bench listing` lists and sorts synthetic folders under `target/tmp`.
- `cargo bench -p monodrive --bench scrolling` scrolls the real browser, list and grid, and carries a file over the list, through GPUI's headless Metal renderer. Its report also counts allocations per run, a number to watch: about 5 MB a frame today.
- `cargo run -p monodrive --release --features profile -- <folder>` prints the launch times and, when the app quits, frame draw times. "On the display by" is stamped at the frame signal after the present, so it's an upper bound; GPUI's `Present` events skip the first frames, so they can't time launch. Launch it a few times and take the median; the first launch after a build is slower. Without a folder it restores and saves your real session.

Criterion keeps the last run in `target/criterion` and compares the next one against it.

## Remote sources

Remote numbers are measured apart from local speed, against a throwaway server on this Mac, so they show what Monodrive adds and not a network. `cargo run --release -p monodrive-core --example remote_e2e` prints them. On an M3 Pro on 2026-10-30, a first listing of 10,000 entries takes 43 ms straight over `sftp-server` and 60 ms through ssh on loopback. Connecting through ssh takes 52 ms. Copying 300 MB through ssh runs at 328 MB/s down and 315 MB/s up, and cancel ends a running copy within about 20 ms. Peak memory stays at 16 MB over `sftp-server`.
