# Agent handoff and agent management

Status: planned for later. Agent features are part of the product direction, not excluded from Monodrive. Ordinary UI/MCP file-operation parity remains part of every earlier milestone and does not wait for this track.

## Dependencies and order

Start after reliable shared jobs, visible operation history and stable source identity. Worktree-aware handoff uses the inventory milestone in [worktree management](22-worktree-management.md). Managed creation/setup waits for that plan's reviewed lifecycle. Neither cloud backends nor extra browser views are prerequisites.

## Handoff first

Let a user select files or a folder, choose a configured T3 Code or Monocode integration, inspect the context and task being sent, and hand it off explicitly. Include source identity, paths and relevant worktree identity; a remote path is not usable by an agent on another machine without a deliberate transfer or remote session.

Use a supported integration discovered during implementation. Do not invent deep links, read private application databases or expose local files publicly. Sending paths need not send file contents; show when content must cross a machine or service boundary. Reuse existing authentication rather than copying provider credentials.

Core owns handoff requests/results; UI and MCP use the same contract. Return an actual session reference when supported, or report a failed/unsupported handoff. Repeated submission after a lost response must not start duplicate sessions. Provide an explicit action to open the companion session and a way to revoke/forget the integration.

## Agent activity in Monodrive

Show linked sessions and their known running, waiting, completed, failed or disconnected state alongside file operations. Attribute file changes only when the integration provides reliable evidence. Unknown or unreachable is not idle. Losing a connection or closing a pane does not imply a session stopped.

Initially delegate conversations, model selection and execution to the companion app. This keeps the first feature useful without implementing a provider runtime. Session lists and status come from supported APIs with events or bounded refresh, never whole-machine process scans.

## Managed launch and worktree tasks later

After handoff and status are useful, design explicit launch, stop, cancellation and recovery for tasks on an assigned local worktree. Reuse the shared job owner and worktree creation/reservation model instead of a second pool or scheduler. Distinguish a Git lock, an agent reservation and an actually running process.

Only stop processes or sessions Monodrive owns or has explicit authority to control. Preserve work from external agents and user-created worktrees. Review cleanup and ownership together with the worktree plan. Remote execution requires the remote host lifecycle and permission design; SSH browsing alone does not provide it.

Native conversations, provider/model selection, scheduling and broader orchestration remain possible subsequent milestones. Define their concrete workflow, credential ownership, persistence, resource limits and differences from companion integrations before expanding implementation scope. They are deferred, not permanently ruled out.

## Verification

Use fake integrations and temporary repositories to test exact handoff context, unsupported paths, duplicate requests, lost connections, revoked access and cancellation authority. Tests never launch real agents, contact providers, run setup hooks or modify live worktrees. UI checks cover review, activity, failure and return navigation.
