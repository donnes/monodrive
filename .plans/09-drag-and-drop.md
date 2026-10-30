# Drag and drop

Depends on operations, places and split panes. Add sidebar pinning here after its command-based version exists.

## Foundation

- GPUI's drag and drop handles drags inside the window. Dragging to and from other apps needs AppKit pasteboard drags with file URLs. Find out how far the vendored GPUI goes before writing a bridge.
- Drops go through the file operations service, so they get undo and progress.

## Behavior, as in Finder

- Dragging within a volume moves, and across volumes copies. ⌥ forces a copy and ⌘ forces a move. The cursor badge shows which.
- Drop targets: folders in the listing, sidebar places, tree rows, the path bar, and other apps.
- Dragging into an app, such as an editor or a chat window, hands it the files.
- Spring-loaded folders, as the maintainer asked: resting about 1 second on a folder, tab, favorite or drive opens it, Space opens it at once, and Esc returns to where the drag began. "Open folders while dragging" in Settings turns it off.

Remote sources later reuse these semantics only where supported. Dragging a remote item into another app requires an explicit local download/materialization step; never expose a remote path as a local file URL.
