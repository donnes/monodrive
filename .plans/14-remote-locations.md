# Remote locations over SSH/SFTP

Depends on shared jobs, source identity, places and split panes. The application stays macOS-only. The first version browses existing SSH/SFTP servers without installing Monodrive remotely.

## Connection model

Accept an explicit host or existing SSH alias. Reuse the system's SSH config, agent and known-host verification; do not copy private keys into preferences or bypass changed-host-key errors. Keep connection setup and credential prompts in a defined native flow. MCP can refer to saved source IDs and report when interactive authentication is required.

Borrow T3 Code's distinction between a machine and its connection routes. Initially one SSH route is sufficient; avoid speculative relay or pairing infrastructure. Add another route to an existing identity only after verification, not because two machines share a name.

Save source identity and path together. A remote path is never interpreted as a local path. Capabilities describe listing, reads, writes, rename, metadata and recovery support. Keep local filesystem behavior simple while introducing this boundary where the second backend needs it.

## Browsing and transfers

List, inspect and read remote files, create folders, and copy between local/remote panes through core services. Offer move/rename only with explicit supported semantics. A cross-source move cannot remove the source unless its recovery policy is satisfied; when remote Trash is unavailable, offer Copy instead. Do not relabel SFTP unlink as Trash.

Connections can be disconnected or forgotten without changing remote files. Disconnection interrupts local-client-driven transfers; it does not imply jobs keep running on the server. Reconnect reconciles job state before retry. Resume support is negotiated and validated rather than promised for every transfer.

Preview or Open With downloads selected content into a bounded task-owned cache with visible progress and cleanup. Read-only preview is the initial scope; automatic remote editor write-back is deferred. Dragging into a local app must materialize a real local file first.

## Search and later helper

Start with bounded directory browsing and explicitly scoped search supported by the connection. No recursive scan merely to open a location. An optional host-side index and durable remote executor are separate later decisions, including Linux support, deployment, authentication and ownership. They are not required to ship SFTP browsing.

## Verification

Use a disposable local SSH/SFTP fixture and isolated credentials/known-hosts state. Cover source/path isolation, authentication failure, changed host keys, disconnect/retry, partial transfers and capability-driven disabled actions. Never connect to a personal machine as a test by default. Measure first listing, transfer throughput, cancellation and memory separately from local index speed.
