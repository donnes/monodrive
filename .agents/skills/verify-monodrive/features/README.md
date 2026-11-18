# Monodrive verification map

The maintained source for proving what users and agents can do in Monodrive. Read this index, then the feature's file, and drive from it. `../SKILL.md` explains `verify`, the tree's roles and the evidence rules; this map assumes them.

## Baseline preconditions

- `verify up` started the window on the fresh fixture, and `verify doctor` prints only `ok`.
- `NAME=$(.agents/skills/verify-monodrive/verify name); A=(--app "$NAME"); V=.agents/skills/verify-monodrive/verify; F=$PWD/.scratch/verify/fixture`, run from the repo root.
- The window shows the fixture with nothing selected and no search: `agent-desktop snapshot "${A[@]}" -i --compact --wait-for "group:Alpha.txt"`.
- Never drive an instance this run didn't start. `doctor` names the one you own.

## Driving conventions

- Drive the window by keys through `agent-desktop press "${A[@]}"`; reach context-menu commands through the palette (`cmd+k`).
- Call MCP tools through `$V mcp <tool> '<json>'` with absolute paths.
- After every action, wait for the visible result with `--wait-for` / `--wait-for-gone` before reading state.
- Check the disk with `ls "$F"` (and `ls ~/.Trash`) after every change, besides the window's rows.
- Put back anything you moved to the Trash before the next recipe. Don't undo a creation to clean up: that moves it to the user's real Trash. To start the next recipe from a clean fixture, `verify down` and `verify up`.

## Proof and skip reporting

- Capture the starting state and the result with `$V capture <run> <label>`; for MCP, keep the reply line.
- Name the feature's sub-feature ID and the entry point with each artifact.
- Report a path you couldn't reach with the command you tried and the precondition that failed. Don't report it verified through another entry point.

## Feature entry contract

Each feature file has an H1, one paragraph on what the user sees, then four H2s in order: `Sub-features` (IDs, one line each), `How to get to it (user POV)` (every entry point), `Driving it with agent-desktop and verify` (starts with `Preconditions:`, then labeled bullets pairing an action with exact commands and the observable result), and `Gotchas`. Keep implementation out; name only user paths, stable handles, state and proof.

## Features

- [Browse folders](./browse.md): list, selection, opening folders, Back and Forward, the path bar, hidden files, over MCP too.
- [New folder and rename](./new-folder-rename.md): create, name, rename, undo and redo, from the window and MCP.
- [Move to Trash and back](./trash.md): trash, undo, Put Back, from the window and MCP, without leaving fixtures in the real Trash.
- [Search](./search.md): whole-disk name search, clearing it, content search, from the window and MCP.
- [Command palette](./palette.md): opening, filtering and running commands, the path to everything a context menu offers.
