# Agent handoff and agent management

Status: planned for later. Monodrive hands work to T3 Code and Monocode and shows its status. It doesn't run agents itself: native conversations, managed launch and orchestration were dropped on 2026-10-30, because those apps already do them. Ordinary UI/MCP file-operation parity remains part of every earlier milestone and does not wait for this track.

## Dependencies and order

Start after reliable shared jobs, visible operation history and stable source identity. Worktree-aware handoff uses the inventory milestone in [worktree management](22-worktree-management.md). Neither cloud backends nor extra browser views are prerequisites.

## Handoff first

Let a user select files or a folder, choose a configured T3 Code or Monocode integration, inspect the context and task being sent, and hand it off explicitly. Include source identity, paths and relevant worktree identity; a remote path is not usable by an agent on another machine without a deliberate transfer or remote session.

Use a supported integration discovered during implementation. Do not invent deep links, read private application databases or expose local files publicly. Sending paths need not send file contents; show when content must cross a machine or service boundary. Reuse existing authentication rather than copying provider credentials.

Core owns handoff requests/results; UI and MCP use the same contract. Return an actual session reference when supported, or report a failed/unsupported handoff. Repeated submission after a lost response must not start duplicate sessions. Provide an explicit action to open the companion session and a way to revoke/forget the integration.

## Agent activity in Monodrive

Show linked sessions and their known running, waiting, completed, failed or disconnected state alongside file operations. Attribute file changes only when the integration provides reliable evidence. Unknown or unreachable is not idle. Losing a connection or closing a pane does not imply a session stopped.

Conversations, model selection and execution stay in the companion app. Session lists and status come from supported APIs with events or bounded refresh, never whole-machine process scans.

## Verification

Use fake integrations and temporary repositories to test exact handoff context, unsupported paths, duplicate requests, lost connections and revoked access. Tests never launch real agents, contact providers, run setup hooks or modify live worktrees. UI checks cover review, activity, failure and return navigation.
