# New folder and rename

A user makes a folder that starts as `untitled folder` with its name field open, types its name, and can undo or redo it; any file can be renamed in place. Agents do the same with `create_folder` and `rename`, and the window can undo what an agent did.

## Sub-features

- `create-window` makes a folder from `cmd+shift+n` with its `Name` field open on `untitled folder`.
- `create-name` commits a typed name with `return`; `escape` keeps the default.
- `rename-window` opens the `Name` field on the selected row with `return` and commits a new name.
- `create-undo` undoes the last name with `cmd+z` and redoes it with `cmd+shift+z`; a second undo moves the new folder to the Trash.
- `create-mcp` makes a folder over MCP, and the window shows it.
- `rename-mcp` renames over MCP, with `dry_run` previewing and `confirm` applying.

## How to get to it (user POV)

- `cmd+shift+n`, or the palette's `New Folder`; `New Folder with Selection` from the palette.
- `return` on a selected row, or the palette's `Rename`.
- `cmd+z`, `cmd+shift+z`, or the palette's `Undo` / `Redo`.
- MCP: `create_folder`, `rename`, `undo`, `redo`.

## Driving it with agent-desktop and verify

Preconditions:

- Baseline state; no `untitled folder` or `renamed` in the fixture.

- **New folder.** Run `agent-desktop press "${A[@]}" cmd+shift+n`, then `agent-desktop snapshot "${A[@]}" -i --compact --wait-for "textfield:Name" --wait-timeout 5000`. A `textfield` `Name` has value `untitled folder`, nested in its row.
- **Name it.** The default name is selected, so typing replaces it. Run `for k in r e n a m e d; do agent-desktop press "${A[@]}" $k; done; agent-desktop press "${A[@]}" return`, then wait for `group:renamed`. `ls "$F"` lists `renamed` and no `untitled folder`.
- **Undo.** Run `agent-desktop press "${A[@]}" cmd+z`, then wait for `group:untitled folder`. `ls "$F"` shows `untitled folder`: undo reverts the name first.
- **Redo.** Run `agent-desktop press "${A[@]}" cmd+shift+z`, then wait for `group:renamed`.
- **Rename a file.** Select `Alpha.txt` (`down` until its row is `selected`), press `return`, wait for `textfield:Name`, type a new name, `return`. `ls "$F"` shows it. Undo with `cmd+z`.
- **MCP create.** Run `$V mcp create_folder "{\"path\":\"$F/Made by MCP\"}" | jq -c '.result.structuredContent.failed'`. Result `0`, and `agent-desktop snapshot "${A[@]}" -i --compact --wait-for "group:Made by MCP"` succeeds.
- **Undo the agent's change from the window.** Press `cmd+z` in the window; wait for `group:Made by MCP` to be gone. `ls ~/.Trash` now holds `Made by MCP`.
- **Redo it.** Press `cmd+shift+z`; wait for `group:Made by MCP`. It's back in `$F` and gone from `~/.Trash`. Never end the recipe between these two steps.
- **MCP rename, dry run.** Run `$V mcp rename "{\"paths\":[\"$F/beta.md\"],\"name\":\"gamma.md\",\"dry_run\":true}" | jq -c '.result.structuredContent'`. A plan with a `token`; `ls "$F"` still has `beta.md`.
- **MCP rename, confirmed.** Run `$V mcp --confirm rename "{\"paths\":[\"$F/beta.md\"],\"name\":\"gamma.md\"}"`. Two reply lines, the plan and the run; `ls "$F"` has `gamma.md`, and the window's row follows (`--wait-for "group:gamma.md"`).
- **Proof.** `$V capture new-folder 1-name-open`, `2-named`, `3-undone`, plus `ls "$F"` after each.

## Gotchas

- After `New Folder`, a `return` commits the name as it is. Don't press `return` to "start" renaming: the field is already open, and the letters you type next start a whole-disk search instead.
- The `Name` field is inside the row; `.data.tree.children[0].children` doesn't list it. Search the whole tree.
- Redo sent before the undo has finished replies `Nothing to redo`. Wait for the undone name to appear, not only for the old one to leave.
- `rename-window` (return on a row) wasn't exercised when this map was written; prove it before relying on the steps.
- Undoing a creation (a new folder from either surface) moves it to the user's real Trash. Redo it before `verify down`.
- A `token` from `verify mcp ... dry_run` can't be confirmed by another `verify mcp` call: each call is its own session. Use `--confirm`.
