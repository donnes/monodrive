# Places and volumes

The sidebar's favorites, the recently opened files and the mounted volumes come from `monodrive-core`'s `places` and `volumes`. The window and every `monodrive mcp` server read and change them the same way.

## Where they're saved

Favorites are in `places.json` and recents in `recents.json`, both in Monodrive's support folder. They stay out of `settings.json` because only the window writes that file, from its copy in memory, and an agent's pin written there would be lost at the window's next save. Each change holds a flock on `places.lock` or `recents.lock` while it reads the file, changes it and renames a new one into place, so concurrent writers never lose each other's changes. A file that doesn't parse is reported and never overwritten; clearing recents is the one change that replaces it. Until the first change there's no list on disk, and the defaults apply: Home, Desktop, Documents, Downloads and Applications. Saved searches are in `places.json` too, with a name, the query as `search::Query` prints it, and a scope; saving one never runs it. Registered Git repositories are in `places.json` too, each its common Git folder and the path shown for it (see [worktrees](worktrees.md)). Remote sources are in `places.json` too, as [remote](remote.md) describes, and so are Connect to Server's saved addresses, never with a password.

## Volume identity

A volume is known by its UUID, or a network share by its remount address (`smb://nas/photos`), since macOS gives shares no lasting UUID. Pinning records the folder's real path, past any links, so a link to a folder on an external drive pins that drive. A favorite on any volume but the startup disk keeps that identity and the path it was mounted at. It resolves on whichever mounted volume has the same identity, under that volume's current path. When none has it, the favorite is unavailable, even if another volume of the same name is mounted where it used to be. A favorite on the startup disk keeps no identity, because a reinstall changes the UUID and the startup disk is always there. A volume macOS gives no identity, such as some FAT drives, can only be matched by path. A favorite that's unavailable stays in the sidebar, dimmed, with the reason, and opening it says that reason; it can still be moved and forgotten.

The list comes from the kernel's mount table, without the volumes the system marks hidden (`MNT_DONTBROWSE`), which are the ones Finder hides too. `NSFileManager`'s own list lags a mount by a moment, so a listing made when the mount event arrives would miss the new volume. The watch hears mounts through kqueue's file system events, with no polling.

## Eject

Eject refuses the startup disk, and any volume that a queued, running or waiting copy or move reads or writes. A waiting job counts because continuing it later needs the volume. Ejecting a whole disk unmounts every volume on it, so the checks cover each mounted volume on the same physical disk: the outermost storage medium above the volume in IOKit's registry, past APFS containers and partitions. Agents can eject only while the Agents setting lets them change files. Eject unmounts the whole disk when its media can be ejected or unplugged, and only the volume otherwise. It never forces, and it passes on macOS's refusal, naming the process that kept the volume busy when macOS says which.

## Connect to Server

`servers` mounts `smb://` shares through macOS's NetFS with no user or password, so macOS finds credentials in the Keychain or asks on screen. An address with a password in it is refused before anything happens, and an address is saved only once it has connected. The window allows the prompt. An MCP server asks NetFS for no UI, since nobody can answer one there, and says so when credentials are needed. A mount in flight is cancelled whenever its card goes away, however it goes, and a cancelled attempt saves nothing. No refusal repeats the typed address, and a remount address loses any password when it's read. Connecting only mounts: the share shows in the sidebar, nothing lists it until someone opens it, and nothing crawls it. Disconnecting is Eject, with its refusals. Forgetting an address leaves a mounted share mounted.

## In the window

The sidebar reads all of this off the UI thread and reads it again on events only: `volumes::watch` for mounts, `Store::watch` for `places.json` and `recents.json`, a finished job, Empty Trash, and the window becoming active, which is what keeps the capacity bars current without polling. `Drives` is the one way views reach the system, so their tests use a made-up list of volumes and never mount or eject anything real.

Recents is a stop in the Back and Forward history, in front of the folder that was on screen. It lists what `Store::recents` holds, newest first. A file that's gone stays in the list, marked, until Clear Recents; opening it says it's gone.
