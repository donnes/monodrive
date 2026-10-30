# Monodrive

Monodrive is a fast file manager for macOS, and only macOS for now (Linux may come later; Windows never will), in the spirit of Spacedrive and a replacement for Finder. It is a native Rust app built with GPUI, styled like Monocode, and searches the whole disk through [fsearch](https://github.com/noahdunnagan/fsearch). It also runs as an MCP server, so coding agents get the same index instead of Spotlight or `find`.

## What we never compromise on

1. **Speed.** A name search answers in about a millisecond, and the UI should never be what makes it feel slower. Nothing blocks the UI thread on disk I/O: listing a folder or running a search happens on the background executor. Big folders use `uniform_list`. No continuously repainting animations; they peg the GPU on high-refresh displays.
2. **Agents are users too.** Anything a person can do in the window, an agent should be able to do over MCP. That only works when the behavior lives in a `monodrive-core` service and both the view and the MCP tool call it.
3. **The disk is the user's.** Monodrive reads by default. Anything that moves, renames or deletes files must be explicit, reversible where macOS allows it (move to Trash, not unlink), and visible in the UI.

## A note on taste

We like ambitious ideas, simple systems, and software that feels obvious. Don't keep complexity because it already exists, and don't add machinery because it looks architecturally impressive. Find the real constraint, then build the smallest model that makes the correct behavior unsurprising. Measure twice, cut once, and fight scope creep.

These are good defaults, not hard rules. If one fights the task in front of you, say so and get a sign-off before breaking it.

## Glossary

- **you** means the agent reading this file.
- **user** means the person using Monodrive.
- **agent** means a coding agent that reaches Monodrive over MCP. Depending on context, that may include you.
- **index** means fsearch's whole-disk name and content index.
- **entry** means one file, folder or link, as `monodrive_core::entries::Entry`.

## Ways to hurt yourself

1. **Killing by pattern.** Never `pkill -f`, `killall monodrive`, or kill a PID you found by matching a name or path. The user may be running Monodrive, Monocode or fsearch while you work. Kill only a PID you captured when you started the process.
2. **Damaging the live index.** `~/Library/Application Support/FSearch` is shared by Monodrive, `monodrive mcp` and the fsearch CLI. Never delete it, edit its files, or point a test at it. The first process to start owns the index and the rest follow, so a stray debug build can become the owner. Stop what you started.
3. **Crawling the disk by accident.** Starting the index on a machine without one crawls the whole disk: minutes of CPU and several hundred MB of disk and memory. The app starts it at launch, as Spacedrive does, so every window you open loads or builds it; run the app only when you need it, and stop it by PID. Tests must never call `Index::start`, and the MCP server starts it on its first search, since each agent session runs its own server.
4. **Editing `vendor/`.** `vendor/gpui-pre*` is Monocode's patched GPUI. Sync it from Monocode when bumping GPUI; don't fix things in place.

## Hit every surface

The most common defect is a change that works on the path you tested and is missing everywhere else. Before calling work done, walk this list and say which entries applied:

- **UI and MCP.** A new capability is a `monodrive-core` function. The view and the MCP tool both call it; neither carries the logic. Changing a tool's name, description or schema changes how agents use it, so keep descriptions short and concrete.
- **Entry points.** Something reachable by mouse usually needs a keybinding, and later a menu item and the command palette.
- **Reverse states.** If you add a way in, add the way out and a way to see it. Search needs a way to clear it; going into a folder needs Back.
- **macOS only.** Monodrive targets macOS alone for now and will never support Windows, so use AppKit and fsearch directly without `cfg` gates or fallbacks for other platforms. Match macOS conventions: ⌘ shortcuts, Finder's sizes and sort order, Trash instead of delete.
- **Permissions.** Without Full Disk Access, fsearch skips consent-gated folders instead of prompting. Features must degrade the same way, never block on a privacy prompt.

## Where code lives

- `crates/monodrive-core` holds the services and has no GPUI. `entries` lists folders, `index` wraps fsearch, `places` and `volumes` are the sidebar's lists and drives, `servers` mounts shares through NetFS, `mcp` is the MCP server, and `ops` changes files through the one owner process the window and every MCP server share (see `docs/operations.md`).
- `crates/monodrive` is the GPUI app; `main.rs` decodes the command line and `lib.rs` opens the window, so benchmarks can build its views. `commands.rs` is the one table of commands that the keymap, menu bar, command palette (`palette.rs`) and Keyboard settings read; add a command there, not a key binding in a view. `browser.rs` is the window, `browser/sidebar.rs` its sidebar and the `Drives` it reaches the system through, `browser/trash.rs` the Trash's bar and Recents, `connect.rs` Connect to Server, `session.rs` saves what each window has open to `windows.json` in the support folder, `settings.rs` is the Settings sheet, `agents.rs` adds `monodrive mcp` to coding agents' configs for its Agents page, `preferences.rs` holds what it edits and saves it to `~/Library/Application Support/Monodrive/settings.json`, `text_field.rs` is the single-line input (cut down from Monocode's `input.rs`), `theme.rs` turns preferences into Monocode's appearance tokens and holds the shared controls and scales, `palettes.rs` holds Monocode's color presets, `background.rs` bakes and paints the background image, `file_icons.rs` draws the icons Finder shows, through `NSWorkspace`, at the size drawn, `thumbnails.rs` decodes image thumbnails for the grid, `goto.rs` is Go to Folder, `jobs.rs` is the title bar's ring of copies and moves and its popover, `empty_trash.rs` asks before Empty Trash, `menu.rs` is the context menu (ported from Monocode's `popover.rs`), which shows the commands whose `scopes` match and that would run where it opened, `workspace.rs` hands files to Finder and Terminal through `NSWorkspace`, `frame.rs` sizes and places the window, `traffic_lights.rs` pins the macOS window buttons, and `profile.rs` is the `profile` feature's launch and frame report.
- `assets/` holds the app icon: its Icon Composer project and the PNGs `scripts/icons` exports from it. See `assets/README.md`.
- `vendor/` is the patched GPUI from Monocode (see rule 4).

## Design

Read `DESIGN.md` before any UI work. It sets the shapes, sizes, type, colors and states, and lists the shared controls in `theme.rs` that views use.

## Reusing Monocode

The look comes from Monocode's GPUI app, `crates/monocode-gpui` on the `mc/migrate-monocode-to-gpui` branch. `vendor/` was synced from that branch at 7a6ea059. Prefer porting its components (`popover.rs`, `modal.rs`, the settings palettes) over writing new ones, strip their Monocode-specific dependencies when you do, and rebuild them on `theme.rs`'s controls. Keep its conventions where `DESIGN.md` doesn't say otherwise: content ink at varying alpha (`theme::content`) instead of fixed greys, 32 pt sidebar rows, 24 pt chrome buttons, and the 40 pt title bar. Monocode's radii don't carry over.

## Running

- `cargo run -p monodrive` opens the window on your home folder; `cargo run -p monodrive -- <folder>` opens another.
- `scripts/dev [folder]` rebuilds the app and relaunches it on the same folder whenever a source file changes, through watchexec. A failed build leaves the last app running, and Ctrl-C stops both. It relaunches into the foreground, so it's for a person at the keyboard, not for agents. `mise.toml` pins the watchexec it needs; run `mise install` in the repo.
- `scripts/bundle` builds `target/debug/Monodrive.app` with the app icon, so the Dock and Finder show it. Run `target/debug/Monodrive.app/Contents/MacOS/monodrive` to launch it and keep its PID.
- `cargo run -p monodrive -- mcp` runs the MCP server on stdio. To try it with an agent, build first and register the binary, for example `claude mcp add monodrive -- <repo>/target/debug/monodrive mcp`.
- Opening the window starts the index. See rule 3 before running the app on a machine without one.

## Verifying

- `scripts/check` runs what CI runs: the design check, format, clippy with warnings denied, and tests. Run it before you push.
- While iterating, run `cargo test -p <crate> <name>`.
- Test behavior that can break: MCP protocol handling, argument decoding, path handling, listing and sorting rules, and the logic a bug fix changed. Skip tests for what types already guarantee, layout, copy text and wiring.
- Extend an existing test before adding one. Test at the lowest layer that fails without the change.
- A bug fix's test must fail before the fix, for the reason the bug describes. Purely visual fixes get before and after screenshots instead of tests.
- View behavior gets a `#[gpui::test]` that drives the real `Browser` with keystrokes over a temp folder; `browse` in `browser.rs`'s tests sets one up. Press navigation keys only: a printable key starts a search, and a search starts the index.
- No sleeps, polling loops or wall-clock assertions in tests.
- `clippy.toml` enforces what it can of the rules above: no sleeps, no deleting files, and `Index::start` only where an `#[expect]` says why. CI also runs cargo-deny (`deny.toml`), so a new dependency's license must be on its allowlist.
- Speed has budgets in `docs/performance.md`. Run the benchmarks it lists when a change touches listing, sorting, the browser's rendering or launch, and compare against the last run.
- Visual changes get a look. `cargo run -p monodrive --example snapshot -- [folder]` draws every surface, dark and light, to `target/snapshots/` through GPUI's headless renderer: no window, no screen capture, no index. Run it before and after a change. Ask before using computer use or screen capture of the real app.

## Taste

- Complexity belongs at the boundary. fsearch's quirks stay in `index.rs`, AppKit's in `traffic_lights.rs`; views stay simple.
- MCP tools decode their arguments into a typed enum, call one service, and format a terse text reply. Agents read paths, so print paths, one per line.
- Prefer inferred types. No `any`-style escape hatches: no `serde_json::Value` where a struct fits, no unnecessary casts, no `unwrap` on user or disk input.
- Default to no comment. Write one for a constraint, an external quirk or an invariant the code can't show, in a line or two.
- Errors from fsearch arrive as strings. Pass them through to the user or agent instead of hiding them.

## Pull requests

- Never open a PR unless the maintainer asks.
- Conventional commit titles in plain language: `feat(mcp): add a stat tool`.
- The body states the problem in a sentence or two, then how you fixed it, then how you verified it.
- UI changes need before and after images; motion needs a short video. Upload PR evidence to GitHub; never commit it.
- One request is one PR.

## Documentation

Most changes need no documentation. Agents can read the code. Add a doc only for a decision or constraint that spans components and that the code can't show, and rewrite it when the decision changes instead of appending to it. Don't commit plans, research notes or agent scratch files.
