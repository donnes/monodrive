# Context menu

A right-click or ⌃-click menu built from the command table, so it never disagrees with the menu bar or the palette.

## Foundation

- Commands gain a "where it applies" field: on a selection, on an empty folder area, or on a sidebar item. The context menu shows the commands that apply, in their groups, with their shortcuts.
- A popover menu component, ported from Monocode's `popover.rs`, opened at the pointer and fully keyboard navigable. ⇧F10 and the Menu key open it on the selection.

## Contents

- On a selection: Open, Open With, Quick Look, Rename, Duplicate, Move to Trash, Copy, Copy Path, Get Info, Reveal in Finder, Open in Terminal, and Copy for Agent (paths one per line).
- On an app or another package: Show Package Contents, which goes inside it. Opening one launches it instead once the [packages milestone](10-open-with-and-info.md) is implemented.
- On empty space: New Folder, New File, Paste, View as, Sort by, Show Hidden Files.
- On a sidebar item: Open, Open in New Tab, Remove from Sidebar, Eject.

Right-clicking an unselected item selects it first, as Finder does.

Only expose actions whose services are implemented. Quick Look, Open With, package contents, tabs and Eject arrive with their respective milestones; this milestone does not implement them all.
