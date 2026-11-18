# Move to Trash and back

A user moves files to the Trash and gets them back with Undo or Put Back; nothing is ever deleted outright. Agents use `trash`, `put_back` and `undo`, through the same owner, so either side can undo the other.

## Sub-features

- `trash-window` moves the selection to the Trash with `cmd+backspace`; the row leaves the list.
- `trash-undo` brings it back with `cmd+z`.
- `trash-mcp` trashes over MCP; the reply names where it went in the Trash, and the window's row leaves.
- `trash-putback-mcp` puts a trashed file back to where it came from.
- `trash-undo-mcp` undoes the last change over MCP, whoever made it.

## How to get to it (user POV)

- `cmd+backspace`, or the palette's `Move to Trash`.
- `cmd+z`; the palette's `Put Back` while viewing the Trash.
- MCP: `trash`, `put_back`, `undo`.

## Driving it with agent-desktop and verify

Preconditions:

- Baseline state; `ls ~/.Trash | grep -c file2.txt` is `0`, so the run's file is the only one with that name.

- **Trash from the window.** Select `file2.txt` with `down`, run `agent-desktop press "${A[@]}" cmd+backspace`, then `agent-desktop snapshot "${A[@]}" -i --compact --wait-for-gone "group:file2.txt" --wait-timeout 5000`. `ls "$F"` lacks it; `ls ~/.Trash` has it.
- **Undo.** Run `agent-desktop press "${A[@]}" cmd+z`, then wait for `group:file2.txt`. It's back in `$F` and gone from `~/.Trash`.
- **Trash over MCP.** Run `$V mcp trash "{\"paths\":[\"$F/file2.txt\"]}" | jq -c '.result.content[0].text'`. The reply is `~/.Trash/file2.txt`'s full path; wait for the window's row to be gone (it can take a few seconds).
- **Undo over MCP.** Run `$V mcp undo '{}' | jq -r '.result.content[0].text'`. It starts `Undo Move to Trash` and names the restored path; the window's row comes back.
- **Put Back over MCP.** Trash again, then run `$V mcp put_back "{\"paths\":[\"$HOME/.Trash/file2.txt\"]}"`. The file is in `$F` again.
- **Proof.** `$V capture trash 1-before`, `2-trashed`, `3-restored`, with `ls "$F"` and `ls ~/.Trash | grep file2` beside each.

## Gotchas

- Move to Trash uses the user's real `~/.Trash`. Always restore before `verify down`, and check `ls ~/.Trash` for your fixture names at the end.
- If the user's Trash already holds a file with the fixture's name, the Trash renames yours; read the path from the reply instead of assuming it.
- The window's Trash place lists the throwaway home's empty `.Trash`, so the Trash view, its Put Back and Empty Trash can't be proven here. Say so; don't count the MCP path for them.
- Empty Trash is irreversible. Never run it, from either surface.
