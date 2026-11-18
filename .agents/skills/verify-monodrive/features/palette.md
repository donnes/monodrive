# Command palette

`cmd+k` opens a palette of every command with its shortcut; typing filters it, and `return` runs the selected row. It's the keyboard route to everything the menu bar and context menus offer, and the only way agents reach context-menu commands headlessly.

## Sub-features

- `palette-open` opens the `Command palette` dialog with the `Command` field focused.
- `palette-filter` narrows the rows as you type; the best match is selected.
- `palette-run` runs the selected command with `return` and closes.
- `palette-dismiss` closes with `escape` and changes nothing.

## How to get to it (user POV)

- `cmd+k` or `cmd+shift+p`.

## Driving it with agent-desktop and verify

Preconditions:

- Baseline state.

- **Open.** Run `agent-desktop press "${A[@]}" cmd+k`, then `agent-desktop snapshot "${A[@]}" -i --compact --wait-for "dialog:Command palette" --wait-timeout 3000`. A `textfield` `Command` is present.
- **Filter.** Run `for k in n e w space f o l; do agent-desktop press "${A[@]}" $k; done`. `Command` has value `new fol`; without `-i`, `agent-desktop snapshot "${A[@]}" --compact | jq -c '[.. | objects | select(.role=="statictext" and (.states//[]|index("selected"))) | .name]'` is `["New Folder"]`.
- **Run.** Run `agent-desktop press "${A[@]}" return`, then wait for `dialog:Command palette` to be gone. An `untitled folder` row is selected with its `Name` field open; continue with [new-folder-rename.md](./new-folder-rename.md), or press `escape` to keep the name and leave the folder for `verify down` to remove. Don't undo it: that moves it to the real Trash.
- **Dismiss.** Open it again, press `escape`, and wait for the dialog to be gone. `ls "$F"` is unchanged.
- **Proof.** `$V capture palette 1-filtered` with the dialog open, `2-ran` after `return`.

## Gotchas

- The palette's rows are `statictext`, dropped by `-i`. Read them without `-i`.
- `space` is the key name for a space; `press " "` doesn't send one.
- Commands whose shortcut is empty in `commands.rs` (`Put Back`, `Search This Folder`) are reachable only through the palette or a menu.
