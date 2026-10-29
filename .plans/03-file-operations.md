# File operations, shared jobs and recovery

Depends on the selection foundation. Implement in slices: operation ownership and local mutations, then copy/move jobs and recovery, then editing commands. These slices belong to one service contract, not independent UI and MCP queues.

## Shared owner

The window and each stdio MCP server are separate processes. An in-memory journal per client cannot provide shared undo. Choose and document one local operation owner and authenticated local IPC before implementing write tools. Keep execution in `monodrive-core`; clients submit typed requests and observe the same jobs. Starting the operation owner must not implicitly start the search index.

Persist operation IDs, request identity, initiating client, source/destination paths, conflict decisions, per-item outcomes and recovery information. Retrying an uncertain submission uses its original ID and returns its existing outcome; a new intentional operation gets a new ID. Define owner startup, exclusive ownership, crash reconciliation and idle shutdown. Do not require a window to stay open for an acknowledged background job.

## Operations

- Trash, restore, rename, new folder/file, duplicate, copy, move, undo and redo share typed services. Empty Trash is a separate explicitly confirmed irreversible action.
- Use native Trash and record the returned location for restoration. Ordinary operations never substitute permanent deletion for unavailable Trash.
- A same-volume move uses rename where supported. A cross-volume move copies, verifies successful completion against the source, and only then trashes the source. If the source changed or cannot be trashed, leave it and report the partial outcome.
- Preserve metadata with native copy facilities where supported; use APFS clones when available without promising zero cost. Report meaningful metadata loss on destinations that cannot preserve it.
- Conflict choices are keep both, replace, skip and stop. Core never prompts; MCP defaults to stop. Replacement preserves the previous destination for recovery before committing the new one, or refuses if that cannot be done safely.
- Revalidate source identity, destination conflicts and permissions at execution. Detect copying a directory into itself. Errors retain the path and OS reason.

## Jobs and interrupted transfers

Expose queued, running, waiting for conflict/input, completed, failed, cancelled and interrupted states with per-item results and progress. Bound concurrent I/O and keep it off the UI thread. Cancellation preserves completed results and identifies partial artifacts; cleanup is scoped to artifacts owned by that job.

Persist intent before a mutation and reconcile uncertain outcomes after a crash. Never blindly replay a rename, replacement or source removal. Retry can skip verified completed items. Byte-offset resume is optional per backend and must validate source and partial destination; otherwise restart that file explicitly. Do not label every operation resumable.

An activity view shows UI and agent operations, their source machine, progress, failures, retry and cancel. MCP can submit, inspect and cancel the same jobs and retrieve resulting paths. Long jobs return a stable job ID, not a blocked tool call with no observable state.

## Undo, redo and previews

Record safe inverse operations and retained originals. Check that files still match the recorded operation before undo or redo. Refuse conflicting reversals rather than overwriting later user edits. Define a visible retention policy and explain when recovery data has expired; retained originals are not permanent backups.

Routine creation and rename need no extra approval step. Batch rename, folder merge, replacement and cleanup provide a concrete preview before execution. A preview is bound to its inputs; changed inputs or filesystem state require revalidation and an updated conflict result.

## UI and MCP

All supported mutations have both entry points. The Agents setting controls agent file mutations at the shared owner, not only in the UI, and defaults on as previously planned. Requests still name explicit paths and policies. Attribute operations to the initiating client without claiming to know the person behind an external agent.

Selection follows successful renames/moves where the destination remains visible. Failed items remain discoverable. Undo from the window can reverse eligible MCP operations even after that MCP client exits.

## Verification

Use isolated owner state and temporary folders. Cover operation/inverse behavior, replacement recovery, metadata preservation, changed-input refusal, cross-process visibility, duplicate submissions and interrupted-job reconciliation. Inject failures at operation boundaries instead of sleeping or killing unrelated processes. Test cross-volume behavior through an isolated supported fixture or report that it was not verified. Measure responsiveness and bounded resource use during large transfers separately from local search latency.
