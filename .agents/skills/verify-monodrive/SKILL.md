---
name: verify-monodrive
description: Drive the real Monodrive app to prove a change works for users and agents. Starts a dev build of the GPUI window beside the user's own copy, drives it through agent-desktop's accessibility tree, calls the same instance over MCP, and captures snapshots and screenshots as evidence. Use after changing anything a user or an agent can see or do in Monodrive, before calling the work done.
---

# Verify Monodrive

Monodrive has two surfaces, and a change usually touches both (AGENTS.md, "UI and MCP"):

- **The window**, a GPUI app. You drive it with [agent-desktop](https://agent-browser.dev/desktop) through the accessibility tree, headless, while the user keeps working.
- **`monodrive mcp`**, the MCP server agents use. You call it over stdio, one tool call per session.

`./verify` wraps both. It runs from any directory in the repo, and every command below uses the path from the repo root. `scripts/agent-app` does the launching underneath; read AGENTS.md's "Ways to hurt yourself" and "Verifying" once before your first run.

## Launch

```sh
.agents/skills/verify-monodrive/verify up          # builds, writes the fixture, starts the window on it
.agents/skills/verify-monodrive/verify up <folder> # or start it on a folder of your own under .scratch/
```

`up` prints the app's name, `Monodrive Agent <worktree>`. Pass that as `--app` to every agent-desktop command; `verify name` prints it again. The first build in a fresh worktree takes several minutes; later ones take seconds. `up` refuses while a window from this worktree is already running: `verify down` first.

The window runs in a throwaway home (`target/agent/home`, linked from `/tmp/mda-<hash>`) with a clone of the saved index, so it never crawls the disk and never touches the user's settings, session or index. It sits past the bottom right of the screen with a sliver showing and never takes focus.

It is ready when `doctor` passes and a snapshot lists the folder's rows. For the default fixture:

```sh
NAME=$(.agents/skills/verify-monodrive/verify name)
agent-desktop snapshot --app "$NAME" -i --compact --wait-for "group:Alpha.txt" --wait-timeout 20000
```

With a folder of your own, wait for one of its names instead.

`up` refuses to start when the app running you lacks Full Disk Access, or when there is no saved index (the user has never opened Monodrive). Report that; don't work around it.

The fixture (`.scratch/verify/fixture`, rebuilt on every `up`):

```
Alpha.txt  beta.md  file2.txt  file10.txt  .hidden-note  mdverify-needle.txt
Empty folder/  Photos/icon.png  Projects/Monodrive/{notes.md,main.rs}  Projects.zip
```

`notes.md` contains `verify-needle-7f3a` for content search. `mdverify-needle.txt` is a name nothing else on the disk shares.

## Doctor

```sh
.agents/skills/verify-monodrive/verify doctor
```

Read-only. It checks Full Disk Access, agent-desktop's Accessibility and Screen Recording grants, that the screen is unlocked, that the window's process is the one this worktree started, that agent-desktop can read its window, that the build is newer than every source file, and that `target/agent/app.log` has no panic. Every line must say `ok`. Run it first, and again whenever a result looks wrong: a stale or unreadable window looks exactly like a bug.

When it fails:

- **Screen locked, or no readable window.** The user locked the screen, switched Space or covered the window's sliver. GPUI stops drawing and agent-desktop reports `ACTION_NOT_SUPPORTED` / `accessible: false`. Wait for the user; never move, focus or raise the window.
- **Source changed after this build.** `verify down`, then `verify up`.
- **Permissions.** `agent-desktop permissions --guide` opens the right pane for the user.

## Drive

### The window

Run `agent-desktop status` and `agent-desktop skills get desktop` once per session. Then:

```sh
NAME=$(.agents/skills/verify-monodrive/verify name); A=(--app "$NAME")
agent-desktop snapshot "${A[@]}" -i --compact            # the whole interactive tree
agent-desktop press "${A[@]}" down                       # keys go to the window without focusing it
agent-desktop snapshot "${A[@]}" -i --compact --wait-for "group:icon.png" --wait-timeout 5000
agent-desktop snapshot "${A[@]}" -i --compact --wait-for-gone "group:file2.txt" --wait-timeout 5000
agent-desktop screenshot "${A[@]}" <path>.png
```

What the tree looks like:

| Thing | Role and name |
|---|---|
| A file or folder row (list, grid) | `group` named by the file name; `states` has `selected` |
| Path bar crumbs | `link` per folder, the current one last |
| Tabs | `tablist` holding a `radiobutton` per tab, named by folder; `Search results` while searching |
| Column headers | `cell`: `Name`, `Date Modified`, `Size`, `Kind` |
| Sidebar places, section toggles | `button`: `Home`, `Desktop`, `Favorites`, `Locations`, `Trash`, `Overview`, `Recents` |
| Toolbar | `button`: `Back`, `Forward`, `Searches`, `Show Sidebar`, `Settings`, `Check for Updates` |
| Search field | `textfield` `Search`, its query in `value` |
| Rename or New Folder name | `textfield` `Name`, nested inside the row, so search the whole tree: `jq '.. \| objects \| select(.role=="textfield")'` |
| Command palette | `dialog` `Command palette` with `textfield` `Command`; its rows are `statictext` and only appear without `-i` |

Rows are direct children of the window's first group: `jq '[.data.tree.children[0].children[] | select(.role=="group") | .name]'`.

Rules that keep a run honest:

- **Keyboard first.** Shortcuts live in `crates/monodrive/src/commands.rs`: `cmd+down` opens, `cmd+[` Back, `cmd+shift+n` New Folder, `cmd+backspace` Move to Trash, `cmd+z` / `cmd+shift+z` Undo and Redo, `cmd+f` Search, `cmd+k` the palette, `escape` clears a search. Quote combos with brackets in zsh: `'cmd+['`.
- **Type with `press`, one key at a time** (`space` for a space). `type` and `set-value` can't reach the text fields.
- **A printable key in the file list starts a whole-disk search**, which starts the index. Send letters only while a text field (`Search`, `Name`, `Command`) is open.
- **Context menus need `--headed`, which takes the user's focus. Use the palette instead:** `cmd+k`, type the command's name, `return`.
- **Wait on the result, never on a clock, and never trust the snapshot right after a press.** File changes reach the window through FSEvents, up to a few seconds later. Use `--wait-for` / `--wait-for-gone`.
- **Never** pass `--headed`, use `focus-window`, `launch --activate`, `close-app`, Open, Open With, Reveal in Finder or Open in Terminal, or the app's Copy (it writes the user's real clipboard).
- **Headless `click` on refs is unproven.** Prefer keys; if you need a click, find the ref with `agent-desktop snapshot` and check the result with a fresh one.

