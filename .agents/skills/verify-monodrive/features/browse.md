# Browse folders

The window lists a folder's files with Finder's sort order, moves a selection with the arrows, opens folders, and goes Back and Forward with the selection restored; the path bar shows where you are. Agents read the same listing with `list_dir`.

## Sub-features

- `browse-list` lists a folder: folders first, natural name order (`file2.txt` before `file10.txt`), hidden files left out.
- `browse-select` moves the selection with `down` / `up`.
- `browse-open` opens the selected folder with `cmd+down`.
- `browse-back` returns with `cmd+[` and restores the selection; `cmd+]` goes forward again.
- `browse-crumbs` shows one path-bar link per folder, the current one last.
- `browse-hidden` shows hidden files with `cmd+shift+.`.
- `browse-mcp` lists the same folder with the same names and order over MCP.

## How to get to it (user POV)

- Launching on a folder, or clicking a sidebar place (`Home`, `Desktop`, ...).
- Arrow keys, `cmd+down` or `cmd+o` to open, `cmd+[` / `cmd+]`, `cmd+up` for the enclosing folder.
- The toolbar's `Back` and `Forward` buttons, the path bar's links.
- The palette: `Back`, `Forward`, `Enclosing Folder`, `Show Hidden Files`.
- MCP: `list_dir`.

## Driving it with agent-desktop and verify

Preconditions:

- Baseline state, fixture untouched.

- **List.** Read the rows. Run `agent-desktop snapshot "${A[@]}" -i --compact | jq -c '[.data.tree.children[0].children[] | select(.role=="group") | .name]'`. Result: `["Empty folder","Photos","Projects","Alpha.txt","beta.md","file2.txt","file10.txt","mdverify-needle.txt","Projects.zip"]`, no `.hidden-note`.
- **Select.** Press `down` twice. Run `agent-desktop press "${A[@]}" down` twice. `Photos` is the only row whose `states` has `selected`.
- **Open.** Run `agent-desktop press "${A[@]}" cmd+down`, then `agent-desktop snapshot "${A[@]}" -i --compact --wait-for "group:icon.png" --wait-timeout 5000`. The only row is `icon.png`, the last link is `Photos`, the tab is named `Photos`.
- **Back.** Run `agent-desktop press "${A[@]}" 'cmd+['`, then wait for `group:Alpha.txt`. `Photos` is selected again.
- **Hidden files.** Run `agent-desktop press "${A[@]}" 'cmd+shift+.'`, then wait for `group:.hidden-note`. Press it again and wait for it to be gone.
- **MCP.** Run `$V mcp list_dir "{\"path\":\"$F\"}" | jq -r '.result.content[0].text'`. The same names in the same order, folders ending in `/`.
- **Proof.** `$V capture browse 1-start` before selecting, `2-photos` after opening, `3-back` after Back.

## Gotchas

- The sidebar's `Home`, `Desktop` and the other places point into the throwaway home, not the user's. Use them to prove navigation, not contents.
- Clicking rows with refs is untested here; keys are the proven path.
- A letter key in the list starts a search instead of jumping to a name.
