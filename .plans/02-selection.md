# Selection

Status: the shared selection model and list/grid integration exist, including range and marquee selection, counts and sizes. Keep this as the contract for later operations; rename/move continuity remains to be implemented with the operations service.

## Model

- The selection is a set of paths plus an anchor and a cursor. It keeps paths, not indexes, so it survives sorting, live updates and refreshes.
- Entries that disappear from the listing leave the selection. An entry renamed by Monodrive keeps its place in it.
- One type serves list, grid and every later view. Views only map clicks and keys onto it.

## Input, as in Finder

- Click selects one. ⌘-click toggles one. ⇧-click selects a range from the anchor.
- ⇧↑ and ⇧↓ extend in the list, and ⇧ with the arrows extends in the grid.
- ⌘A selects all. ⌥⌘A deselects all.
- Printable typing starts name search, as the current browser does. Do not silently replace it with type-ahead selection.
- Escape clears search first, then the selection.

## Surfaces

- The status line shows "3 of 120 selected, 14 MB".
- MCP: tools take a list of paths. There's no selection state over MCP. An agent names what it acts on.

## Tests

- The selection survives sort changes and quiet refreshes.
- Range selection follows the visible order.