### MCP

```sh
V=.agents/skills/verify-monodrive/verify
$V mcp --list | jq -r '.result.tools[].name'                  # core profile, 29 tools
$V mcp --list --profile all | jq '.result.tools | length'      # 54
$V mcp list_dir "{\"path\":\"$PWD/.scratch/verify/fixture\"}" | jq -r '.result.content[0].text'
$V mcp create_folder "{\"path\":\"$PWD/.scratch/verify/fixture/Made by MCP\"}"
```

Each call starts `target/debug/monodrive mcp`, initializes, makes one `tools/call` and prints that reply as one JSON line: `.result.content[0].text` is what an agent reads, `.result.structuredContent` the typed data, `.result.isError` and `structuredContent.error.code` the failure. The server runs in the window's home, so its owner, journal, settings and index clone are the window's: what an agent changes, the window shows, and the window can undo. It refuses to run without the window, since any other home could crawl the disk.

A dry run's `token` only confirms in the session that made it, so a second `verify mcp` call can't use it. `verify mcp --confirm <tool> '<json>'` sends the dry run and its confirm through one session and prints both replies, plan first:

```sh
$V mcp --confirm rename "{\"paths\":[\"$PWD/.scratch/verify/fixture/beta.md\"],\"name\":\"gamma.md\"}"
```

- Pass absolute paths. `~` means the throwaway home.
- Inputs reject unknown fields; read a tool's `inputSchema` from `--list` instead of guessing argument names.
- To prove a raw protocol detail the helper hides (several calls in one session, notifications), pipe JSON-RPC lines into `HOME=/tmp/mda-<hash> target/debug/monodrive mcp` yourself.

## Evidence

```sh
.agents/skills/verify-monodrive/verify capture <run> <label>   # <label>.json (tree) + <label>.png
```

Evidence goes to `.scratch/verify/evidence/<run>/`, which is gitignored and survives `verify down`. Name the run after the task and the label after the step: `capture rename-undo 2-after-undo`.

A proof:

- **Takes the user's path.** Keys, the palette and the toolbar in the window; tool calls over MCP. Not a test helper, not a hand-edited `settings.json`, not a file created with `mv` standing in for the app's own move.
- **Shows before and after.** Capture the state the action starts from and the state it leaves, not only the end.
- **Checks the disk too.** After a change, `ls` the fixture, or call `stat` / `list_dir` over MCP, and check the real Trash (`ls ~/.Trash`) for anything trashed. The window's rows alone don't prove a file moved.
- **Covers both surfaces** when the change does: drive the window, call the MCP tool, and show each one's change in the other where it applies.
- **Covers every entry point** the feature map lists for the feature: shortcut, palette, toolbar, MCP. Say which you skipped and why; don't count one path as another.
- **Uses `dry_run` honestly.** `rename`, `organize`, `cleanup_duplicates`, `remove_worktree` and `run_command` take `dry_run: true`. Run the dry run alone with `verify mcp` and confirm with `ls` that it changed nothing; then prove the change with `verify mcp --confirm`.
- UI changes in a PR also need before-and-after images (AGENTS.md); `capture` makes them, and `cargo run -p monodrive --example snapshot` renders every surface headlessly when you need dark and light.

## Cleanup

```sh
.agents/skills/verify-monodrive/verify down
```

Stops the window by the PID `scripts/agent-app` saved, removes its bundle, its throwaway home and link, and the fixture. It keeps `.scratch/verify/evidence/` and `target/agent/app.log`. The operation owner (`target/debug/monodrive ops`) exits by itself once its last client is gone and no job is queued or running; don't kill it by name. Let copies, moves and saved commands finish before `down` removes the home they write their journal to. A `cancel_job` reply only means the request landed: wait until `job_status` reports the job stopped.

Before `down`, make sure nothing of the fixture is in the user's real Trash, which `down` won't fish out:

- Put back what you moved to the Trash: `cmd+z` in the window, or `$V mcp put_back "{\"paths\":[\"$HOME/.Trash/<name>\"]}"`.
- Undoing a new folder, new file, copy or duplicate moves it to the Trash too. Redo it (`cmd+shift+z` or `$V mcp redo '{}'`) so it's back in the fixture, where `down` removes it.
- Check with `ls ~/.Trash` for the fixture's names.

Run `down` after every attempt, failed ones too, and say what you left running if you skip it.

## Features

The map in [features/README.md](features/README.md) lists what users can do and how to prove each one. Read it before driving a feature, and update the feature's file when you learn a handle or a trap it lacks.
