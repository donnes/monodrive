# Offline catalogs

Depends on stable source identities and saved search. Begin with explicitly selected removable drives or folders, not every source automatically.

## Scope

Let a user opt into retaining a searchable catalog of names, paths, sizes and timestamps when a source disconnects. Store it in Monodrive's own data location; do not edit fsearch's shared index files. First investigate whether fsearch provides the needed retention API before adding another live index. Keep any separate catalog limited to offline metadata requirements.

Each catalog records its source identity, scope, last successful refresh and incomplete/error state. Offline results are clearly stale observations. Opening a result identifies the source to reconnect; once connected, resolve and validate the current entry before opening or operating on it.

Refreshing is explicit or a separately enabled policy. Use bounded background work with cancellation. A disconnected source does not appear empty, and a failed scan must not erase the last good catalog. Forgetting a catalog removes only Monodrive metadata and previews, never source files. Expose storage usage and removal controls.

The initial catalog stores metadata, not copies of file contents or a backup. No automatic content hashing, OCR or peer synchronization. UI and MCP can create, inspect, refresh, search and forget the same catalogs.

## Verification

Use synthetic source identities and temporary trees for disconnect/reconnect, remount path changes, failed refresh and stale-result revalidation. Test bounded retention and cancellation without the live index or real external drives. Measure catalog growth and refresh cost on generated datasets.
