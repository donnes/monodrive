# Offline catalogs

`monodrive-core`'s `catalogs` keeps the names, kinds, sizes and modification times under a source the user or an agent picks, so a search finds them while the source is disconnected. A catalog holds no file contents and no hashes. Nothing is cataloged or refreshed unless someone asks.

## Why not fsearch

fsearch can't keep a volume's entries once it's disconnected. Its crawl stops at every mount point, so external drives and shares are never in the index, and /Volumes is one of its consent-gated folders. An `Engine` indexes the whole disk into one shared folder with one owner, and the folders it skips are set once per process, so it can't run a second engine per volume either. Its `Index` builder takes only listings from fsearch's own crawl, and the file format changes with fsearch. Catalogs use fsearch's matcher instead: `fsearch::Query::match_path` tests each cataloged path, so a catalog search takes the same words, globs and filters as `find_files` and ranks them the same way.

## Sources

A source is a volume, or a folder on one, known by the volume's identity (see [places](places.md#volume-identity)) and the folder's path inside it. A catalog's id is a hash of the two, so cataloging the same source again by any path refreshes the same catalog. The startup disk is refused, since it's always connected, and so is a volume macOS gives no identity, since a catalog couldn't tell it from another drive. Hidden volumes count, such as a disk image attached with `-nobrowse`. Without Full Disk Access the walk never opens /Volumes, so cataloging a volume mounted there fails with "needs Full Disk Access".

## The file

Each catalog is one file, `catalogs/<id>.catalog` in Monodrive's support folder. It holds an 8-byte magic that carries the format version, the body's length, the body, and then a JSON header with the source, the last good refresh, the entry count, what's missing and the last failure. Listing catalogs reads only headers. The draft is synced before it's renamed over the old file, and the folder after, so a crash leaves one whole catalog. A header over 64 KiB, a body past the file's end, a path longer than `PATH_MAX`, or a body holding another number of entries than its header says makes the catalog unreadable: listing and searching name it and leave it out, and forget removes it.

The body is deflate over one record per entry, in the walk's order. A record holds how many bytes of the previous entry's path it shares, the rest of its path inside the source, its kind as a byte, and its size and modification second, as varints. A folder's items come together, so the shared prefix covers most of each path, and deflate compresses most of the rest. A search inflates each body once and tests every path, so the file needs no index or sorted order.

On an APFS disk image with a release build, 100,818 files and folders with random names make a 2.4 MB catalog, about 24 bytes an entry, and 101,100 with repetitive generated names make 41 KB. Real names fall between the two. A refresh of either takes 0.5 to 0.65 s warm, and 1.3 s for the first walk after mounting. A search takes 20 to 30 ms per 100,000 entries, about twice that when nothing matches and typos are tried.

## Refresh

A refresh uses the walk that folder sizes and storage scans share (`sizes::walk`). It never follows links, downloads placeholders or lists a placeholder folder, and it doesn't open consent-gated folders without Full Disk Access. A hard-linked file is cataloged only at the first path the walk meets. The body goes to a draft file as the walk meets entries, holding only each folder's path, and the draft is renamed into place when the walk finishes.

The last good catalog stays when a refresh is cancelled, runs out of time, can't list the source, or finds it disconnected. A source that's unplugged during the walk leaves the walk skipping what vanished and failing to list what's left, so a refresh compares the source folder's device and inode before and after, and keeps nothing when they differ. A source folder that's become a link, or sits on another volume than its own, is treated as disconnected too, and resolving an entry refuses one under a folder that's become a link or one on another volume. The draft is removed, and the header records why. A refresh that meets 2,000,000 entries stops there and keeps them, marked incomplete, as it does for folders it couldn't read. A first catalog that doesn't finish keeps nothing. A flock on `<id>.lock` lets one refresh of a catalog run at a time across the window and every MCP server. Lock files stay after forget, empty, since removing one would let a refresh holding it run beside one that locks a new file.

Refreshing is explicit. An automatic policy would hear mounts through `volumes::watch`, ask `Source::locate` whether a cataloged source came back, and call `Store::refresh` off the UI thread.

## Search and resolving

`Store::search` runs a `search::Query` over every catalog, or only those its `in:` overlaps. A hit's path is under where its source is mounted now, or under where it was last seen while it's disconnected. Each catalog in the results carries its refresh time and whether its source is connected. `Catalog::resolve` checks a hit against the connected source: when the entry is the same kind, and for anything but a folder the same size, it returns the current path; otherwise it says what changed or that the entry is gone. Open or operate on a catalog entry only after resolving it.

## Agents

`catalog_create`, `catalog_list`, `catalog_refresh`, `catalog_forget` and `catalog_search` call the same services. The Agents setting doesn't gate them. It covers changing the user's files, and these tools only read the source and write Monodrive's own folder, as `pin` and `save_search` do. Forgetting removes only the catalog file.
