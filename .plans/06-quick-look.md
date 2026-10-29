# Quick Look

Space previews the selection, as Finder does. It uses macOS's own Quick Look, which already handles hundreds of file types and plugins.

## Foundation

- An AppKit bridge for `QLPreviewPanel`: Monodrive's window becomes the panel's data source and delegate. That code stays in one AppKit module, like `traffic_lights.rs`.
- The panel follows the selection. Arrow keys move through the listing while it's open, and it pages through a multiple selection.
- Space or Escape closes it.

## Later

- An inline preview pane (⇧⌘P is taken by the palette, so this needs its own shortcut) using `QLPreviewView` for the selected file. It stays off by default.
- MCP doesn't need Quick Look. Agents read files directly.
