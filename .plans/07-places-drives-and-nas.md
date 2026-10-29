# Places, external drives and NAS

Depends on file operations and context-menu commands. Finder-style mounted volumes are the first slice; connecting SMB shares is the second.

## Favorites and source identity

Pin, reorder and remove favorite folders. Support a command before drag and drop exists; add drag-to-pin in the drag milestone. Forgetting a place removes only Monodrive's bookmark.

List mounted volumes with capacity, read-only state and Eject. Observe mount/unmount notifications and retain a stable volume identity when the OS provides one, not only its display name or `/Volumes` path. Remounting under another path must not silently redirect a saved place to a different drive.

Expose source identity, availability and supported operations in core services. Use a small model driven by local volumes and SMB first; defer a universal provider framework until the remote milestone needs it.

## Connect to Server

Add saved SMB locations, explicit connect/reconnect, disconnect and forget. Use macOS mounting and credential facilities; never store passwords in preferences or MCP output. Show connection and permission failures. Listing a mounted share and establishing a connection are separate operations.

Eject/disconnect must account for Monodrive's active jobs and report OS refusal. Do not force-unmount. Disconnecting during a transfer leaves the job interrupted with recoverable outcomes. Reconnecting does not restart writes without validating source and destination.

## Trash and recents

Show Trash with Put Back and explicit Empty Trash through the operation service. Availability follows the volume's actual Trash behavior. Do not promise Put Back for arbitrary Trash contents without original-location metadata.

Show recently opened files using known open history; filesystem access time alone is not a trustworthy user-open history. Keep this bounded and define clearing it. Neither recents nor mounting triggers a whole-disk scan.

## Surfaces and verification

The sidebar, commands and MCP expose source inventory, capacity, availability and supported connect/disconnect actions. Keep raw credentials out of tool arguments; use an appropriate native credential flow when needed. Mount work and capacity reads run in the background.

Test remount identity, missing/read-only sources, forget semantics and interrupted jobs with controlled notifications and fixtures. Use an explicitly chosen test share for integration checks, never unrelated live mounts. Verify sidebar states with headless snapshots.
