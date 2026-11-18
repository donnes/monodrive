# Search

Typing in the window searches every file name on the disk through the index and lists the best matches; Escape returns to the folder. Agents use `find_files` for names and `grep` for contents.

## Sub-features

- `search-window` searches names from `cmd+f` or any letter typed in the list; the tab becomes `Search results`.
- `search-fresh` finds a file created while the window runs.
- `search-clear` clears the query with `escape` and restores the folder.
- `search-mcp-names` finds names over MCP, with `in:<absolute folder>` to narrow.
- `search-mcp-contents` finds text inside files over MCP with `grep`.

## How to get to it (user POV)

- `cmd+f`, or typing a letter with the list focused.
- The palette: `Search`, `Search File Contents` (`cmd+alt+f`), `Search This Folder`, `Searches…`.
- MCP: `find_files`, `grep`, `index_status`.

## Driving it with agent-desktop and verify

Preconditions:

- Baseline state. `doctor` passes; the window's index loaded from the clone (`grep 'replay done' target/agent/app.log`).

- **Search.** Run `agent-desktop press "${A[@]}" cmd+f`, then `for k in m d v e r i f y; do agent-desktop press "${A[@]}" $k; done`, then wait for `group:mdverify-needle.txt` (up to 20 s on the first search). The `Search` field's value is `mdverify`, the tab list is `Search results`, and `mdverify-needle.txt` is the first row, selected.
- **Fresh file.** Run `printf x > "$F/mdverify-fresh.txt"`, then search `mdverify-fresh` the same way. The row appears.
- **Clear.** Run `agent-desktop press "${A[@]}" escape`, then wait for `group:Alpha.txt`. The field is empty and the tab is `fixture` again.
- **Names over MCP.** Run `$V mcp find_files "{\"query\":\"mdverify-needle in:$F\"}" | jq -r '.result.content[0].text'`. One line, the fixture's file.
- **Contents over MCP.** Run `$V mcp grep "{\"pattern\":\"verify-needle-7f3a\",\"path\":\"$F\"}" | jq -r '.result.content[0].text'`. `…/Projects/Monodrive/notes.md:2: verify-needle-7f3a`.
- **Proof.** `$V capture search 1-results` with the query showing, `2-cleared` after Escape; keep the MCP reply lines.

## Gotchas

- Window search is whole-disk: common names (`notes`) bury the fixture among the user's files, and rows show names only. Search the unique `mdverify` names.
- `in:~/...` resolves `~` to the throwaway home. Use absolute paths.
- A one-shot `verify mcp` search sees the index as of the window's last save, so a file made after `verify up` isn't found over MCP even when the window finds it (seen 2026-11-18, also with a session held open 10 s). Use files the fixture creates for MCP search proofs, and report it if a change claims MCP search is live.
- Every search starts the index. That's fine in the throwaway home, which has a clone; never search from a build pointed at the user's live index.
