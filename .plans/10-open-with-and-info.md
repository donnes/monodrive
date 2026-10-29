# Open With, packages and Get Info

Depends on editing commands and Quick Look. Basic system opening already exists; this milestone adds richer handlers and metadata.

## Open With

- An NSWorkspace bridge: the apps that can open a file, the default one, and opening with a chosen app.
- Open With appears in the context menu and the File menu, and changing the default for a file type works too.
- Reveal in Finder, and Open in Terminal using the terminal set in Settings.

## Get Info (⌘I)

- A sheet with the kind, size, where, created, modified and opened dates, the permissions, and an editable name.
- Folder sizes compute on the background executor and cache per path until FSEvents says the folder changed. A list column can show them on demand.
- MCP gets a `stat` tool with the same fields.

## Packages

Opening an app or document package launches its handler instead of entering the bundle. Use native package classification, not a hard-coded extension list. Return renames as specified by editing commands; Open remains double-click, ⌘↓ or ⌘O. Show Package Contents is the explicit way inside, with normal Back behavior. Packages have no ordinary folder disclosure triangle. MCP listing may still explicitly inspect their contents.

## Verification

Core metadata and package handling use fixtures. Folder-size work is cancellable and does not block navigation; permission failures remain visible. Verify the native handler and Quick Look bridges separately from headless layout tests. Later remote sources report unavailable native metadata instead of fabricating it.
