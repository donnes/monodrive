# Editing commands

The user-facing commands on top of the file operations service, all in `commands.rs` so they appear in the menus, the palette and Keyboard settings.

## Finder's shortcuts

| Command | Shortcut |
| --- | --- |
| Rename | ↩ |
| Open | ⌘↓, ⌘O, double-click |
| New Folder | ⇧⌘N |
| New Folder with Selection | ⌃⌘N |
| Duplicate | ⌘D |
| Move to Trash | ⌘⌫ |
| Empty Trash | ⇧⌘⌫ |
| Copy | ⌘C |
| Paste | ⌘V |
| Move Here | ⌥⌘V |
| Undo, Redo | ⌘Z, ⇧⌘Z |
| Select All, Deselect All | ⌘A, ⌥⌘A |
| Get Info | ⌘I |
| Quick Look | Space, ⌘Y |
| Copy Path | ⌥⌘C |

Following Finder means ↩ renames instead of opening, which changes today's binding. → still enters a folder, so keyboard browsing keeps working.

## Rename

- Inline, in the row or grid cell, with the name selected up to the extension, as Finder does.
- ↩ commits and Escape cancels. Clicking away commits.
- An invalid name (`/`, an empty name, or one that already exists) shows the reason inline and keeps the field open.
- Renaming several selected items at once comes in [batch rename](12-batch-rename.md).

## Copy and paste

- ⌘C writes file URLs to the general pasteboard, so pasting into Finder, Mail or a terminal works.
- ⌘V pastes file URLs from any app as a copy. ⌥⌘V moves them.
- Copy Path keeps writing plain text paths for agents and terminals.

## New items

- New Folder creates "untitled folder" and starts renaming it.
- A New File command (no Finder equivalent) creates an empty file and starts renaming it. It's useful for agents' workflows.

The shortcut table is the intended final command set. Wire Get Info and Quick Look when their services arrive. Mutation commands use shared operation IDs and outcomes; Undo and Redo use the same validated history as MCP.
