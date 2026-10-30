# Storage inspection

`storage::scan` adds up a folder the user or agent picks, and `Report::duplicates` finds identical files in it. Neither changes the disk, and neither runs unless asked: opening a folder never starts a scan.

## What the sizes mean

A scan walks the folder with the same walk as folder sizes (`sizes::walk`): links are never followed, and a hard-linked file counts once, in the first folder it's met in. Each file has three sizes.

- **Bytes** is its length, as Finder's Size.
- **On disk** is the blocks it takes. A compressed or sparse file takes less than its length.
- **Unshared** is APFS's `ATTR_CMNEXT_PRIVATESIZE`: the part of on disk that no clone and no snapshot shares, which is what trashing that file alone frees once the Trash is emptied. A file and its clone are each 0, and so is every file a Time Machine local snapshot still holds, until the snapshot goes. Volumes that don't report it, such as HFS+ or exFAT, have nothing to share, so unshared is all of on disk there.

Unshared adds up per file, so a folder's total is a floor: two clones inside one folder are 0 each, but trashing both frees their blocks. Cleanup reports what it frees as "at least" for the same reason. It costs one `getattrlist` per file, beside the `lstat` the walk already makes.

## Bounds

A scan stops after 500,000 files and folders, at its deadline, or when cancelled, and says which. It keeps about 100 bytes a file, so a full scan holds about 50 MB. What it couldn't read is listed with macOS's reason and left out of the totals.

Opening a consent-gated folder without Full Disk Access shows a privacy prompt and blocks until it's answered, which no deadline interrupts. So when the process lacks it, the walk shared by scans, folder sizes and comparisons never opens fsearch's gated folders (`fsearch::gated`: Desktop, Documents, Downloads, the containers and cloud folders in the home folder, and /Volumes). It reports each as unreadable, "needs Full Disk Access", and refuses one given as the folder to scan or compare, resolving a link given there first.

`Progress` carries the cancel flag and the counts, and calls its `notify` on the scanning thread every 4,096 items or 64 MB read, so a view redraws when told rather than polling.

## Duplicates

Only files whose sizes match are candidates. Two readers then hash each candidate's first, middle and last 64 KiB with BLAKE3, which splits most false candidates without reading them whole. The rest are read whole, two at a time with a 256 KiB buffer each, and a group is verified only when every member's full BLAKE3 hash matches and each file's identity, length and modification time were the same before and after it was read. A file no larger than its three samples is read whole the first time. Hard links to one file are one file, never its own duplicates; clones are duplicates whose trashing frees nothing, and their unshared size shows it.

Stopping keeps each group at the last stage it finished for every member: same size, or same samples. Only verified groups can be cleaned up. A cloud placeholder is never read, since reading it would download it; one the size of another file is listed as not downloaded instead (see [cloud folders](cloud.md)).

## Cleanup

The person or agent names the exact copies to trash. `Duplicates::cleanup` refuses a copy that isn't in a verified group and a plan that would trash every copy of a group, then carries each copy's identity and stamp as verified. The operation owner checks them again (see [operations](operations.md#duplicate-cleanup)). There are no automatic rules: nothing is chosen by age, folder or name.
