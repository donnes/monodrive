# Video thumbnails

Status: built.

## Goal

Videos in the grid and inspector show a frame instead of an icon, with the same rules as pictures: off the UI thread, local files only, and nothing draws late enough to feel slow.

`thumbnails.rs` handles pictures only. It reads the whole file through the `image` crate and skips anything over 64 MB, and it keeps results only in memory. None of that works for video.

## Decision: `QLThumbnailGenerator`

Use the system's generator as the first choice, and only build our own if step 1 proves it inadequate.

Measured on an M3 Pro with macOS 27.0.1, using synthetic 120-second files in a warm disk cache, three runs each:

| File | `QLThumbnailGenerator` | `AVAssetImageGenerator` |
|---|---|---|
| 4K HEVC, 300 MB | 216 ms, then 3 to 4 ms | 254 ms, then 30 to 34 ms |
| 1080p H.264, 120 MB | 40 ms, then 4 to 29 ms | 12 to 14 ms |
| `.webm` (VP9) | Fails | Fails |
| `.mkv` (H.264) | Fails | Fails |

The two decode at similar speed. The difference is what we'd have to own:

- **Cache.** The generator has a system cache behind it, so repeats returned in 3 to 4 ms. Direct decoding needs our own disk cache, with eviction, a key, a size limit, an exception to the no-delete lint, and a way to purge thumbnails of private files that were since deleted.
- **Frame choice.** The generator picks the frame, the same one Finder shows. Direct decoding lets us pick, but then we'd have to solve black opening frames ourselves.
- **Reach.** Adding PDFs and Office files later would be one list change on the generator.

The tradeoff is less control: no chosen frame, no duration, and opaque errors. A file manager matching Finder here is fine.

Step 1, measured through a fresh `monodrive mcp` process per call on 720p files: the first call took 42 to 65 ms, and the next process took about 20 ms, so the system's cache outlives a process, and the generator works with no window. `mov`, `mp4`, `m4v`, `mpg` and `3gp` work; `avi`, `mkv` and `webm` fail. `ts` works too, but that extension is more often TypeScript, so it keeps its icon. A real share, a reboot and a folder behind a privacy prompt weren't measured, so a request gives up after 20 seconds and a person still needs to check the prompt.

## What to build

Core gets the generation, because MCP uses it too:

```rust
// monodrive_core::thumbnails
pub fn generate(path: &Path, side: u32, cancel: &Cancel) -> Result<Bitmap, String>;
```

`Bitmap` is width, height and BGRA bytes. The generator's callback lands on its own queue and sends through a channel. Cancel maps to `cancel(_:)` on the request. Add `objc2-quick-look-thumbnailing` to core, with narrow features.

The app's `thumbnails.rs` changes in small ways:

- `wanted` also accepts the five extensions step 1 confirmed. `.avi`, `.webm` and `.mkv` stay on their icon. Kind is still decided by extension, as `docs/search.md` explains.
- `decode` picks by kind. The 64 MB limit stays for pictures only.
- Videos get their own permit pool of 2. A NAS stalled on a seek must not hold up pictures, listings or searches.
- **Skip what's gone.** Today a thumbnail that's released while still queued is dropped only after it finishes. That's tolerable for pictures, but scrolling past 5,000 videos would queue 5,000 pending frames in order, and the row on screen would wait behind them all. Give the loader a shared set of live thumbnails, captured from the `Holders` global when `load` runs. After taking a permit, it skips the work if its thumbnail has been released. Pictures get this too, since it's the same code.
- The existing `thumbnails` preference, `Availability::Local` gate and `Thumb` key stay. A newer file is a new thumbnail because `modified` is in the key.
- The inspector needs no change, since `swap_when_ready` goes through `wanted` and `get`.

No duration or play badge in this plan. If wanted later, read duration with `AVURLAsset` once in `info.rs`, and draw it from there.

## Why not ffmpeg

`.mkv` and `.webm` get no thumbnails here, because Apple's generators failed to open both test files. A bundled ffmpeg would fix that, as a separate helper process so it can't crash the app and stays a replaceable LGPL program. `deny.toml` only checks Rust crates, so it wouldn't block this. The costs are roughly tens of MB (an estimate), signing, security updates, and parsing untrusted media. Revisit it if those formats turn out to matter.

## Order

1. **Measure.** Against real videos, on the startup disk and an external or SMB one: does the system cache survive a new process and a reboot, what does a cold scroll through 500 videos cost, what happens on a stalled share, and does the generator work in the `monodrive mcp` process, which has no window. List which extensions work. If the cache doesn't persist or a share can't be cancelled, fall back to `AVAssetImageGenerator` with `appliesPreferredTrackTransform`, a 10% seek point, positive time tolerance and a small disk cache, and rewrite this plan's decision.
2. **Grid and inspector.** Everything under "What to build".
3. **MCP `thumbnail` tool.** `path` and `side`, returning MCP image content, the first non-text reply in the server. Agents can't watch a video or read a HEIC, so this gives them something they can't get otherwise. Read-only, and it respects the placeholder gate. Decide its place in plan 26's profiles against the 16 KB budget.

## Hit every surface

- **UI and MCP.** Generation lives in core, and the view and tool both call it.
- **Entry points.** None new. It follows the Thumbnails setting.
- **Reverse states.** Turning off Thumbnails gives icons back. A failed generation stays on the icon, without a retry loop or an error row.
- **Permissions.** The generator must not trigger a privacy prompt. Check this in step 1, on a consent-gated folder without Full Disk Access. If it can prompt, gate it as the inspector gates pictures.
- **Cloud.** Placeholders keep their icon, as pictures do. Add the placeholder case for a video to the existing test.

## Verification

- Extend `only_local_picture_files_get_thumbnails` with video extensions and the formats refused.
- Core: `generate` on a tiny committed `.mp4` fixture with a rotation flag returns the rotated shape, and on a `.mkv` returns an error.
- Regression test for skipping released work: with permits held, queue several thumbnails, release all but the last, then let the permits go. Only the last generates. It extends `drawn_thumbs` in `browser/tests.rs` if the seam fits, and it must fail on today's code.
- Rerun `scrolling` on a grid. The UI thread gains no work, so frame times shouldn't move. Add a synthetic video grid only if they do.
- Snapshots of a video folder, dark and light. The snapshot example has to wait for generation, as it does for pictures.
