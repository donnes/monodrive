# Places and volumes

The sidebar's favorites, the recently opened files and the mounted volumes come from `monodrive-core`'s `places` and `volumes`. The window and every `monodrive mcp` server read and change them the same way.

## Where they're saved

Favorites are in `places.json` and recents in `recents.json`, both in Monodrive's support folder. They stay out of `settings.json` because only the window writes that file, from its copy in memory, and an agent's pin written there would be lost at the window's next save. Each change reads the file again, changes it and renames a new one into place. A `places.json` that doesn't parse is reported and never overwritten. Until the first change there's no list on disk, and the defaults apply: Home, Desktop, Documents, Downloads and Applications. Saved servers for Connect to Server will go in `places.json` next to the favorites.

## Volume identity

A volume is known by its UUID, or a network share by its remount address (`smb://nas/photos`), since macOS gives shares no lasting UUID. A favorite on any volume but the startup disk keeps that identity and the path it was mounted at. It resolves on whichever mounted volume has the same identity, under that volume's current path. When none has it, the favorite is unavailable, even if another volume of the same name is mounted where it used to be. A favorite on the startup disk keeps no identity, because a reinstall changes the UUID and the startup disk is always there. A volume macOS gives no identity, such as some FAT drives, can only be matched by path.

The list comes from the kernel's mount table, without the volumes the system marks hidden (`MNT_DONTBROWSE`), which are the ones Finder hides too. `NSFileManager`'s own list lags a mount by a moment, so a listing made when the mount event arrives would miss the new volume. The watch hears mounts through kqueue's file system events, with no polling.

## Eject

Eject refuses the startup disk, and any volume that a queued, running or waiting copy or move reads or writes. A waiting job counts because continuing it later needs the volume. Agents can eject only while the Agents setting lets them change files. Eject unmounts the whole disk when its media can be ejected or unplugged, and only the volume otherwise. It never forces, and it passes on macOS's refusal, naming the process that kept the volume busy when macOS says which.
