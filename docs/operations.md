# File operations

The window and each `monodrive mcp` server are separate processes, and an agent's MCP server exits with its session. Undo has to work across them: the window can undo what an agent did after the agent has gone. So one process owns every file change, and everything else asks it.

## The owner

`monodrive ops` is a hidden subcommand of the same binary. A client that can't connect starts it detached, in its own process group with `/` as its folder, waits for it to print `ready`, and connects. A `starting.lock` flock keeps two clients from starting two owners at once.

- **State.** `~/Library/Application Support/Monodrive/ops/`, mode 0700, holds the socket, the journal and the locks.
- **Ownership.** The owner holds an exclusive flock on `owner.lock` for its whole life. A second owner fails to take it and exits.
- **Authentication.** Clients connect over the Unix socket `owner.sock`. The owner checks each peer's uid with `getpeereid` and drops connections from other users.
- **Protocol.** One JSON line each way: a `Submission` (request id, origin, typed `Request`) and a `Result<Outcome, String>`. An error means nothing changed.
- **Lifetime.** Clients keep their connection open; the window connects on its first undo or redo, an MCP server on its first change. The owner exits when its last client disconnects, or 30 seconds after starting if none connects. It never starts the search index.
- **Serial.** The owner runs one request at a time, to the end. Copy and move jobs, with progress and cancellation, will need workers; their requests and outcomes extend these types.

## The journal

`journal.jsonl` is JSON lines, synced after each line. A crash mid-append leaves a last line without its newline, which startup cuts off before appending; any other line that doesn't read stops the owner with an error instead of being skipped. Each operation writes a `Begin` record before anything on disk changes, naming each item by device and inode with where it is and where it's going. It writes an `End` record with the per-item outcome after the changes are made.

- **Retries.** A client that loses its connection resends the same request id. A request id the journal has finished returns its recorded outcome instead of running again.
- **Crashes.** On startup, an operation with a `Begin` and no `End` is settled from the disk. Each item is looked for, by inode, where it was and where it was going, including the volume's Trash. The owner records what it finds as an interrupted outcome and never repeats a rename or a removal. An interrupted creation or copy can't be told from one the user made, so it's reported and left out of the history.
- **Retention.** Past 1,000 finished operations, startup trims the journal to the newest 500. Undo reaches back no further, and an item emptied from the Trash can't be put back.

## Undo and redo

The history is linear and shared: undo reverses the newest operation still done, whoever made it, and a new operation drops what could be redone. Each item an operation moved toggles between two places: a path, or the Trash. An item an operation created waits in the Trash while that operation is undone, so undoing New Folder or Duplicate trashes the item and redo puts it back.

Before undo or redo changes anything, it checks every item: the same inode is where the operation left it, nothing else occupies the place it's going, and the item hasn't been modified since Monodrive last moved or made it. A folder's stamp covers everything inside it (entry count, total size, latest mtime), so moving a large folder walks it. Any mismatch refuses the whole step with the path and reason. Renames use `renamex_np` with `RENAME_EXCL`, so a change made between the check and the rename still can't be overwritten.

## Trash

Items go to the Trash through `NSFileManager trashItemAtURL:resultingItemURL:`, and the returned location is recorded for put back. A volume without a Trash fails the item; nothing falls back to permanent deletion. Without Full Disk Access, macOS can refuse to read the Trash, and the error passes through to the user or agent.

## Agents

Agent requests carry `Origin::Agent` with the name the MCP client gave at `initialize`, if any. The person behind an agent isn't known, so it isn't recorded. The owner reads `agents_change_files` from `settings.json` at each agent request and refuses every agent change, undo and redo included, while it's off. It's on by default. This setting controls a feature; it isn't a security boundary. Any process running as the user can edit the settings file.
